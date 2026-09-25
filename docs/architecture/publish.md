# Publishing

`iroh_live_media::LocalBroadcast` wraps a `moq_net::broadcast::Producer`. It
owns the catalog, a media clock, one video slot and one audio slot.
`set_video`, `set_encoded_video` and `set_audio` fill a slot and replace what it
held. `clear_video` and `clear_audio` empty it. `LocalBroadcast::new()` creates
a broadcast that is published nowhere yet, and a transport reads it through
`moq_net::Consume`. In iroh-live, `Live::publish(name, &broadcast)` publishes
it at `live/<this node's id>/<name>`.

Encoding is upstream. `moq_video::encode` and `moq_audio::encode` own the
codec, its thread and the catalog entry it writes. This crate adds a simulcast
ladder over one source, and pre-encoded streams published without re-encoding.
The code is in `iroh-live-media/src/publish/`.

## Sources

A source is already open when you get it. Opening a device is async and fails
if the device does not open, so a missing camera is an error at the call that
asked for it. Each source runs on a thread of its own, which non-`Send`
platform capture objects require. A source runs while any clone exists,
including the clone a broadcast holds, and any number of broadcasts and
previews can read it.

`VideoSource` has these constructors:

- `capture(moq_video::capture::Config)` opens a camera, a display or a window,
  and returns once it produced a frame. It needs the `capture` feature. A device
  that produces no frame within 30 seconds fails.
- `test_pattern(size, rate)` draws a sweeping bar, a frame counter, a clock,
  and a marker that flashes in step with `AudioSource::test_pattern`'s beep.
- `push(format)` returns a `FrameSender` for frames the application makes. The
  Android demo hands over camera buffers this way.
- `spawn(name, format, run)` runs `run` on a dedicated thread with a
  current-thread runtime entered, and hands it the sender. It is for
  thread-bound platform objects.
- `rpicam(config)` reads raw pictures from `rpicam-vid` on a Raspberry Pi.

`FrameSender::push` never blocks: a new video frame replaces one not yet taken.
`FrameSender::demand()` reports whether any rendition is encoding, so an
application camera can idle while nobody watches. `push` returns `Closed` once
the source is gone, and the producing loop should stop then.

`EncodedVideoSource` takes an H.264 Annex-B stream that is already encoded,
through `annex_b(stream)` or `rpicam(config)`. On a Raspberry Pi,
`rpicam-vid` encodes in hardware and no raw picture reaches us.

`AudioSource` has `microphone(MicrophoneConfig)`, `file(path, looping)` for a
WAV or MP3 decoded in real time, `tone(hz, layout)`, `test_pattern(layout)`,
and `push(format)` for PCM the application produces. `MicrophoneConfig` holds
the capture config and an optional `echo_reference`, the `AudioOutput` whose
signal is removed from the microphone (needs the `aec` feature). A PCM source
fans out to every broadcast through a buffer of 100 frames. A broadcast that
falls further behind loses the oldest frames and counts them in its stats.

## Simulcast

`set_video(source, encoding)` takes a `VideoEncoding`. `VideoEncoding::single`
publishes one rendition and `VideoEncoding::ladder` several. A
`VideoRendition`'s `name` is its track name. It also has an optional `size`,
`bitrate` and `rate`, a `keyframe_interval` (two seconds by default), a
`codec` and an `encoder` backend. `VideoRendition::new(name)` encodes at the
source's size.
`p180`, `p360`, `p720` and `p1080` are 16:9 presets at 150 kbit/s,
500 kbit/s, 1.8 Mbit/s and 4 Mbit/s.

`set_video` fails at once for an empty ladder, an empty or duplicate name, an
odd or zero size, a rate above the source's, a zero keyframe interval, or H.265
asked for in software. openh264, the one encoder every build has, encodes only
H.264.

The source opens once, and each rendition's encoder reads its frames through a
latest-wins handle of its own. Frames are `Arc`s, so the preview and every rung
share one allocation. A rendition that falls behind drops frames without
stalling the others. The first frame any rendition encodes anchors the mapping
from source timestamps to the broadcast clock (`Rebase`). Every rung carries
the same timestamp for the same picture, so a subscriber that switches sees no
jump.

Before a rendition is advertised, `encode::Config::probe()` runs once for it,
so the catalog entry describes exactly what the track will carry. The whole
ladder is probed before any of it reaches the catalog. With the encoder `Auto`
or `Hardware`, a hardware encoder that fails to open or fails mid-stream falls
back to software once. `prefer_hardware: false` asks for software from the
start. A backend named explicitly is never replaced, so a broken driver shows
as a failed rendition.

Audio has no ladder. `set_audio(source, encoding)` publishes one track, named
after its codec. `AudioEncoding::voice()` is Opus mono at 32 kbit/s,
`AudioEncoding::music()` Opus stereo at 128 kbit/s, and `AudioEncoding::pcm()`
uncompressed PCM at the source's rate and layout.

## Demand

Each rendition's encoder waits on `producer.demand().used()` and opens a
`moq_video::encode::Sink` only once someone subscribes to that rendition. While
it encodes, it counts as demand on the source, which `FrameSender::demand()`
reports. When the last viewer leaves, the encoder closes and the producer
records a discontinuity, so the next timestamp does not stretch a frame across
the gap. The track and its catalog entry stay advertised throughout.

The source itself keeps running while nobody watches, because the publisher's
preview reads the same frames. A microphone is the exception: moq-audio opens
it inside the publication that encodes it. It opens when the broadcast first
has a listener, and a failure then shows in `status()`.

`LocalBroadcast::status()` watches a `PublishStatus`: each slot's `SlotState`
(`Off`, `Starting`, `Running`, `Failed`, `Ended`), and per rendition a
`RenditionState` (`Idle`, `Encoding { encoder }`, `Failed`). `stats()` returns
a `PublishStats` snapshot with one entry per rendition.

## Preview

`VideoSource::frames()` returns a `VideoFrames` handle onto the source's raw
frames. These are the frames the encoders receive, so a preview costs no
decode, and reading it does not count as demand. The preview belongs to the
source, so it works before any broadcast exists. An `EncodedVideoSource` has no
preview.

## Pre-encoded video

`set_encoded_video` never opens an encoder. `moq_mux::codec::h264::Split` cuts
the byte stream into access units, and `moq_mux::codec::h264::Import` publishes
them as the one rendition `video`. `Import` fills in the catalog entry from the
first SPS. The splitter holds the last access unit until the next start code,
so the end of the stream flushes it explicitly. A stream that ends before its
first access unit shows as a failed video slot.

See [Raspberry Pi](../guide/raspberry-pi.md) for `EncodedVideoSource::rpicam`.

## Catalog and clock

The catalog is hang's, written by `moq_mux::catalog::Producer` with no
extension, so any hang player reads it. Rooms carry display names in their own
announcements (see [rooms](../guide/rooms.md)).

A `LocalBroadcast` holds one `moq_mux::Clock`. Both slots stamp their media
from it, and the catalog advertises its wall-clock mapping. Each source's
timestamps are rebased onto it at the first frame, so audio and video share a
timeline even though the devices open at different times.

`close()` ends the broadcast for every clone. It stops the encoders, gives each
up to two seconds to finish its track, then ends the catalog and the broadcast,
which withdraws it everywhere it was published. `closed()` waits for that.
Dropping the last clone without `close` also ends the broadcast, without the
clean finish.
