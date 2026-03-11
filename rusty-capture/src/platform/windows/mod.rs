//! Windows capture backends (stub).
//!
//! # Implementation Plan
//!
//! ## Screen Capture
//!
//! Two viable APIs, both producing GPU-resident `ID3D11Texture2D`:
//!
//! - **DXGI Desktop Duplication** (`IDXGIOutputDuplication`): Works on Windows 8+.
//!   `AcquireNextFrame()` gives an `IDXGIResource` → `ID3D11Texture2D` on GPU.
//!   Single-GPU only, full desktop only.
//!
//! - **Windows.Graphics.Capture** (WGC): Works on Windows 10 1803+.
//!   `Direct3D11CaptureFramePool` gives `IDirect3DSurface` → `ID3D11Texture2D`.
//!   Supports per-window capture, cross-GPU. Recommended for modern Windows.
//!
//! Use `windows-capture` crate (v1.5.0, 130k downloads) for WGC, or the raw
//! `windows` crate for DXGI.
//!
//! ## Camera Capture
//!
//! MediaFoundation `IMFSourceReader` with `MF_SOURCE_READER_D3D_MANAGER` for
//! early GPU upload. USB cameras always start on CPU (USB transport limitation),
//! but the upload to `ID3D11Texture2D` happens once, then stays on GPU for
//! encoding via MF H.264 MFT with `IMFDXGIDeviceManager`.
//!
//! ## Zero-Copy Path
//!
//! Add `NativeFrameHandle::D3D11Texture(ID3D11Texture2D)` variant. Both screen
//! (WGC) and camera (after upload) converge on `ID3D11Texture2D` → wrap as
//! `FrameData::Gpu` with the D3D11 native handle.
//!
//! MF H.264 encoder accepts `IMFSample` wrapping `ID3D11Texture2D` via
//! `MFCreateDXGISurfaceBuffer` — true GPU-to-GPU encode.
//!
//! ## Dependencies
//!
//! - `windows-capture` (v1.5.0) for WGC screen capture
//! - `windows` crate (Microsoft official) for MF camera + D3D11 interop
//!
//! ## Estimated Effort
//!
//! - Screen (WGC): ~120 lines
//! - Camera (MF): ~150 lines
//! - Shared D3D11→GpuFrame wrapper: ~80 lines
