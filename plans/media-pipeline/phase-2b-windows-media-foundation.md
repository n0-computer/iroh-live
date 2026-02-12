# Phase 2b-Windows: Media Foundation H.264 Encoder

**Status: Pending**

## Context

Windows provides hardware-accelerated H.264 encoding via Media Foundation Transforms (MFT). The official Microsoft `windows` crate exposes the full MFT API. This adds an `MfEncoder` behind the `media-foundation` feature flag.

**Note:** MFT requires setting output type BEFORE input type — `IMFTransform::SetInputType` returns `MF_E_TRANSFORM_TYPE_NOT_SET` otherwise.

## Dependencies

```toml
# moq-media/Cargo.toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", optional = true, features = [
    "Win32_Media_MediaFoundation",
    "Win32_System_Com",
] }

[features]
media-foundation = ["dep:windows"]
```

## Implementation: `moq-media/src/codec/video/mf_enc.rs`

```rust
pub struct MfEncoder {
    transform: IMFTransform,
    width: u32, height: u32, framerate: u32, bitrate: u64,
    frame_count: u64,
    avcc: Option<Vec<u8>>,
    packet_buf: Vec<hang::Frame>,
}
```

**Constructor:**
1. `MFStartup()` — init Media Foundation
2. `MFTEnumEx()` — find H.264 encoder MFT (category `MFT_CATEGORY_VIDEO_ENCODER`)
3. `CoCreateInstance()` — create transform instance
4. Set output type FIRST: H.264, bitrate, profile Baseline
5. Set input type: NV12 or I420, dimensions, framerate
6. `ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING)`
7. Return `Err` if no H.264 MFT found

**push_frame():**
1. `pixel_format_to_yuv420()` — convert RGBA/BGRA → YUV420
2. Create `IMFMediaBuffer` from YUV data
3. Wrap in `IMFSample` with timestamp
4. `ProcessInput(0, sample, 0)`
5. `ProcessOutput(0, &mut output_buffer, &mut status)` — may need to loop
6. Extract Annex B NALs from output sample
7. Convert to length-prefixed via `annex_b_to_length_prefixed()`
8. On first keyframe: `extract_sps_pps()` + `build_avcc()`

**pop_packet():** Same FIFO pattern.

**config():** `VideoCodec::H264(...)`, avcC description, dimensions, framerate.

**Drop:** Release COM objects, `MFShutdown()`.

### Key details

- **NAL format:** MFT outputs Annex B NALs — convert via `annex_b_to_length_prefixed()`.
- **COM init:** Requires `CoInitializeEx` on the thread. Consider doing this in constructor.
- **Output type before input type** is critical — reversed order fails silently.

## Key files

- `moq-media/src/codec/video/mf_enc.rs` — New: main implementation
- `moq-media/src/codec/video/encoder.rs` — Reference: H264Encoder pattern
- `moq-media/src/codec/video/util/convert.rs` — Reuse: `pixel_format_to_yuv420()`
- `moq-media/src/codec/video/util/annexb.rs` — Reuse: `annex_b_to_length_prefixed()`, `build_avcc()`, `extract_sps_pps()`
- `moq-media/Cargo.toml` — Dependencies

## Tests

### Unit tests (`moq-media/src/codec/video/mf_enc.rs`)
- `#[test] #[ignore] fn mf_encode_basic()` — push 30 frames, verify packets produced
- `#[test] #[ignore] fn mf_encode_decode_roundtrip()` — MF encode → openh264 decode, verify dimensions + color
- `#[test] #[ignore] fn mf_cross_compat_sw()` — openh264 encode → verify MF-encoded stream is decodable by openh264
- `#[test] #[ignore] fn mf_encode_keyframe_interval()` — verify keyframes at expected intervals
- `#[test] fn mf_encoder_fallback()` — graceful error when HW unavailable (runs on any platform)

### CI: GitHub Actions (in `.github/workflows/hw-accel.yml`)
```yaml
windows-media-foundation:
  runs-on: windows-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo check -p moq-media --features media-foundation
    - run: cargo check -p moq-media  # without feature
    - run: cargo clippy -p moq-media --features media-foundation -- -D warnings
    - run: cargo test -p moq-media --features media-foundation -- --ignored
```

Windows GitHub Actions runners have Media Foundation available, so HW MFT tests run directly.

## Verification

1. `cargo check -p moq-media --features media-foundation` — compiles on Windows
2. `cargo check -p moq-media` — compiles without feature
3. `cargo clippy -p moq-media --features media-foundation` — clean
4. `cargo test -p moq-media --features media-foundation -- --ignored` — HW tests pass on Windows
5. Run example with `--codec mf-h264`
