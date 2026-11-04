# moq-ext-sources

Media sources for [moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio) that upstream moq
lacks. A source is an opened device, file or generator that produces raw
frames on a thread of its own, for a publisher to encode. No iroh or transport
dependency.

Use it to feed [`moq-ext-publisher`](../moq-ext-publisher), or any encoder you
run yourself. A `VideoSource` feeds several encoders at once and offers a
latest-wins preview; an `AudioSource` fans PCM out to every broadcast reading
it.

```rust
use moq_ext_sources::{AudioSource, VideoSource};

// `size` and `rate` are moq-video's `Size` and `FrameRate`, `layout` is
// moq-audio's `Layout`.
let video = VideoSource::test_pattern(size, rate);
let audio = AudioSource::test_pattern(layout);
```

Also available: `VideoSource::push` and `AudioSource::push` for frames the
application makes, and `EncodedVideoSource::annex_b` for H.264 that needs no
encoder. The crate docs list every constructor.

## Features

| Feature | Default | Effect |
|---|---|---|
| `pattern` | yes | test pattern and tone generators |
| `file` | no | WAV and MP3 file source |
| `capture` | no | camera, screen and microphone |
| `sound-server` | no | PipeWire and PulseAudio devices (Linux) |
| `aec` | no | echo cancellation for the microphone |
| `pipewire` | no | Linux screen capture through the desktop portal |
| `rpicam` | no | the Raspberry Pi camera through `rpicam-vid` (Linux) |
