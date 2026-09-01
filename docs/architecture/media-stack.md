# The media stack

Capture, encoding, decoding, and GPU rendering are `moq-video` and `moq-audio`,
upstream in the [moq](https://github.com/moq-dev/moq) repository. This repository
no longer carries a codec, a capture backend, or a renderer of its own. What
follows is what we use, what we add, and what we lost when the in-house stack was
deleted.

The authoritative documentation for the upstream half is upstream:

- [moq-video](https://doc.moq.dev/lib/rs/crate/moq-video): the capture, encode,
  decode, and render modules, the backend selection order, the zero-copy matrix
  per platform, and the device enumerators.
- [moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio): microphone capture,
  Opus and PCM encode, Opus, PCM, and AAC-LC decode, the playback engine, and
  echo cancellation.
- [hang](https://doc.moq.dev/lib/rs/crate/hang) and
  [moq-mux](https://doc.moq.dev/lib/rs/crate/moq-mux): the catalog and the
  container formats.

`moq-media` re-exports both as `moq_media::video` and `moq_media::audio`, so a
crate that depends on moq-media names the exact build of moq-video that our
renderer links, rather than guessing at a compatible version. That matters most
for wgpu: `moq_video::render` hands back a `wgpu::Texture` from its own build of
wgpu 30, and a texture from a different wgpu major is a different type.

## What runs where

Codec selection is upstream's, and it is automatic and ordered: platform hardware
first, then openh264 as the software fallback. No public type or error variant
names a backend. `moq_video::encode::Kind` lets a caller ask for `Hardware`,
`Software`, or a specific backend by `Named("vaapi")` and friends, which is what
`irl publish --encoder` exposes.

H.264 encodes and decodes everywhere, because openh264 is statically linked and
always compiled in. H.265 is hardware-only: with no usable platform backend you
get an error rather than a slow path. AV1 is decode-only, via NVDEC on Linux
and MediaCodec on Android.

Audio is Opus or PCM out, and Opus, PCM, or AAC-LC in. AAC decode stays enabled
in our dependency because a broadcast that reached us through an RTMP or HLS
gateway carries AAC, and dropping its audio track would be a silent failure.

## What we add

`moq_media::rpicam` drives `rpicam-vid` and publishes the Annex-B H.264 it
already encoded. Shelling out to a camera application is an application concern
rather than a moq-video one, which is why it lives here. See [Raspberry
Pi](../guide/raspberry-pi.md).

`moq_media::audio_file` demuxes and decodes a local audio file with symphonia and
presents the result as a frame stream. moq-audio pulls symphonia only for raw
AAC-LC frames off the wire, so it has no container reader we could use instead.

`moq_media::test_source` generates a moving pattern and a sine tone, so a test can
publish over a real transport with no camera and no microphone. The pattern
changes every frame on purpose: a static image compresses to almost nothing after
the first keyframe, so a test watching for bytes would pass on a stalled pipeline.

`moq-media-egui` draws the texture `moq_video::render` returns inside an egui
panel, and carries the debug overlay. `moq-media-android` provides the Camera2
push bridge and an EGL renderer for `AHardwareBuffer` frames, neither of which is
a moq-video concern. `demos/pi-zero/src/gles.rs` is a GLES2 renderer for hardware
with no Vulkan, which is the Pi Zero.

## What we contributed upstream

Five commits landed on `moq-dev/moq` during this work, in the order they were
needed:

1. `feat(moq-video): expose the capture stream`. `capture::open` and its frame
   stream were `pub(crate)`, so `encode::publish_capture` was the only way to
   reach a camera. That covers one rendition and nothing else, and a simulcast
   ladder, a local preview, and a compositing pipeline all need raw frames.
2. `feat(moq-audio): let publish_capture carry a catalog extension`.
   `encode::Producer` was already generic over the catalog extension but
   `publish_capture` pinned it to `()`, so an application publishing an extended
   catalog could use the turnkey path for video and had to hand-roll audio.
3. `feat(moq-video): add a HardwareBuffer surface variant for Android`.
4. `feat(moq-video): add the Android MediaCodec encoder`.
5. `feat(moq-video): add the Android MediaCodec decoder`.

The MediaCodec pair replaces the encoder and decoder this repository used to
carry. They sit behind `cfg(target_os = "android")` alongside the objc2 and
Windows families, and are selected with `encode::Kind::Named("mediacodec")`.

Until those are in a published release, the workspace carries a
`[patch.crates-io]` block pointing the whole moq family at
`Frando/moq@iroh-live`, one commit per prospective PR. Every pinned version
matches what `moq-dev/moq@main` publishes, so deleting the patch block is the
whole revert.

## What we lost

Three capabilities went away with the in-house stack and have no replacement.

**VAAPI decode.** moq-video has a VAAPI encoder and no VAAPI decoder, so H.264
decode on Intel and AMD Linux runs in software through openh264. That is a
performance regression, not a correctness one, and it is the one most likely to
be noticed: a 1080p stream that used to decode on the GPU now costs CPU.

**V4L2 stateful M2M encode and decode.** This is what drove the Raspberry Pi's
VideoCore hardware codec. It is not on any path we still ship, because the Pi
demos publish pre-encoded H.264 through `rpicam-vid` and never open an encoder,
but the on-device codec test and the software-encode fallback both went with it.

**Raw libcamera capture.** The YUV capture path that opened the Pi camera as a
device is gone; `rpicam-vid` is the only Pi camera source. A libcamera capture
backend is a genuine moq-video concern and is offered upstream, but it does not
exist yet.

Two smaller things also went: AV1 encode and software AV1 decode, which came from
rav1e and rav1d, and the dioxus integration crate, which had no users.

## Feature flags

Every codec compiles unconditionally upstream, so the old per-codec flags are
gone. What is left gates a build dependency or a graphics stack. `moq-media`
defines them and `iroh-live` and `iroh-live-cli` pass them through.

| Feature | Default | What it costs |
|---|---|---|
| `capture` | yes | Camera, screen, and microphone devices. Pulls V4L2 and ALSA build dependencies on Linux |
| `playback` | no | Speaker output through `moq_audio::playback` |
| `aec` | no | Echo cancellation. Implies `capture` and `playback`, since the canceller taps the output mix |
| `pipewire` | no | Linux screen capture through xdg-desktop-portal. Links `libpipewire-0.3` at build time |
| `render` | no | The wgpu renderer and its graphics stack |
| `vaapi` | no | Intel and AMD hardware H.264 encode |
| `nvidia` | no | NVIDIA hardware encode and decode. On upstream by default, off here so a default build stays free of the CUDA graph |
| `rpicam` | no | The `rpicam-vid` source. Linux only, and needs the binary on PATH |
| `test-source` | no | The generated video and audio sources |

`iroh-live` defaults to `capture` and `render`. `iroh-live-cli` adds `playback`.
