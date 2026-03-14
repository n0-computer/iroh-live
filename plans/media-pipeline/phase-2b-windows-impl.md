# Windows Platform Implementation Plan

The existing design document (`phase-2b-windows-media-foundation.md`) covers the
MFT encoder architecture. This document tracks what is needed for a complete
Windows implementation and what we fall short on today.

## Current state

No platform-specific code exists for Windows. Software codecs (openh264, rav1e,
rav1d, unsafe-libopus) work on Windows out of the box since they are pure
Rust/C. The gap is hardware acceleration and native capture.

## Crate selection

Research into MIT/Apache-2.0-compatible crates (2026-03-14):

| Purpose | Crate | License | Notes |
|---------|-------|---------|-------|
| Win32 API bindings | `windows` (Microsoft) | MIT/Apache-2.0 | MFT, DXGI, D3D11 |
| Screen capture | `windows-capture` | MIT | WGC + DXGI DD, mature |
| Camera capture | `nokhwa` | Apache-2.0 | Media Foundation backend |

All three are license-compatible.

## Work items

### 1. Media Foundation H.264 encoder (~400 LOC)

Wrap `IMFTransform` (CLSID_CMSH264EncoderMFT) from the `windows` crate. The API
surface is:

- `MFTEnumEx` to find the hardware H.264 encoder
- `IMFTransform::SetInputType` / `SetOutputType` with `MFVideoFormat_NV12` input
  and `MFVideoFormat_H264` output
- `IMFTransform::ProcessInput` / `ProcessOutput` per frame
- `IMFSample` + `IMFMediaBuffer` for data exchange
- Rate control via `ICodecAPI::SetValue(CODECAPI_AVEncCommonMeanBitRate)`

**Where we fall short**: No `IMFDXGIDeviceManager` integration for GPU-resident
input buffers. The first implementation would upload CPU frames, matching our
VAAPI encoder's CPU path. Zero-copy D3D11 texture input is a future
optimization.

**Inspiration**: The C++ MFT round-trip pattern from Microsoft's Media Foundation
SDK samples (MIT-licensed).

### 2. Media Foundation H.264 decoder (~350 LOC)

Same `IMFTransform` pattern with `CLSID_CMSH264DecoderMFT`. Output
`MFVideoFormat_NV12`, wrap in a `MfGpuFrame` implementing `GpuFrameInner`.
Register in `DynamicVideoDecoder` with auto-detection like VAAPI and VTB.

**Where we fall short**: No D3D11 texture output for zero-copy wgpu rendering.
The first implementation would read NV12 from CPU-accessible `IMFMediaBuffer`,
matching the VTB decoder's download pattern. D3D11→wgpu texture import is a
future optimization.

### 3. Screen capture (~150 LOC)

Use `windows-capture` crate (MIT). It provides BGRA CPU frames via
Windows.Graphics.Capture (WGC) on Windows 10 1803+, falling back to DXGI
Desktop Duplication on older versions.

Integration: implement the `CaptureSource` trait from `rusty-capture`, emit
`VideoFrame::new_packed(data, w, h, PixelFormat::Bgra)`.

**Where we fall short**: No D3D11 texture output for zero-copy encode. The
`windows-capture` crate outputs CPU BGRA only. For zero-copy, we would need to
use the raw WGC API to get `ID3D11Texture2D` directly, which is a separate
(larger) effort.

### 4. Camera capture (~50 LOC)

Use `nokhwa` crate (Apache-2.0) with `input-native` feature for Media Foundation
on Windows. This is the same crate already used for Linux (V4L2) and macOS
(AVFoundation).

**Where we fall short**: `nokhwa` provides CPU frames only. No GPU-resident
camera frame path.

### 5. Feature gating

Add to `rusty-codecs/Cargo.toml`:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.62", optional = true, features = [
    "Win32_Media_MediaFoundation",
    "Win32_Graphics_Direct3D11",
] }
```

Feature flag: `media-foundation = ["dep:windows"]`

### 6. Dynamic decoder/encoder registration

Same pattern as VAAPI and VTB: `#[cfg(all(target_os = "windows", feature =
"media-foundation"))]` in `dynamic.rs` and `codec.rs`.

## Priority and ordering

1. Screen capture via `windows-capture` — lowest effort, highest visibility
2. Camera via `nokhwa` — trivial if already used on other platforms
3. MFT H.264 encoder — enables hardware encode on Windows
4. MFT H.264 decoder — enables hardware decode on Windows

Total estimated effort: ~950 LOC, spread across rusty-codecs and rusty-capture.

## What is NOT planned

- **D3D11 zero-copy encode/decode**: Requires `IMFDXGIDeviceManager`, D3D11
  texture sharing, and wgpu HAL-level D3D11 import. Significant complexity for
  moderate gain. Deferred until profiling shows CPU copy is a bottleneck.
- **DirectShow**: Legacy API, superseded by Media Foundation. Not worth
  supporting.
- **WMF audio**: Opus via unsafe-libopus already works. No need for MF AAC.
