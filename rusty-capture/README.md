# rusty-capture

Cross-platform screen and camera capture for the iroh-live media pipeline.

Each platform backend runs its capture loop on a dedicated OS thread and delivers frames through the `VideoSource` trait. `pop_frame()` always returns the latest frame, preventing latency buildup when the encoder is slower than the capture rate.

## Platform support

| Platform | Screen | Camera | Backend | Feature |
|----------|--------|--------|---------|---------|
| Linux (Wayland) | Yes | Yes | PipeWire + xdg-desktop-portal | `pipewire` |
| Linux (X11) | Yes | -- | MIT-SHM | `x11` |
| Linux | -- | Yes | V4L2 | `v4l2` |
| macOS 12.3+ | Yes | -- | ScreenCaptureKit | `apple-screen` |
| macOS, iOS | -- | Yes | AVFoundation | `apple` |
| Android | -- | Yes | CameraX via JNI | (in `moq-media-android`) |
| Windows | -- | -- | -- | -- |

See [`plans/platforms.md`](../plans/platforms.md) for tested hardware and future plans.

When multiple Linux backends are enabled, runtime dispatch prefers PipeWire (if the daemon is running) over X11 and V4L2.

## Usage

```rust
use rusty_capture::{ScreenConfig, PipeWireScreenCapturer, VideoSource};

let mut capturer = PipeWireScreenCapturer::new(&ScreenConfig::default())?;

loop {
    if let Some(frame) = capturer.pop_frame()? {
        // frame is a VideoFrame: encode, render, or inspect
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

Xcode command-line tools provide the required frameworks (AVFoundation, ScreenCaptureKit, CoreMedia, CoreVideo).

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `camera` | yes | Camera capture (via platform backend) |
| `screen` | yes | Screen capture (via platform backend) |
| `xcap` | yes | Cross-platform screen capture fallback |
| `nokhwa` | yes | Cross-platform camera capture fallback |
| `pipewire` | | PipeWire screen + camera capture via xdg-desktop-portal |
| `v4l2` | | V4L2 camera capture (USB cameras, Raspberry Pi) |
| `x11` | | X11 screen capture via MIT-SHM |
| `apple` | | AVFoundation camera capture (macOS, iOS) |
| `apple-screen` | | ScreenCaptureKit screen capture (macOS 12.3+) |

## Architecture

PipeWire backends use the xdg-desktop-portal D-Bus API (via `ashpd`) for session negotiation, then consume the PipeWire stream directly via `pipewire-rs`. DMA-BUF buffers are memory-mapped within the process callback. V4L2 uses MMAP streaming for zero-copy kernel-to-userspace transfer.
