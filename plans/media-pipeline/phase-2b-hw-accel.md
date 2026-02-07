# Phase 2b: Hardware Acceleration (VAAPI + VideoToolbox)

**Status: Pending**

## Goal
Add optional hardware-accelerated H.264 encoding behind feature flags. After this phase, Linux users get VAAPI GPU encoding and macOS users get VideoToolbox.

## Prerequisites
- Phase 1 complete (openh264 baseline working)
- Phase 2 complete (DynamicVideoDecoder pattern established)

## Feature flags
```toml
[features]
vaapi = ["dep:cros-codecs"]
videotoolbox = ["dep:objc2-video-toolbox"]
```

## Steps

### 1. VAAPI backend (Linux)

#### `codec/video/vaapi_enc.rs` — feature: `vaapi`
- **Crate**: `cros-codecs` — Rust + libva, H.264 VAAPI encoding
- **Input**: YUV420P planes (from shared convert.rs)
- **Output**: H.264 NAL units
- **Config**: match openh264 parameters (bitrate, GOP, profile)
- **Detection**: probe for VAAPI device at `/dev/dri/renderD128`
- **Fallback**: if VAAPI init fails, caller falls back to openh264
- Implement `VideoEncoder` + `VideoEncoderInner` traits
- `#[cfg(feature = "vaapi")]` gated

### 2. VideoToolbox backend (macOS)

#### `codec/video/vtb_enc.rs` — feature: `videotoolbox`
- **Crate**: `objc2-video-toolbox` — Rust bindings to Apple VideoToolbox
- **API flow**:
  1. `VTCompressionSessionCreate` with H.264 codec type
  2. Set properties: bitrate, keyframe interval, profile, realtime flag
  3. Per frame: `CVPixelBuffer` from YUV420P → `VTCompressionSessionEncodeFrame`
  4. Callback receives `CMSampleBuffer` → extract H.264 NAL data
- **avcC**: Extract format description with SPS/PPS for `VideoConfig`
- Implement `VideoEncoder` + `VideoEncoderInner` traits
- `#[cfg(all(target_os = "macos", feature = "videotoolbox"))]` gated

### 3. Dynamic encoder selection (optional)
- Consider a `DynamicVideoEncoder` pattern similar to `DynamicVideoDecoder`
- Or keep it simple: user selects encoder type via CLI/config
- Backend probing: try HW first, fall back to SW

### 4. Feature forwarding

#### `iroh-live/Cargo.toml`
```toml
[features]
vaapi = ["moq-media/vaapi"]
videotoolbox = ["moq-media/videotoolbox"]
```

### 5. Update examples
- Add `--codec vaapi-h264` and `--codec vtb-h264` options to examples
- Or auto-detect: `--hw` flag tries HW first, falls back to SW

## Testing

- Feature-gate compilation: `cargo check` with each feature individually
- All features combined: `cargo check --features vaapi,videotoolbox,av1`
- VAAPI encode roundtrip (requires GPU, `#[ignore]` by default)
- VideoToolbox encode roundtrip (requires macOS HW, `#[ignore]` by default)
- Cross-codec decode: HW-encoded H.264 decodable by openh264
- Fallback on init failure: HW unavailable → clean error, not panic
