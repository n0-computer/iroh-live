# Publishing

`moq_media::publish::LocalBroadcast` wraps a `moq_net::broadcast::Producer` and
owns the catalog that describes it. Video goes through `LocalBroadcast::video()`
and audio through `LocalBroadcast::audio()`, both of which hand back a borrowed
publisher handle. In iroh-live the producer comes from `Live::publish(path)`,
which creates the broadcast on the node's origin.

Encoding itself is upstream. `moq_video::encode` and `moq_audio::encode` own the
codec, the thread it runs on, and the catalog entry it writes. What this crate
adds is the layer above: one camera feeding a simulcast ladder, a pre-encoded
byte stream published without re-encoding, and the iroh-live catalog extension.

## Sources

`VideoSource` names the three ways pictures reach a broadcast.

`Capture(moq_video::capture::Config)` opens a camera, a display, a window, or a
macOS application through `moq_video::capture::open`. It needs the `capture`
feature.

`Frames(BoxStream<moq_video::Frame>)` takes frames the application produced. The
Android demo uses it to hand over Camera2 buffers, and `moq_media::test_source`
uses it for a generated pattern. The first frame determines the geometry the
catalog advertises, so the publish task pulls it, reads its size and color, and
puts it back at the head of the stream before any encoder opens.

`AnnexB(BoxStream<bytes::Bytes>)` takes an H.264 byte stream the source already
encoded. This is the Raspberry Pi path, where `rpicam-vid` encodes in hardware
and no raw picture ever reaches us.

`AudioSource` is the same shape with two variants: `Device` for a microphone or
the macOS system mix, and `Frames { input, frames }` for PCM the application
produced, such as a decoded file.

## Simulcast

`VideoPublisher::set(source)` publishes one rendition named `video` at the
source's own resolution. `VideoPublisher::set_renditions(source, renditions)`
publishes a ladder, where each `VideoRendition` carries a name, an optional
`Size`, an optional bitrate, a `moq_video::encode::Codec`, and a
`moq_video::encode::Kind` naming the backend to prefer.

The ladder is the reason this code exists. Upstream, one
`moq_video::encode::Producer` publishes one rendition and owns the device it
captures from, so a second rendition would need a second camera. Here the source
is opened once and every frame is wrapped in an `Arc` and sent to each
rendition's encoder through a latest-wins slot. One allocation per frame is
shared by the preview and every rung, which is what
`moq_video::encode::Sink::encode` taking an `Arc<Frame>` is for. A rendition that
falls behind drops frames rather than stalling the ones that have not.

Before an encoder opens, `encode::Config::probe()` runs once per rendition. That
costs one encoder open and buys a catalog entry describing exactly what the track
will carry, so a subscriber can pick a rendition before a single frame has been
encoded.

Audio has no ladder. A subscriber under pressure drops video renditions and never
audio, so `AudioPublisher::set_with` publishes exactly one track.

## Demand gating

Each rendition's encoder idles on `producer.demand().used()` and opens a
`moq_video::encode::Sink` only once someone subscribes to that rendition. When
the last viewer leaves, the encoder closes and the producer records a
discontinuity so the next timestamp does not stretch a frame across the gap. The
track and its catalog entry stay advertised throughout.

The source is deliberately not gated, which is where we diverge from upstream.
`moq_video::encode::publish_capture` releases the camera when nobody is watching.
We cannot do that, because the publisher's own preview draws the frames on their
way to the encoders and a publisher expects to see itself before anyone tunes in.

## Preview

`LocalBroadcast::preview()` returns a receiver for the raw frames the source
produced, before encoding. It costs no extra decode: these are the same frames
the encoders receive. It returns `None` when no video is publishing and when the
source is `AnnexB`, since a pre-encoded stream has no raw picture to tap.

## Pre-encoded video

The `AnnexB` path never opens an encoder. `moq_mux::codec::h264::Split` cuts the
byte stream into access units and `moq_mux::codec::h264::Import` publishes them,
filling in the catalog rendition from the first SPS it sees. The stream describes
itself, so nothing here has to state a profile and level it did not choose. The
splitter holds the final access unit until the next start code, so end of stream
flushes it explicitly.

`moq_media::rpicam` produces such a stream by running `rpicam-vid` and reading
Annex-B off its stdout. See [Raspberry Pi](../guide/raspberry-pi.md).

## Catalog

The catalog is `moq_mux::catalog::Producer<IrohLiveExt>`, which is hang's catalog
with an extension flattened alongside the `video` and `audio` sections. The
extension carries two things iroh-live publishes and hang has no place for:
`enable_chat` creates a track and advertises it under `chat`, and `set_user`
advertises the publisher's identity under `user`. A base hang consumer ignores
both, so the broadcast stays wire-compatible with any hang player.

## Clock

`LocalBroadcast::clock()` is the `moq_mux::Clock` both media tracks are stamped
from. Audio and video share it so their timelines stay aligned even though the
two devices open at different times.
