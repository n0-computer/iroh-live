# Publishing

`moq_ext_publisher::LocalBroadcast` wraps a `moq_net::broadcast::Producer`. It
owns the catalog, a media clock, one video slot and one audio slot.
`set_video`, `set_encoded_video` and `set_audio` fill a slot and replace what it
held. `clear_video` and `clear_audio` empty it. A transport creates the
broadcast it publishes, and `LocalBroadcast::new_in(producer)` (or
`TryFrom<broadcast::Producer>`) fills that one, so readers get it without a copy.
In iroh-live, `let broadcast: LocalBroadcast = layout.publish(&node, name)?` publishes
one at `live/<this node's id>/<name>`. `LocalBroadcast::new()` creates one of
its own, published nowhere, for a local preview or a test. Closing a
`LocalBroadcast`, or dropping its last clone, closes the broadcast, which
withdraws the publication.

Encoding is upstream. `moq_video::encode` and `moq_audio::encode` own the
codec, its thread and the catalog entry it writes. This crate adds a simulcast
ladder over one source, and pre-encoded streams published without re-encoding.
The code is in `moq-ext-publisher/src/publish/`.

## Sources

A source is already open when you get it. Opening a device is async and fails
if the device does not open, so a missing camera is an error at the call that
asked for it. Each source runs on a thread of its own, which non-`Send`
platform capture objects require. A source runs while any clone exists,
including the clone a broadcast holds, and any number of broadcasts and
previews can read it.

`VideoSource` has these constructors:

- `capture(moq_video::capture::Config)` opens a camera, a display or a window,
  and returns once it produced a frame. It needs the `capture` feature. A
  camera that produces no frame within 10 seconds fails, and a display or a
  window within 30 seconds.
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
application camera can idle while nobody watches. `push` returns `SourceClosed`
once the source is gone, and the producing loop should stop then.

`switchable(format)` returns a source and a `VideoSwitch` that swaps what
feeds it, for a camera menu. The broadcast encodes the switchable source once,
and a forwarding task per input copies the current input's frames into its
slot, keeping each frame's `sent_at`, and passes the broadcast's demand back
to the input. `replace(source)` swaps at once, `open(future)` opens the next
source in the background and drops one still opening, and `clear()` stops
feeding. The previous source is dropped first, so the same camera opens again.
`status()` watches a `SlotState`: `Starting` while an open runs, then
`Running`, `Failed` or `Ended`. Unlike a second `set_video`, a switch neither
probes encoders again nor needs the ladder at hand; the broadcast sees one
source whose timeline breaks, and starts a group there.

`set_encoded_video` takes a stream of Annex-B H.264 bytes that is already
encoded, such as the one `rpicam_h264(config)` returns. On a Raspberry Pi,
`rpicam-vid` encodes in hardware and no raw picture reaches us.

`AudioSource` has `microphone(config)` for moq-audio's `capture::Config`,
`file(path, at_end)` for a WAV or MP3 decoded in real time,
`tone(hz, layout)`, `test_pattern(layout)`, and `push(format)` for PCM the
application produces. For echo cancellation, the config's `aec` holds the
canceller of the speaker's `playback::Engine`, from `Engine::canceller`
(needs the `aec` feature). The microphone attaches to it when its publication
opens the device, after a publication it replaces has let go. A PCM source
fans out to every broadcast through a buffer of 100 frames. A broadcast that
falls further behind loses the oldest frames and counts them in its stats.

## Simulcast

`set_video(source, encoding)` takes a `VideoEncoding`. `VideoEncoding::single`
publishes one rendition and `VideoEncoding::ladder` several. A
`VideoRendition`'s `name` is its track name. It also has an optional `size`
and `rate`, and moq-video's own `encode::Config` for the rest: bitrate,
keyframe interval (`join`, two seconds by default), codec, backend (`kind`)
and preset. The config's size, rate and color are filled in when the rendition
opens. `VideoRendition::new(name)` encodes at the source's size.
`p360` and `p720` are 16:9 presets at 500 kbit/s and 1.8 Mbit/s.

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

The rebase means a subscriber only ever sees broadcast timestamps. A
round-trip measurement, such as the latency from capture to a result a
subscriber reports back, echoes one of those timestamps to the publisher, which
then has to name its own frame. `VideoEncoding::record_timestamps` turns on a
record for that, off by default. `video_source_timestamp` returns the source
timestamp of an echoed broadcast timestamp, and `video_captured_at` the instant
the frame entered the source, which `VideoFrames::sent_at` reports: when the
application pushed it, or when a capture thread handed it over.

The record keeps timestamps and nothing else, no pixels and no encoded data:
the broadcast timestamp, the source timestamp and the instant, 48 bytes for
each of the last 512 published pictures, at most 24 KiB per broadcast and
about 17 s at 30 fps. It is per frame rather than an offset per timeline,
because the instant is not a function of the source timestamp: a camera stamps
frames with its device clock, and an application stamps pushed frames however
it likes. Without the option nothing is recorded and both methods return
`None`.

Before a rendition is advertised, `encode::Config::probe()` runs once for it,
so the catalog entry describes exactly what the track will carry. The whole
ladder is probed before any of it reaches the catalog. With the encoder `Auto`
or `Hardware`, a hardware encoder that fails to open or fails mid-stream falls
back to software once. `Kind::Software` asks for software from the start. A
backend named explicitly is never replaced, so a broken driver shows
as a failed rendition.

Audio has no ladder. `set_audio(source, encoding)` publishes one track, named
after its codec. The encoding is moq-audio's `encode::Config`, and the
`audio_encoding` module has three: `voice()` is Opus mono at 32 kbit/s,
`music()` Opus stereo at 128 kbit/s, and `pcm(format)` uncompressed PCM in the
given format, usually the source's (`AudioSource::format`).

## Demand

Each rendition's encoder waits on `producer.demand().used()` and opens a
`moq_video::encode::Sink` only once someone subscribes to that rendition. While
it encodes, it counts as demand on the source, which `FrameSender::demand()`
reports. When the last viewer leaves, the encoder closes and the producer
records a discontinuity, so the next timestamp does not stretch a frame across
the gap. The track and its catalog entry stay advertised throughout.

The source itself keeps running while nobody watches, because the publisher's
preview reads the same frames. A microphone is not a shared source. The audio
slot opens it on a thread of its own as soon as `set_audio` runs, as one
`moq_audio::capture::Producer` that feeds the encoder. A failure, at open or
later, shows in `status()` as a `Failed` audio slot, and the slot stays failed
until the next `set_audio`.

`LocalBroadcast::status()` watches a `PublishStatus`: each slot's `SlotState`
(`Off`, `Starting`, `Running`, `Failed`, `Ended`), and per rendition a
`RenditionState` (`Idle`, `Encoding { encoder }`, `Failed`). `stats()` returns
a `PublishStats` snapshot with one entry per rendition.

## Preview

`VideoSource::frames()` returns a `VideoFrames` handle onto the source's raw
frames. These are the frames the encoders receive, so a preview costs no
decode, and reading it does not count as demand. The preview belongs to the
source, so it works before any broadcast exists. Pre-encoded video has no
preview.

## Pre-encoded video

`set_encoded_video` never opens an encoder. `moq_mux::codec::h264::Split` cuts
the byte stream into access units, and `moq_mux::codec::h264::Import` publishes
them as the one rendition `video`. `Import` fills in the catalog entry from the
first SPS. The splitter holds the last access unit until the next start code,
so the end of the stream flushes it explicitly. A stream that ends before its
first access unit shows as a failed video slot.

See [Raspberry Pi](../guide/raspberry-pi.md) for `rpicam_h264`.

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
