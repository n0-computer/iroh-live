# The media stack

Capture, encoding, decoding and GPU rendering come from `moq-video` and
`moq-audio`, upstream in the [moq](https://github.com/moq-dev/moq) repository.
This repository has no codec, capture backend or renderer of its own. The
upstream documentation covers them:

- [moq-video](https://doc.moq.dev/lib/rs/crate/moq-video): capture, encode,
  decode and render, the backend selection order, and the device enumerators.
- [moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio): microphone capture,
  encode and decode, the playback engine, and echo cancellation.
- [hang](https://doc.moq.dev/lib/rs/crate/hang) and
  [moq-mux](https://doc.moq.dev/lib/rs/crate/moq-mux): the catalog and the
  container formats.

`iroh-live-media` re-exports them as `iroh_live_media::video` and
`iroh_live_media::audio`. A crate that uses these paths gets the exact build
our renderer links. This matters most for wgpu: `moq_video::render` returns a
`wgpu::Texture` from its own wgpu build, and a texture from another wgpu major
version is a different type.

The workspace patches every moq crate to `Frando/moq@iroh-live-5` in its
`[patch.crates-io]` block. See the comment there for why.

## Backends

Upstream picks the codec backend: platform hardware first, then openh264 as
the software fallback. `moq_video::encode::Kind` and `moq_video::decode::Kind`
let a caller ask for `Auto`, `Hardware`, `Software` or one backend by name,
such as `Named("vaapi")`. The CLI's `--encoder` and `--decoder` take `auto`,
`hardware` (`hw`), `software` (`sw`) or a name from moq-video's `encode::NAMES`
and `decode::NAMES`.

H.264 encodes and decodes everywhere, since openh264 is always compiled in.
H.265 needs a hardware encoder.

## What we add

`EncodedVideoSource::rpicam` runs `rpicam-vid` and publishes the Annex-B H.264
it already encoded. `VideoSource::rpicam` reads raw pictures from the same
program. See [Raspberry Pi](../guide/raspberry-pi.md).

`AudioSource::file` decodes a WAV or MP3 file with symphonia on its own thread,
in real time, and fans the PCM out to every broadcast that reads it. moq-audio
has no container reader.

`VideoSource::test_pattern` and `AudioSource::test_pattern` are a matching
pair: a sweeping bar, a frame counter, a clock, and a marker that flashes with
the tone's beep. Together they show smoothness, dropped frames, latency and A/V
sync without a camera or microphone. The picture changes every frame on
purpose. A static image compresses to almost nothing after the first keyframe,
so a test that waits for bytes would pass on a stalled pipeline.
`AudioSource::tone` is a steady sine.

`iroh-live-egui` draws the texture `moq_video::render` returns in an egui panel,
and has the debug overlay. `iroh-live-media-android` has an EGL renderer for
Android. `demos/pi-zero/src/gles.rs` is a GLES2
renderer for the Pi Zero, which has no Vulkan.

## Feature flags

`iroh-live-media` defines the device and graphics features, and its
[README](../../iroh-live-media/README.md#feature-flags) lists them.
`iroh-live` forwards them and defaults to `capture`, `render` and
`sound-server`. Its `media` feature (default) turns the media stack on, and
each device feature implies it. `iroh-live-cli` always renders and defaults to
`aec`, `playback` and `sound-server`.
