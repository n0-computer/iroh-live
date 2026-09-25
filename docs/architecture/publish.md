# Publishing

`iroh_live_media::LocalBroadcast` wraps a `moq_net::broadcast::Producer` and
owns the catalog that describes it, a media clock, one video slot, and one audio
slot. `set_video`, `set_encoded_video`, and `set_audio` fill the slots, each
replacing whatever the slot held, and `clear_video` and `clear_audio` empty
them. `LocalBroadcast::new()` creates a broadcast published nowhere yet, which a
transport reads through `moq_net::Consume`. In iroh-live, `Live::publish(name, &broadcast)`
takes the broadcast by reference and publishes what it reads through
`Consume` at `live/<this node's id>/<name>`, and `Live::ticket(name)` names that
path for a subscriber.

Encoding itself is upstream. `moq_video::encode` and `moq_audio::encode` own the
codec, the thread it runs on, and the catalog entry it writes. What this crate
adds is the layer above: one camera feeding a simulcast ladder, a pre-encoded
byte stream published without re-encoding, and the iroh-live catalog extension.

## Sources

A source is a value that is already open. Opening a device is async and fails
if the device will not open, so a missing camera is an error where the
application asked for it rather than a log line from a task that retries. Each
source runs on a thread of its own for its whole life, which is what non-`Send`
platform capture objects require, and only frames cross. A source runs while any
clone exists, including the clone a broadcast holds, and any number of
broadcasts and previews read it at once.

`VideoSource` has these constructors:

- `capture(moq_video::capture::Config)` opens a camera, a display, or a window
  and returns once it produced a frame. It needs the `capture` feature. A device
  that opens and produces nothing within thirty seconds fails.
- `test_pattern(size, rate)` draws a sweeping bar, a frame counter, a clock, and
  a marker that flashes in step with `AudioSource::test_pattern`'s beep.
- `push(format)` returns a `FrameSender` for frames the application makes. The
  Android demo uses it to hand over Camera2 buffers.
- `spawn(name, format, run)` runs `run` on a dedicated thread with a
  current-thread runtime entered and hands it the sender, for thread-bound
  platform objects.
- `rpicam(config)` reads raw pictures from `rpicam-vid` on a Raspberry Pi.

`FrameSender::push` never blocks: a video frame replaces a pending one. Its
`demand()` watcher reports whether any rendition is encoding, so an application
camera can idle while nobody watches, and `push` returns `Closed` once the
source is gone so a producing loop knows to stop.

`EncodedVideoSource` takes an H.264 byte stream the source already encoded,
through `annex_b(stream)` or `rpicam(config)`. This is the Raspberry Pi path,
where `rpicam-vid` encodes in hardware and no raw picture ever reaches us.

`AudioSource` has `microphone(MicrophoneConfig)`, `file(path, looping)` for a
WAV or MP3 decoded in real time, `tone(hz, layout)`, `test_pattern(layout)`,
and `push(format)` for PCM the application produces. `MicrophoneConfig` names
the device and, through `with_echo_cancellation(&output)`, the `AudioOutput`
whose signal is subtracted from the microphone. A PCM source fans out to every
broadcast that reads it through a bounded buffer of about a hundred frames, and
a broadcast that falls further behind loses the oldest and counts them in its
stats.

## Simulcast

`set_video(source, encoding)` takes a `VideoEncoding`. `VideoEncoding::single`
publishes one rendition and `VideoEncoding::ladder` several. Each
`VideoRendition` carries a name, which is its track name, an optional `Size`,
an optional `Bitrate`, an optional frame rate at or below the source's, a
keyframe interval (two seconds by default), a `moq_video::encode::Codec`, and a
`moq_video::encode::Kind` naming the backend. `VideoRendition::new(name)`
encodes at the source's own size; `p180`, `p360`, `p720`, and `p1080` are 16:9
presets at 150 kbit/s, 500 kbit/s, 1.8 Mbit/s, and 4 Mbit/s. `set_video` fails
at once for an empty ladder, duplicate names, a rate above the source's, or a
codec no compiled-in encoder supports.

The ladder is the reason this code exists. Upstream, one
`moq_video::encode::Producer` publishes one rendition and owns the device it
captures from, so a second rendition would need a second camera. Here the source
is opened once, and each rendition's encoder reads its frames through a
latest-wins handle of its own. The frames are `Arc`s, so one allocation per
frame is shared by the preview and every rung, which is what
`moq_video::encode::Sink::encode` taking an `Arc<Frame>` is for. A rendition that
falls behind drops frames rather than stalling the ones that have not. Every
rendition's timestamps are mapped onto the broadcast clock from one anchor, so
two rungs carry the same timestamp for the same picture and a subscriber
switching between them sees no jump.

Before an encoder opens, `encode::Config::probe()` runs once per rendition. That
costs one encoder open and buys a catalog entry describing exactly what the track
will carry, so a subscriber can pick a rendition before a single frame has been
encoded. With `prefer_hardware` (the default) and no backend named, a hardware
encoder that will not open falls back to software once; a backend named
explicitly is not replaced, so a broken driver shows up as a rendition that
failed.

Audio has no ladder. A subscriber under pressure drops video renditions and never
audio, so `set_audio(source, encoding)` publishes exactly one track, named after
its codec. `AudioEncoding::voice()` is Opus mono at 32 kbit/s,
`AudioEncoding::music()` Opus stereo at 128 kbit/s, and `AudioEncoding::pcm()`
uncompressed at the source's own rate and layout.

## Demand gating

Each rendition's encoder idles on `producer.demand().used()` and opens a
`moq_video::encode::Sink` only once someone subscribes to that rendition. While
it encodes, it counts as demand on the source, which is what
`FrameSender::demand()` reports. When the last viewer leaves, the encoder
closes and the producer records a discontinuity so the next timestamp does not
stretch a frame across the gap. The track and its catalog entry stay advertised
throughout.

`LocalBroadcast::status()` is a watcher over `PublishStatus`: the state of each
slot (`Off`, `Starting`, `Running`, `Failed`, `Ended`), and per rendition
whether it is idle, encoding and with which backend, or failed. `stats()`
returns a `PublishStats` snapshot with the source's frame rate and each
rendition's encoder on its own, so a ladder reports every rung rather than one
blended figure.

The source is deliberately not gated, which is where we diverge from upstream.
`moq_video::encode::publish_capture` releases the camera when nobody is watching.
We cannot do that, because the publisher's own preview reads the same frames
and a publisher expects to see itself before anyone tunes in. A microphone is
the exception: moq-audio opens it only inside the publication that encodes it,
so it opens when the broadcast first has a listener, and a failure then shows in
`status()` rather than from `AudioSource::microphone`.

## Preview

`VideoSource::frames()` returns a `VideoFrames` handle onto the raw frames the
source produced, before encoding. It costs no extra decode, since these are the
same frames the encoders receive, and reading it does not count as demand. The
preview belongs to the source rather than to the broadcast, so it works before
any broadcast exists, and a `set_video` that swaps the source means pointing the
preview at the new source's frames, which is what `irl publish` does. An
`EncodedVideoSource` has no preview, since a pre-encoded stream has no raw
picture to tap.

## Pre-encoded video

`set_encoded_video` never opens an encoder. `moq_mux::codec::h264::Split` cuts the
byte stream into access units and `moq_mux::codec::h264::Import` publishes them
as the one rendition `video`, filling in the catalog rendition from the first
SPS it sees. The stream describes itself, so nothing here has to state a
profile and level it did not choose. The splitter holds the final access unit
until the next start code, so end of stream flushes it explicitly. A stream that
ends before its first access unit shows as a failed video slot.

`EncodedVideoSource::rpicam` produces such a stream by running `rpicam-vid` and
reading Annex-B off its stdout. See [Raspberry Pi](../guide/raspberry-pi.md).

## Catalog

The catalog is hang's, written by `moq_mux::catalog::Producer` with no
extension, so any hang player reads it. A room carries display names in its own
announcements and publishes chat as a broadcast of its own (see
[rooms](../guide/rooms.md)).

## Clock

A `LocalBroadcast` holds one `moq_mux::Clock`, which both slots stamp their media
from, and the catalog advertises its wall mapping. Audio and video share it so
their timelines stay aligned even though the two devices open at different
times; each source's own timestamps are rebased onto it at the first frame the
broadcast reads.

`close()` ends the broadcast for every clone: it stops the encoders, lets each
finish its track, then ends the catalog and the broadcast, which withdraws it
from everywhere it was published. `closed()` waits for that to finish. Dropping
the last clone without `close` also ends the broadcast, without the clean
finish.
