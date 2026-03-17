# Windows platform

Media Foundation H.264 encoder and decoder, screen capture via `windows-capture`,
and camera capture via `nokhwa`. No code has been written yet.

## Status

- [ ] MFT H.264 encoder (`MediaFoundationEncoder`)
- [ ] MFT H.264 decoder (`MediaFoundationDecoder`)
- [ ] Screen capture via `windows-capture` crate
- [ ] Camera capture via `nokhwa` crate
- [ ] D3D11 zero-copy decode (deferred — CPU copy first)

## Details

### Codec integration

The encoder uses `IMFTransform` with `CLSID_CMSH264EncoderMFT`. Key
implementation detail: `SetOutputType` must be called before `SetInputType`
(opposite of the usual MFT pattern). The `windows` crate (MIT) provides the
FFI bindings.

Constructor sequence:
1. `MFStartup` → `MFTEnumEx` for H.264 encoder
2. `SetOutputType` with `MFVideoFormat_H264`, bitrate, profile
3. `SetInputType` with `MFVideoFormat_NV12`, dimensions, framerate
4. `ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING)`

NAL format: MFT outputs Annex B by default. For avcC mode, parse SPS/PPS from
the first IDR and build the avcC record using the existing `annexb` helpers in
`rusty-codecs`.

### Capture

- **Screen**: `windows-capture` (MIT) wraps `Windows.Graphics.Capture` with a
  callback-driven API. Outputs BGRA frames.
- **Camera**: `nokhwa` (Apache-2.0) provides a cross-platform camera API with
  a DirectShow backend on Windows.

### Feature gating

The `media-foundation` feature flag in `rusty-codecs` gates all Windows-specific
code. The `VideoCodec::MediaFoundationH264` variant and `DynamicVideoDecoder`
dispatch follow the same pattern as VAAPI and V4L2.

### Effort estimate

~950 lines of Rust across encoder, decoder, and capture integration. No D3D11
zero-copy in the first pass — decoded frames are copied to CPU memory and
converted via the existing NV12/I420 paths.

## Implementation history

No commits yet.
