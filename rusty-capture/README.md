# rusty-capture

Cross-platform screen and camera capture with zero-copy GPU buffer support.

Provides `ScreenCapturer` and `CameraCapturer` types that implement
`rusty_codecs::traits::VideoSource`, producing `VideoFrame` values ready for
encoding or rendering.

## Platform backends

| Platform | Screen | Camera | Zero-copy |
|----------|--------|--------|-----------|
| Linux (PipeWire) | `pipewire` feature | `pipewire` feature | DMA-BUF mmap (NV12, BGRx) |
| Linux (X11) | `x11` feature | — | No (MIT-SHM CPU copy) |
| Linux (V4L2) | — | `v4l2` feature | DMA-BUF via EXPBUF (planned) |
| macOS | `apple-screen` feature | `apple` feature | IOSurface (planned) |
| iOS | — | `apple` feature | IOSurface (planned) |
| Windows | — | — | D3D11 (planned) |
| Android | — | — | AHardwareBuffer (planned) |

When multiple Linux backends are enabled, the runtime dispatch prefers PipeWire
(if the daemon is running) over X11 and V4L2.

## Feature flags

- **`pipewire`** — PipeWire screen + camera capture via xdg-desktop-portal.
  Requires `libpipewire-0.3-dev` at build time. Supports DMA-BUF buffers from
  compositors and libcamera sources.

- **`v4l2`** — V4L2 camera capture. Works on Raspberry Pi (1–5, Zero) and USB
  cameras via the `v4l2r` crate. MMAP streaming with planned DMA-BUF export.

- **`x11`** — X11 screen capture via MIT-SHM. CPU-only fallback for
  non-Wayland systems.

- **`apple`** — AVFoundation camera capture on macOS and iOS.

- **`apple-screen`** — ScreenCaptureKit screen capture on macOS 12.3+.

## Usage

```rust
use rusty_capture::{ScreenConfig, PipeWireScreenCapturer, VideoSource};

let mut capturer = PipeWireScreenCapturer::new(&ScreenConfig::default())?;

loop {
    if let Some(frame) = capturer.pop_frame()? {
        // frame is a VideoFrame — encode, render, or inspect.
        println!("{}x{}", frame.width(), frame.height());
    }
}
```

## Build dependencies

### Linux (PipeWire)

```sh
# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libclang-dev

# Arch
sudo pacman -S pipewire libclang
```

### Linux (V4L2)

No system dependencies beyond a working V4L2 kernel driver.

### macOS

Xcode command-line tools provide the required frameworks (AVFoundation,
ScreenCaptureKit, CoreMedia, CoreVideo).

## Architecture

Each platform backend runs its capture loop on a dedicated OS thread and sends
frames to the caller via `std::sync::mpsc`. The `VideoSource` trait provides a
uniform `pop_frame()` interface that drains to the latest frame, preventing
latency buildup.

PipeWire backends use the xdg-desktop-portal D-Bus API (via `ashpd`) for
session negotiation, then consume the PipeWire stream directly via
`pipewire-rs`. DMA-BUF buffers are mmapped and copied within the process
callback — not true zero-copy, but avoids the compositor→SHM copy overhead
that SHM-only paths incur.
