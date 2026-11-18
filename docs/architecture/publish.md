# Publishing

A `LocalBroadcast`, from `moq-ext-publisher`, wraps a moq-net broadcast
producer. It owns the catalog, a media clock, one video slot and one audio
slot. `set_video`, `set_encoded_video` and `set_audio` fill a slot and
replace what it held. `clear_video` and `clear_audio` empty it.

A transport creates the broadcast it publishes, and
`LocalBroadcast::new_in(producer)` fills that one, so readers get it without
a copy. In iroh-live, the layout publishes one at
`live/<endpoint id>/<name>`:

```rust,ignore
let broadcast: LocalBroadcast = layout.publish(&node, name)?;
```

`LocalBroadcast::new()` creates one of its own, published nowhere, for a
local preview or a test. Closing a `LocalBroadcast`, or dropping its last
clone, closes the broadcast, which withdraws the publication.

Encoding is upstream. `moq_video::encode` and `moq_audio::encode` own the
codec, its thread and the catalog entry it writes. This crate adds a ladder
of renditions over one source, and publishes pre-encoded streams without
re-encoding them.

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
  window within 30 seconds. A camera open first waits up to three seconds
  for the cameras dropped before it to close their devices, so a switch back
  to the same camera does not fail as busy (see below).
- `camera(id)` and `screen()` are `capture` with a camera or the main display
  at its default mode. `screen_source()` returns the source `screen()` opens:
  the desktop portal on Wayland with the `pipewire` feature, the main display
  elsewhere.
- `test_pattern(size, rate)` draws a sweeping bar, a frame counter, a clock,
  and a marker that flashes in step with `AudioSource::test_pattern`'s beep.
- `push(format)` returns a `FrameSender` and the source it feeds, for frames
  the application makes. The Android demo hands over camera buffers this way.
- `spawn(name, format, run)` runs `run` on a dedicated thread with a
  current-thread runtime entered, and hands it the sender. It is for
  thread-bound platform objects.
- `rpicam(config)` reads raw pictures from `rpicam-vid` on a Raspberry Pi.

The test pattern changes every frame on purpose. A static image compresses
to almost nothing after the first keyframe, so a test that waits for bytes
would pass on a stalled pipeline. With `AudioSource::test_pattern`, it shows
smoothness, dropped frames, latency and A/V sync without a camera or
microphone.

`FrameSender::push` never blocks: a new video frame replaces one not yet taken.
`FrameSender::demand()` reports whether anything wants the frames: a rendition
that encodes, or a `VideoFrames` handle from `VideoSource::frames()`. An
application camera can idle while nothing does. `push` returns `SourceClosed`
once the source is gone, and the producing loop should stop then.

`switchable(format)` returns a `VideoSwitch` and the source it feeds, for a
camera menu. The broadcast encodes the switchable source once. A forwarding
task per input copies the current input's frames into the source's slot,
keeps each frame's `sent_at`, and passes the switchable source's demand back
to the input. The switch has three operations:

- `replace(source)` swaps at once.
- `open(future)` opens the next source in the background, and drops one still
  opening.
- `open_spec(spec, options)` opens a source specifier from `spec` that way,
  and clears the switch for `none`.
- `clear()` stops feeding.

The previous source is dropped first, so the same camera opens again.
`replace` and `open` also drop the previous source's last frame from the
slot, so no reader gets a frame of the old source after the switch.

`status()` watches a `SlotState`: `Starting` while an open runs, then
`Running`, `Failed` or `Ended`. Unlike a second `set_video`, a switch neither
probes encoders again nor needs the ladder at hand. The broadcast sees one
source whose timeline breaks, and starts a group there.

moq-video's capture stream closes its device when it drops. A camera source
drops that stream on its own capture thread, after the last handle is gone. So
the device can still be held when the application opens the next camera. A
dropped camera source therefore leaves a *release token* in a small
process-wide list, and `capture` waits on those tokens before it opens a
camera. Some captures get stuck in a read, such as a Media Foundation camera
that delivers neither a frame nor an error. The wait gives up on such a
capture after three seconds. `VideoSource::released` returns the same token,
for a device that something else opens next.

`set_encoded_video` takes a stream of Annex-B H.264 bytes that is already
encoded, such as the one `rpicam_h264(config)` returns. On a Raspberry Pi,
`rpicam-vid` encodes in hardware and no raw picture reaches iroh-live.

`AudioSource` has these constructors:

- `microphone(config)` opens a microphone from moq-audio's `capture::Config`.
- `file(path, at_end)` decodes a WAV or MP3 with symphonia on its own thread,
  in real time. The `FileEnd` says whether it stops or restarts at the end of
  the file. moq-audio has no container reader.
- `tone(hz, layout)` and `test_pattern(layout)` generate sound.
- `push(format)` returns a `FrameSender` for PCM the application produces.

For echo cancellation, the config's `aec` holds the canceller of the speaker's
`playback::Engine`, from `Engine::canceller` (needs the `aec` feature). The
microphone attaches to it when its publication opens the device, after a
publication it replaces has let go. A PCM source fans out to every broadcast
through a buffer of 100 frames. A broadcast that falls further behind loses
the oldest frames and counts them in its stats.

## Simulcast

`set_video(source, encoding)` takes a `VideoEncoding`. `VideoEncoding::single`
publishes one rendition and `VideoEncoding::ladder` several. A
`VideoRendition` has these parts:

- `name`, its track name;
- an optional `size` and `rate`;
- moq-video's own `encode::Config` for the rest: bitrate, keyframe interval
  (`join`, two seconds by default), codec, backend (`kind`) and preset. The
  config's size, rate and color are filled in when the rendition opens.

`VideoRendition::new(name)` encodes at the source's size. `p360` and `p720`
are 16:9 presets at 500 kbit/s and 1.8 Mbit/s. Both are close to the 0.07
bit per pixel that moq-video derives without a bitrate (484 kbit/s and
1.94 Mbit/s at 30 fps).

`set_video` fails at once for a ladder it cannot encode: an empty ladder, an
empty or duplicate name, an odd or zero size, a rate above the source's, a
zero keyframe interval, or H.265 in software. openh264, the one encoder every
build has, encodes only H.264.

The source opens once, and each rendition's encoder reads its frames through
a latest-wins handle of its own. Frames are `Arc`s, so the preview and every
rendition share one allocation. A rendition that falls behind drops frames
without stalling the others. A frame of another size than its rendition, such
as a phone camera turned to portrait, is scaled on that rendition's encoder
thread by moq-video's `encode::Config::resize`.

The first frame any rendition encodes anchors the mapping from source
timestamps to the broadcast clock. Every rendition carries the same timestamp
for the same picture, so a subscriber that switches sees no jump.

A subscriber only ever sees broadcast timestamps. A publisher that measures
a round trip, such as the latency from capture to a result that a subscriber
reports back, needs to map an echoed timestamp to its own frame.
`VideoEncoding::with_recorded_timestamps()` keeps a small record for that,
and its API docs describe the methods that read it.

Before a rendition is advertised, `encode::Config::probe()` runs once for it,
so the catalog entry describes exactly what the track will carry. The whole
ladder is probed before any of it reaches the catalog. With the encoder `Auto`
or `Hardware`, a hardware encoder that fails to open or fails mid-stream falls
back to software once. `Kind::Software` asks for software from the start. A
backend named explicitly is never replaced, so a broken driver shows
as a failed rendition.

Audio has no ladder. `set_audio(source, encoding)` publishes one track, named
after its codec. The encoding is moq-audio's `encode::Config`, and the
`audio_encoding` module has three:

- `voice()`: Opus mono at 32 kbit/s.
- `music()`: Opus stereo at 128 kbit/s.
- `pcm(format)`: uncompressed PCM in the given format, usually the
  source's (`AudioSource::format`).

## Demand

Each rendition's encoder waits on `producer.demand().used()` and opens a
`moq_video::encode::Sink` only once someone subscribes to that rendition. While
it encodes, it counts as demand on the source, which `FrameSender::demand()`
reports. When the last viewer leaves, the encoder closes and the producer
records a discontinuity, so the next timestamp does not stretch a frame across
the gap. The track and its catalog entry stay advertised throughout.

A subscriber that joins mid-group reads the newest group from its keyframe.
Each rendition's track sets `track::Info::refresh` to 2, as moq-video's
capture does. Once a group's deltas outweigh twice its keyframe, a new viewer
asks for a newer group (`Demand::wanted`). The encoder then makes a keyframe
on its next frame. So the joiner starts near a keyframe instead of
downloading the rest of a long group, and a later joiner benefits too.

moq-video measured the cost at 10 to 13% more bitrate for one join a second,
and about 1% for one every ten seconds. A player that switches renditions
joins too. Requests that arrive while the keyframe is still in the encoder
coalesce, and a fresh encoder's opening keyframe serves every earlier
request, as in moq-video's own capture.

moq-video advertises the refresh only when every backend it may open honors
a keyframe request. iroh-live advertises it unless the rendition names the
V4L2 encoder. An encoder that cannot make a keyframe on request answers with
its next scheduled keyframe, which is what a joiner gets without a refresh.

The catalog reservation carries `encode::Config::effective_join()`, so
moq-mux declares `media.join.interval` from the encoder's keyframe interval
rather than measuring it from the first group.

The encoders and the publisher's frame-rate meter read the source through
`VideoSource::observe()`, which does not count as demand, so an idle broadcast
does not keep an application camera running. A preview's handle does count, so
such a camera still feeds the publisher's own preview while nobody watches.
Capture devices and generators run regardless of demand.

A microphone is not a shared source. The audio slot opens it on a thread of
its own as soon as `set_audio` runs, as one `moq_audio::capture::Producer` that
feeds the encoder. A failure, at open or later, shows in `status()` as a
`Failed` audio slot, and the slot stays failed until the next `set_audio`.

`LocalBroadcast::status()` watches a `PublishStatus`: each slot's `SlotState`
(`Off`, `Starting`, `Running`, `Failed`, `Ended`), and per rendition a
`RenditionState` (`Idle`, `Encoding { encoder }`, `Failed`). `stats()` returns
a `PublishStats` snapshot with one entry per rendition.

## Preview

`VideoSource::frames()` returns a `VideoFrames` handle onto the source's raw
frames. These are the frames the encoders receive, so a preview costs no
decode. The handle counts as demand until it and its clones are dropped, so a
source that idles until wanted produces frames for it. The preview belongs to
the source, so it works before any broadcast exists. Pre-encoded video has no
preview.

## Pre-encoded video

`set_encoded_video` never opens an encoder. `moq_mux::codec::h264::Split` cuts
the byte stream into access units, and `moq_mux::codec::h264::Import` publishes
them as the one rendition `video`. `Import` fills in the catalog entry from the
first SPS. The splitter holds the last access unit until the next start code,
so the end of the stream flushes it explicitly. A stream that ends before its
first access unit shows as a failed video slot.

See [Publish from a Raspberry Pi](../how-to/raspberry-pi.md) for `rpicam_h264`.

## Catalog and clock

The catalog is hang's, written by `moq_mux::catalog::Producer` with no
extension, so any hang player reads it. Rooms carry display names in their own
announcements (see [Run a room](../how-to/rooms.md)).

A `LocalBroadcast` holds one `moq_mux::Clock`. Both slots stamp their media
from it, and the catalog advertises its wall-clock mapping. Each source's
timestamps are rebased onto it at the first frame, so audio and video share a
timeline even though the devices open at different times.

`close()` ends the broadcast for every clone. It stops the encoders, gives each
up to two seconds to finish its track, then ends the catalog and the broadcast,
which withdraws it everywhere it was published. `closed()` waits for that.
Dropping the last clone without `close` also ends the broadcast, without the
clean finish.

## Where in the code

- `moq-ext-publisher/src/publish/`: `LocalBroadcast`, the slots, the ladder
  and demand.
- `moq-ext-sources/src/`: `VideoSource`, `AudioSource`, the switch and the
  release tokens.
- `VideoEncoding::with_recorded_timestamps`, `video_source_timestamp`,
  `video_captured_at` and `video_encoded_at`: the timestamp record, 64 bytes
  for each of the last 512 pictures.
