# Platform support

H.264 encodes and decodes everywhere, through the openh264 software codec that
moq-video builds in. The rest of this page is about hardware paths and about
what we have run.

The backends themselves are documented upstream:
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) covers capture,
encode, decode, render, and zero-copy, and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio) covers audio devices.

| Platform | State |
|---|---|
| Linux, Intel and AMD | Main development target. Tested on Intel Meteor Lake |
| Linux, NVIDIA | NVENC and NVDEC behind the `nvidia` feature. Not tested here |
| macOS | Built in CI and by hand. Lightly tested |
| Android | Tested on a handset: two-way audio and video with a Linux desktop |
| Raspberry Pi | Tested on a Pi Zero 2 W and a Pi 4 |
| Windows | Built for release. Not tested here |
| iOS | Upstream has AVFoundation and VideoToolbox. Never built here |

CI checks and tests Linux and macOS, and cross-builds the CLI for aarch64
Linux. The release workflow builds `irl` for Linux x86-64 and aarch64, macOS
aarch64, and Windows x86-64, plus an arm64 Android APK. The Linux builds add
`nvidia`, `vaapi`, `v4l2`, `rpicam`, `pipewire`, and `aec` to the default
features. macOS and Windows add `aec`.

## Linux

Cameras are captured through V4L2. Screen capture goes through PipeWire and
xdg-desktop-portal, behind the `pipewire` feature, which links
`libpipewire-0.3`.

The `vaapi` feature adds VA-API H.264 encode and decode for Intel and AMD.
Upstream marks the encoder as not yet validated on hardware. The decoder hands
its pictures to the renderer as DMA-BUFs, so decoded video stays on the GPU up
to the screen. `irl watch`, `irl call`, and `irl room` all draw it that way.
PipeWire screen capture frames are imported the same way when the wgpu device
has `wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`.

The `v4l2` feature adds the memory-to-memory H.264 codecs that ARM SoCs expose
as device nodes. On a desktop it is of no use.

Rendering is wgpu on Vulkan.

## macOS

Cameras are captured through AVFoundation. Displays, windows, and applications
are captured through ScreenCaptureKit, which is why `irl devices` lists windows
and applications only here. System audio capture works too. VideoToolbox
encodes and decodes, and the renderer imports decoded pictures through
`CVMetalTextureCache`, so they stay on the GPU.

## Android

MediaCodec encode and decode are in moq-video. The app pushes Camera2 frames
through `iroh_live_media_android::camera`, and
`iroh_live_media_android::renderer` draws decoded `AHardwareBuffer` frames as
an EGL external texture without a copy. See [Android](guide/android.md).

## Raspberry Pi

`irl publish --video rpicam` runs `rpicam-vid`, which drives the camera ISP and
the Pi's hardware H.264 encoder. It needs the `rpicam` feature and `rpicam-vid`
on `PATH`. `--video rpicam:raw` reads raw pictures from `rpicam-vid` instead,
for our own encoders.

The `v4l2` feature adds the VideoCore memory-to-memory encoder and decoder.
Both run on a Pi 4. `MOQ_V4L2_ENCODER` and `MOQ_V4L2_DECODER` name a device
node when probing picks the wrong one.

The Pi Zero has no Vulkan, so the Pi demo draws with its own GLES2 renderer.
See [Raspberry Pi](guide/raspberry-pi.md).

## Codecs without software fallback

There is no software decoder for H.265 or AV1, and no software encoder for
H.265. These codecs need a hardware backend. Without one, opening the encoder
or decoder fails.
