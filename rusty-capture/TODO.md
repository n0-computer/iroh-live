# rusty-capture — TODO

Tracks missing platforms, features, and improvements. Each item is scoped for
an agent to pick up independently.

## Linux

### PipeWire

- [ ] **True DMA-BUF zero-copy**: hold the PipeWire buffer across frames
  instead of mmap+copy in the process callback. Requires a buffer pool that
  tracks which buffers are in flight and defers `queueBuffer` until the
  downstream consumer releases them. Produces `FrameData::Gpu(GpuFrame)` with
  `NativeFrameHandle::DmaBuf`.

- [ ] **DMA-BUF modifier negotiation**: parse `SPA_FORMAT_VIDEO_modifier` from
  the format pod and pass it through to `DmaBufInfo`. Compositors like Mutter
  and KWin advertise specific modifiers (e.g. Intel CCS) that downstream
  Vulkan/VAAPI importers need to know.

- [ ] **PipeWire camera enumeration**: the Camera portal returns a single fd
  with `PW_ID_ANY`. Enumerate actual camera nodes from the PipeWire graph to
  populate `CameraInfo` with real device names and supported formats.

- [ ] **PipeWire screen capture target_fps**: honor `ScreenConfig::target_fps`
  by setting the framerate constraint in the format pod instead of using a
  range.

- [ ] **Reconnect on stream error**: if the PipeWire stream errors (e.g.
  compositor restarts), attempt reconnection instead of silently stopping.

### V4L2

- [ ] **DMA-BUF export via EXPBUF**: after MMAP buffer allocation, call
  `VIDIOC_EXPBUF` to get DMA-BUF file descriptors when the driver supports it.
  Enables zero-copy to V4L2 M2M encoders (Pi 4 `/dev/video11`) and VAAPI
  encoders. Fall back to MMAP CPU path when EXPBUF returns `EINVAL`/`ENOTTY`.

- [ ] **Non-blocking dequeue**: use `poll()` or `select()` instead of
  `try_recv` + 1ms sleep for waiting on filled buffers.

- [ ] **Frame interval control**: set `VIDIOC_S_PARM` to request the desired
  framerate from `CameraConfig::preferred_fps`.

- [ ] **Multi-plane support**: handle V4L2 multi-plane devices
  (`VIDEO_CAPTURE_MPLANE`) for cameras that use separate Y/UV planes.

### X11

- [ ] **XRandR monitor enumeration**: use XRandR to get per-output monitor
  info (position, refresh rate, scale) instead of iterating X11 screen roots.

- [ ] **XDamage incremental capture**: use the XDamage extension to capture
  only changed regions, reducing CPU load for mostly-static screens.

## macOS

### ScreenCaptureKit (`apple-screen`)

- [ ] **Implement `MacScreenCapturer`**: wrap `SCStream` to capture screen
  content. Use `SCContentFilter` for monitor/window selection and
  `SCStreamConfiguration` for resolution and framerate. Output
  `CMSampleBuffer` frames and convert to `VideoFrame`.

- [ ] **Monitor enumeration**: implement `monitors()` using
  `SCShareableContent` to list displays with position, dimensions, and scale.

- [ ] **IOSurface zero-copy**: extract `IOSurface` from `CMSampleBuffer` and
  wrap as `GpuFrame` with a future `NativeFrameHandle::IoSurface` variant.

### AVFoundation (`apple`)

- [ ] **Implement `AppleCameraCapturer`**: wrap `AVCaptureSession` with
  `AVCaptureDeviceInput` and `AVCaptureVideoDataOutput`. Use the
  `captureOutput:didOutputSampleBuffer:` delegate callback to receive frames.

- [ ] **Camera enumeration**: implement `cameras()` using
  `AVCaptureDevice.DiscoverySession` to list devices with supported formats.

- [ ] **IOSurface zero-copy**: extract `CVPixelBuffer` → `IOSurface` from
  sample buffers for GPU-direct paths.

## Windows

- [ ] **DXGI Desktop Duplication**: implement screen capture using
  `IDXGIOutputDuplication`. Produces D3D11 textures that can be imported into
  encoders via `NativeFrameHandle::D3D11Texture`.

- [ ] **Media Foundation camera**: implement camera capture using
  `IMFSourceReader` with `MF_SOURCE_READER_FIRST_VIDEO_STREAM`.

- [ ] **Monitor enumeration**: use `EnumDisplayMonitors` + `GetMonitorInfo` to
  populate `MonitorInfo`.

- [ ] **Camera enumeration**: use `MFEnumDeviceSources` with
  `MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID`.

## Android

- [ ] **Camera2 API**: implement camera capture using `ACameraManager` /
  `ACaptureRequest` from the NDK Camera2 API. Output `AHardwareBuffer` frames
  for zero-copy encode.

- [ ] **Screen capture**: implement via `MediaProjection` API (requires JNI
  bridge or `ndk` crate equivalent).

## Cross-platform

- [ ] **Async `VideoSource` trait**: evaluate an async variant of `pop_frame()`
  that awaits the next frame instead of polling. Would simplify integration
  with tokio-based pipelines.

- [ ] **Frame timestamp propagation**: capture backends should set
  `VideoFrame::timestamp` from the source clock (PipeWire PTS, V4L2
  `v4l2_buffer.timestamp`, CMSampleBuffer PTS) instead of `Duration::ZERO`.

- [ ] **Configurable buffer depth**: allow callers to control the mpsc channel
  bound to trade latency for resilience against producer/consumer jitter.

- [ ] **Error recovery callback**: expose a callback or event channel for
  capture errors (device unplugged, permission revoked) so callers can react
  without polling.

- [ ] **Integration tests**: add end-to-end tests that create a capturer,
  receive at least one frame, and verify dimensions match. Gate behind a
  `test-capture` feature that requires a display server / camera.
