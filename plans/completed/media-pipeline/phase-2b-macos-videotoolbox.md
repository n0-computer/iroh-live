# Phase 2b-macOS: VideoToolbox H.264 Encoder

**Status: Pending**

## Context

macOS provides hardware-accelerated H.264 encoding via Apple's VideoToolbox framework. This adds a `VtbEncoder` behind the `videotoolbox` feature flag, implementing `VideoEncoder` + `VideoEncoderInner`.

## Dependencies

```toml
# moq-media/Cargo.toml
[target.'cfg(target_os = "macos")'.dependencies]
objc2-video-toolbox = { version = "0.3", optional = true, features = ["VTCompressionSession"] }
objc2-core-media = { version = "0.3", optional = true }
objc2-core-video = { version = "0.3", optional = true }
objc2-core-foundation = { version = "0.3", optional = true }
objc2 = { version = "0.6", optional = true }

[features]
videotoolbox = ["dep:objc2-video-toolbox", "dep:objc2-core-media", "dep:objc2-core-video", "dep:objc2-core-foundation", "dep:objc2"]
```

## Implementation: `moq-media/src/codec/video/vtb_enc.rs`

```rust
pub struct VtbEncoder {
    session: *mut VTCompressionSession,
    width: u32, height: u32, framerate: u32, bitrate: u64,
    frame_count: u64,
    avcc: Option<Vec<u8>>,
    packet_buf: Vec<hang::Frame>,  // filled by callback
}
unsafe impl Send for VtbEncoder {}
```

**Constructor:** `VTCompressionSession::create()` with H.264 codec type, dimensions, output callback → set properties via `VTSessionSetProperty()`:
- `kVTCompressionPropertyKey_RealTime` = true
- `kVTCompressionPropertyKey_AverageBitRate` = calculated bitrate
- `kVTCompressionPropertyKey_MaxKeyFrameInterval` = framerate (1 keyframe/sec)
- `kVTCompressionPropertyKey_ProfileLevel` = Baseline AutoLevel
- `kVTCompressionPropertyKey_AllowFrameReordering` = false (no B-frames, lower latency)
→ `prepare_to_encode_frames()`

**Callback design:** C function pointer receiving `*mut c_void` refcon → `Arc<Mutex<Vec<hang::Frame>>>`. Extracts length-prefixed NALs from `CMSampleBuffer` → `CMBlockBuffer`. On first keyframe: extract SPS/PPS via `CMVideoFormatDescriptionGetH264ParameterSetAtIndex`, build avcC via existing `build_avcc()`. Pushes `hang::Frame` to the shared vec.

**push_frame():**
1. `pixel_format_to_yuv420()` — convert RGBA/BGRA → YUV420
2. Get CVPixelBuffer from `session.pixel_buffer_pool()` (pre-allocated, avoids per-frame alloc)
3. Lock buffer, copy Y/U/V planes (respecting row strides from `CVPixelBufferGetBytesPerRowOfPlane`), unlock
4. Create `CMTime::new(frame_count as i64, framerate as i32)`
5. `session.encode_frame(pixel_buffer, timestamp, duration, None, null, &mut info_flags)`
6. Lock shared mutex, drain callback results into `self.packet_buf`

**pop_packet():** Same FIFO pattern as H264Encoder.

**config():** `VideoCodec::H264(...)`, avcC description, dimensions, framerate.

**Drop:** `session.invalidate()` to release hardware resources.

### Key details

- **NAL format:** VTB outputs length-prefixed NALs (AVCC format) — same format hang expects. No Annex B conversion needed.
- **CVPixelBuffer format:** `kCVPixelFormatType_420YpCbCr8Planar` (3-plane I420) matches `pixel_format_to_yuv420()` output.
- **Thread safety:** Callback fires on VTB's internal thread. `Arc<Mutex<Vec<hang::Frame>>>` shared between encoder struct and callback.

## Key files

- `moq-media/src/codec/video/vtb_enc.rs` — New: main implementation
- `moq-media/src/codec/video/encoder.rs` — Reference: H264Encoder pattern
- `moq-media/src/codec/video/util/convert.rs` — Reuse: `pixel_format_to_yuv420()`
- `moq-media/src/codec/video/util/annexb.rs` — Reuse: `build_avcc()`
- `moq-media/Cargo.toml` — Dependencies

## Tests

### Unit tests (`moq-media/src/codec/video/vtb_enc.rs`)
- `#[test] #[ignore] fn vtb_encode_basic()` — push 30 frames, verify packets produced
- `#[test] #[ignore] fn vtb_encode_decode_roundtrip()` — VTB encode → openh264 decode, verify dimensions + color
- `#[test] #[ignore] fn vtb_decode_sw_encoded()` — openh264 encode → verify VTB-encoded avcC is compatible with openh264 decoder
- `#[test] #[ignore] fn vtb_encode_keyframe_interval()` — verify keyframes at expected intervals
- `#[test] fn vtb_encoder_fallback()` — graceful error when HW unavailable (runs on any platform)

### CI: GitHub Actions (in `.github/workflows/hw-accel.yml`)
```yaml
macos-videotoolbox:
  runs-on: macos-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo check -p moq-media --features videotoolbox
    - run: cargo check -p moq-media  # without feature
    - run: cargo clippy -p moq-media --features videotoolbox -- -D warnings
    - run: cargo test -p moq-media --features videotoolbox -- --ignored
```

macOS GitHub Actions runners have VideoToolbox available (Apple Silicon), so HW tests run directly.

Cross-compatibility tests verify:
- HW-encoded H.264 is decodable by openh264 (SW decoder)
- SW-encoded H.264 avcC format matches what HW encoder produces (compatible stream format)

## Verification

1. `cargo check -p moq-media --features videotoolbox` — compiles on macOS
2. `cargo check -p moq-media` — compiles without feature
3. `cargo clippy --workspace --all-features` — clean
4. `cargo test -p moq-media --features videotoolbox -- --ignored` — HW tests pass
5. Run example with `--codec vtb-h264` — video encodes using HW
