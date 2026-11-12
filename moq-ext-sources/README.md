# moq-ext-sources

Media sources for [moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio) that upstream moq
lacks. A source is an opened device, file or generator that produces raw
frames on a thread of its own, for a publisher to encode. No iroh or transport
dependency.

Use it to feed [`moq-ext-publisher`](../moq-ext-publisher), or any encoder you
run yourself. A `VideoSource` feeds several encoders at once and offers a
latest-wins preview. An `AudioSource` fans PCM out to every broadcast that
reads it.

```rust
use moq_ext_sources::{AudioSource, VideoSource};

// `size` and `rate` are moq-video's `Size` and `FrameRate`, `layout` is
// moq-audio's `Layout`.
let video = VideoSource::test_pattern(size, rate)?;
let audio = AudioSource::test_pattern(layout)?;
```

`VideoSource::push` and `AudioSource::push` take frames the application
makes. `VideoSource::switchable` returns a `VideoSwitch` and the source it
feeds, and the switch swaps or opens what feeds the source while a broadcast
encodes it. `AudioSource::file(path, FileEnd)` plays a WAV or MP3 file and
stops or restarts at its end. `rpicam_h264` returns the Annex-B H.264 that
`rpicam-vid` encoded, for `LocalBroadcast::set_encoded_video`. The crate docs
list every constructor.

`VideoSource::capture` waits up to three seconds for a camera dropped before
it to release the device, so a switch back to the same camera does not fail as
busy. With the `pipewire` feature on Linux, the default camera is PipeWire's,
which other applications can share, and V4L2 is the fallback. The default
display is the screen the user picks in the desktop portal's dialog, with the
main X11 display as the fallback on an X11 session only. A microphone
opens when a broadcast starts to publish it. If it fails, it is not opened
again.

Errors are `moq_ext_sources::Error`, the type the player and the publisher
return too.

## Features

| Feature | Default | Effect |
|---|---|---|
| `pattern` | yes | test pattern and tone generators |
| `file` | no | WAV and MP3 file source |
| `capture` | no | camera, screen and microphone |
| `sound-server` | no | PipeWire and PulseAudio devices (Linux) |
| `aec` | no | echo cancellation for the microphone |
| `pipewire` | no | the desktop portal's screen capture and PipeWire's camera as the Linux defaults |
| `rpicam` | no | the Raspberry Pi camera through `rpicam-vid` (Linux) |
