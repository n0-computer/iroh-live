//! Android capture backends (stub).
//!
//! # Implementation Plan
//!
//! ## Screen Capture
//!
//! Use `MediaProjection` API (Android 5.0+):
//! 1. Request `MediaProjection` permission (shows system dialog)
//! 2. Create `VirtualDisplay` with an `ImageReader` surface
//! 3. `ImageReader` delivers `Image` objects containing `AHardwareBuffer`
//! 4. `AHardwareBuffer` is GPU-resident — import into Vulkan via
//!    `VK_ANDROID_external_memory_android_hardware_buffer`
//!
//! Add `NativeFrameHandle::AHardwareBuffer(..)` variant for zero-copy.
//!
//! ## Camera Capture
//!
//! Use Camera2 API (Android 5.0+):
//! 1. Open `CameraDevice` via `CameraManager`
//! 2. Create `CaptureSession` with an `ImageReader` surface
//! 3. `ImageReader` delivers YUV_420_888 `Image` objects backed by
//!    `AHardwareBuffer`
//! 4. Import into MediaCodec H.264 encoder surface for zero-copy encode
//!
//! ## Rust Bindings
//!
//! - `ndk` crate for `AHardwareBuffer`, `AImageReader`
//! - `jni` crate for Camera2 API (Java/Kotlin API, no stable C NDK equivalent)
//! - Alternatively, use `camera2-ndk` if/when Android NDK adds native Camera2
//!
//! ## Shared Code
//!
//! Both screen and camera produce `AHardwareBuffer`. Downstream
//! AHardwareBuffer→GpuFrame wrapper is shared (~60 lines).
//!
//! ## Estimated Effort
//!
//! - Screen (MediaProjection): ~200 lines (complex JNI)
//! - Camera (Camera2): ~250 lines (complex JNI)
//! - Shared AHardwareBuffer wrapper: ~60 lines
