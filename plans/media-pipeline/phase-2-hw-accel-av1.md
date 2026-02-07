# Phase 2: Hardware Acceleration + AV1

## Goal
Add optional hardware-accelerated H.264 encoding and AV1 codec support behind feature flags. After this phase, users on Linux get VAAPI GPU encoding, macOS users get VideoToolbox, and AV1 is available as an alternative codec.

## Prerequisites
- Phase 1 complete (codec/ module with openh264 baseline working)
- `VideoEncoderBackend` trait defined and used by `codec/video/encoder.rs`

## Feature flags
```toml
[features]
vaapi = ["dep:cros-codecs"]
videotoolbox = ["dep:objc2-video-toolbox"]
av1 = ["dep:rav1e", "dep:rav1d"]
```

## Steps

### 1. VAAPI backend (Linux)

#### `codec/video/vaapi_enc.rs` — feature: `vaapi`
- **Crate**: `cros-codecs` — Rust + libva, H.264 VAAPI encoding
- **Input**: YUV420P planes (from shared convert.rs layer)
- **Output**: H.264 NAL units
- **Config**: match openh264 parameters (bitrate, GOP, profile)
- **Detection**: probe for VAAPI device at `/dev/dri/renderD128` (or configurable)
- **Fallback**: if VAAPI init fails, backend selector moves to next candidate
- Implement `VideoEncoderBackend` trait
- `#[cfg(feature = "vaapi")]` gated

#### Backend selector update
- `codec/video/encoder.rs`: add VAAPI to candidate list on Linux, before openh264:
  ```rust
  #[cfg(all(target_os = "linux", feature = "vaapi"))]
  candidates.push(VaapiBackend);
  ```

### 2. VideoToolbox backend (macOS)

#### `codec/video/vtb_enc.rs` — feature: `videotoolbox`
- **Crate**: `objc2-video-toolbox` — Rust bindings to Apple VideoToolbox framework
- **API flow**:
  1. `VTCompressionSessionCreate` with H.264 codec type
  2. Set properties: bitrate, keyframe interval, profile (baseline), realtime flag
  3. Per frame: create `CVPixelBuffer` from YUV420P data → `VTCompressionSessionEncodeFrame`
  4. Callback receives `CMSampleBuffer` → extract H.264 NAL data
- **Input**: YUV420P planes → `CVPixelBuffer` (kCVPixelFormatType_420YpCbCr8Planar)
- **Output**: H.264 NAL units (Annex B or length-prefixed from VTB)
- **avcC**: VideoToolbox provides format description with SPS/PPS — extract for `VideoConfig`
- Implement `VideoEncoderBackend` trait
- `#[cfg(all(target_os = "macos", feature = "videotoolbox"))]` gated

#### Backend selector update
- Add VideoToolbox to candidate list on macOS, before openh264:
  ```rust
  #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
  candidates.push(VideotoolboxBackend);
  ```

### 3. AV1 encoder

#### `codec/video/rav1e_enc.rs` — feature: `av1`
- **Crate**: `rav1e` — pure Rust AV1 encoder
- **Config**:
  - Speed preset: 10 (fastest, for real-time)
  - Bitrate: `pixels * 0.05 * framerate_factor` (AV1 is ~30% more efficient than H.264)
  - Keyframe interval: same as fps
  - Pixel format: YUV420P 8-bit
  - Threads: 0 (auto)
- **API flow**:
  1. `Config::new()` → set dimensions, timebase, bitrate, speed
  2. `Context::new(config)` 
  3. Per frame: `ctx.send_frame(frame)` → `ctx.receive_packet()`
- **Extradata**: AV1 sequence header OBU from first packet
- **Integration with hang**: `VideoCodec::AV1` variant in catalog config
- Implement `VideoEncoderBackend` trait
- `#[cfg(feature = "av1")]` gated

### 4. AV1 decoder

#### `codec/video/rav1d_dec.rs` — feature: `av1`
- **Crate**: `rav1d` — pure Rust AV1 decoder (port of dav1d, ~5% slower)
- **API flow**:
  1. Create decoder settings + context
  2. Feed OBU packets → `send_data()`
  3. Pull decoded frames → `get_picture()`
  4. Extract YUV planes → convert to RGBA via convert.rs
- **Integration**: video decoder routes to rav1d when `VideoConfig` codec is AV1
- `#[cfg(feature = "av1")]` gated

### 5. Decoder routing update

#### `codec/video/decoder.rs`
- Match on `VideoConfig::codec`:
  - `VideoCodec::H264(_)` → openh264 decoder
  - `VideoCodec::AV1(_)` → rav1d decoder (if feature enabled, else error)
- Same shared output path: YUV→RGBA via convert.rs, viewport scaling

### 6. Feature forwarding

#### `iroh-live/Cargo.toml`
```toml
[features]
vaapi = ["moq-media/vaapi"]
videotoolbox = ["moq-media/videotoolbox"]
av1 = ["moq-media/av1"]
```

## Testing

### 7. Unit tests

#### 7a. Backend selection tests (`test_backend_select.rs`)
- **Default backend**: with no HW features enabled, encoder selects openh264. Verify via log output or backend type accessor.
- **Feature-gate compilation**: `cargo check` with each feature individually (`vaapi`, `videotoolbox`, `av1`) — must compile without errors.
- **All features combined**: `cargo check --features vaapi,videotoolbox,av1` — no conflicts.
- **No features**: `cargo check` with zero optional features — openh264-only, must compile and work.
- **Candidate ordering**: verify backend selector tries HW first, then software. Mock a failing HW init → verify fallback to openh264.
- **AV1 codec routing**: when `VideoConfig` specifies AV1, decoder routes to rav1d (not openh264). When H.264, routes to openh264.
- **AV1 without feature**: if `av1` feature is off, requesting AV1 codec returns a clear error (not a panic or silent failure).

#### 7b. AV1 encoder tests (`test_rav1e.rs`) — `#[cfg(feature = "av1")]`
- **Encode single frame**: 320×240 RGBA → YUV → rav1e encode → produces valid AV1 OBU packet.
- **Encode sequence**: 30 frames → keyframe emitted at expected intervals.
- **Extradata**: first encode produces valid AV1 sequence header OBU.
- **Bitrate control**: verify rav1e respects target bitrate (output size correlates with configured rate).
- **Speed preset**: verify speed 10 (fastest) is used for real-time.
- **Backend trait compliance**: verify `VideoEncoderBackend` trait methods work (encode, flush, extradata, set_bitrate).
- **Multiple resolutions**: 180p, 360p, 720p all encode successfully.

#### 7c. AV1 decoder tests (`test_rav1d.rs`) — `#[cfg(feature = "av1")]`
- **Encode→Decode roundtrip**: rav1e encode 10 frames → rav1d decode all → verify RGBA output dimensions match input.
- **Visual roundtrip**: encode solid blue frame, decode, verify output pixels are approximately blue.
- **Corrupt OBU**: feed random bytes → error, not crash.
- **Sequence header**: feed valid sequence header then frame OBU → successful decode.
- **Multiple frames**: decode 100 frames in sequence → no memory leaks or crashes.

#### 7d. VAAPI backend tests (`test_vaapi.rs`) — `#[cfg(feature = "vaapi")]`
- **Compilation test**: module compiles when feature is enabled.
- **Device probe test**: `VaapiBackend::probe()` returns `Ok(true)` on capable system, `Ok(false)` or `Err` otherwise (test should be `#[ignore]` on systems without VAAPI).
- **Encode roundtrip** (requires GPU, `#[ignore]` by default): encode 10 frames → decode with openh264 → verify valid output.
- **Fallback on init failure**: simulate missing `/dev/dri/renderD128` → backend init fails → encoder falls back to openh264 (no panic).
- **Backend trait compliance**: verify `VideoEncoderBackend` trait methods work.

#### 7e. VideoToolbox backend tests (`test_vtb.rs`) — `#[cfg(feature = "videotoolbox")]`
- **Compilation test**: module compiles on macOS when feature is enabled.
- **Session creation** (requires macOS HW, `#[ignore]` by default): `VTCompressionSession` creates successfully.
- **Encode roundtrip** (requires macOS HW, `#[ignore]`): encode 10 frames → decode with openh264 → verify valid output.
- **avcC extraction**: verify VideoToolbox format description provides valid SPS/PPS for avcC.
- **Fallback on init failure**: if VideoToolbox unavailable → encoder falls back to openh264.
- **Backend trait compliance**: verify `VideoEncoderBackend` trait methods work.

### 8. Integration tests

#### 8a. Cross-codec decode test (`test_cross_codec.rs`)
- **H.264→openh264**: encode with openh264, decode with openh264 → works.
- **AV1→rav1d**: encode with rav1e, decode with rav1d → works.
- **VAAPI H.264→openh264**: encode with VAAPI, decode with openh264 → interop works (VAAPI produces standard H.264).
- **VTB H.264→openh264**: encode with VideoToolbox, decode with openh264 → interop works.
- These tests verify that HW-encoded streams are decodable by the software decoder (critical for heterogeneous deployments).

#### 8b. Feature-gate isolation tests
- Build with `--features av1` only → no VAAPI/VTB code compiled (verify via binary size or symbol check).
- Build with `--features vaapi` only → no AV1 code compiled.
- Build with no features → only openh264, minimal binary.

#### 8c. Dynamic backend switching test
- Start encoder with HW backend, simulate HW failure mid-stream → verify graceful fallback (if supported) or clean error.
- Verify encoder recovers and continues with software backend.

## Verification (manual)

1. Manual: verify VAAPI is selected on Linux with capable GPU (check log)
2. Manual: verify VideoToolbox is selected on macOS (check log)
3. Manual: AV1 encode → decode roundtrip works with live camera
4. Manual: fallback to openh264 when HW not available (disable GPU, check log)
5. Manual: cross-platform call — VAAPI encoder on Linux → openh264 decoder on macOS → works
