# Platform support

H.264 encodes and decodes everywhere, because openh264 is vendored and statically
linked upstream. Everything below that is about which hardware path is available
and which parts we have actually run.

The per-backend detail is upstream: [moq-video](https://doc.moq.dev/lib/rs/crate/moq-video)
documents the capture, encode, decode, and render backends and the zero-copy
matrix, and [moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio) the audio
devices. This page says what that means for this repository.

| Platform | State here |
|---|---|
| Linux, Intel and AMD | Primary development target. Tested on Intel Meteor Lake |
| Linux, NVIDIA | NVENC and NVDEC behind the `nvidia` feature. Not tested here |
| macOS | Builds in CI. VideoToolbox and ScreenCaptureKit come from upstream. Lightly tested |
| Android | Tested on device, two-way audio and video against a Linux desktop |
| Raspberry Pi | Tested on a Pi Zero 2 W: publish through `rpicam-vid`, watch with software decode |
| Windows | Upstream has Media Foundation and DXGI. Never built or tested here |
| iOS | Upstream has AVFoundation and VideoToolbox. Never built or tested here |

CI builds and tests Linux and macOS, and cross-builds the CLI for aarch64 Linux.

## Linux

Camera capture is V4L2 and screen capture is PipeWire through
xdg-desktop-portal, behind the `pipewire` feature because it links
`libpipewire-0.3` at build time.

Encoding runs on openh264 by default. The `vaapi` feature adds Intel and AMD
hardware H.264 encode; upstream flags that backend as never validated on real
hardware, and it is off by default for that reason.

**Decoding is software only.** moq-video has no VAAPI decoder, so an H.264 stream
on Intel or AMD Linux decodes on the CPU. That is a regression from the in-house
stack this repository used to carry, and it is the one most likely to be felt: a
1080p stream that used to decode on the GPU now costs CPU.

Rendering is wgpu on Vulkan. Zero-copy DMA-BUF import works for packed RGB frames
from PipeWire screen capture when the device is created with
`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`; multi-plane NV12 import is
tracked upstream and not done. Decoded frames take the CPU upload path.

## macOS

Capture is AVFoundation for cameras and ScreenCaptureKit for displays, windows,
and applications, which is why `irl devices` lists windows and applications there
and nowhere else. VideoToolbox handles both encode and decode, and the renderer
imports the decoder's `CVPixelBuffer` through `CVMetalTextureCache`, so the whole
decode-to-screen path stays on the GPU. macOS also has system audio capture.

This is upstream's best-supported platform. We build it in CI and have run it by
hand, but it is not where the day-to-day testing happens.

## Android

MediaCodec encode and decode are upstream in moq-video, ported out of this
repository during the v2 rewrite and gated on `cfg(target_os = "android")`.
Camera frames are pushed in from Kotlin's Camera2 through
`moq_media_android::camera`, and decoded `AHardwareBuffer` frames are drawn by
`moq_media_android::renderer` as an EGL external texture, which is zero-copy from
the decoder to the screen. See [the Android guide](guide/android.md).

## Raspberry Pi

Publishing goes through `rpicam-vid`, which drives the libcamera ISP and the Pi's
hardware encoder; the Pi never software-encodes and never sees a raw picture.
Watching decodes H.264 in software and draws through a GLES2 renderer that lives
in the demo, since the Pi Zero has no Vulkan.

The V4L2 stateful M2M path that used to drive the VideoCore codec directly is
gone, and so is raw libcamera capture. See [the Raspberry Pi
guide](guide/raspberry-pi.md).

## What no longer exists anywhere

AV1 encode and software AV1 decode came from rav1e and rav1d in the deleted
stack. Upstream decodes AV1 through NVDEC only, so an AV1 stream needs an NVIDIA
GPU or it does not play. H.265 is hardware-only for the same reason: there is no
software fallback, and a machine without a platform backend gets an error rather
than a slow path.
