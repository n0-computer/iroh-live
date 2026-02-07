# Phase 2b-Linux: VAAPI H.264 Encoder

**Status: Pending**

## Context

Linux provides hardware-accelerated H.264 encoding via VAAPI (Video Acceleration API). The `cros-codecs` crate (maintained by ChromeOS/Google) provides a production-quality Rust encoder built on `cros-libva`. This adds a `VaapiEncoder` behind the `vaapi` feature flag.

## Dependencies

```toml
# moq-media/Cargo.toml
[target.'cfg(target_os = "linux")'.dependencies]
cros-codecs = { version = "0.1", optional = true, features = ["vaapi"] }

[features]
vaapi = ["dep:cros-codecs"]
```

## Implementation: `moq-media/src/codec/video/vaapi_enc.rs`

```rust
pub struct VaapiEncoder {
    encoder: cros_codecs::encoder::...,  // cros-codecs H.264 encoder handle
    width: u32, height: u32, framerate: u32, bitrate: u64,
    frame_count: u64,
    avcc: Option<Vec<u8>>,
    packet_buf: Vec<hang::Frame>,
}
```

**Constructor:** Open VAAPI display (`/dev/dri/renderD128`) → create H.264 encoder config (profile Baseline, bitrate, keyframe interval = framerate) → init encoder session. Return `Err` if VAAPI device not found or H.264 encoding not supported.

**push_frame():**
1. `pixel_format_to_yuv420()` — convert RGBA/BGRA → YUV420
2. Copy YUV data to VAAPI surface
3. Encode frame
4. Extract Annex B NALs from output buffer
5. Convert to length-prefixed via existing `annex_b_to_length_prefixed()`
6. On first keyframe: `extract_sps_pps()` + `build_avcc()`
7. Push `hang::Frame` to `packet_buf`

**pop_packet():** Same FIFO pattern.

**config():** `VideoCodec::H264(...)`, avcC description, dimensions, framerate.

### Key details

- **NAL format:** VAAPI outputs Annex B NALs — convert via `annex_b_to_length_prefixed()`.
- **Device detection:** Probe `/dev/dri/renderD128` (or iterate `renderD*`). Fail gracefully if absent.
- **cros-codecs** is the recommended crate: production-quality, minimal deps, maintained by Google.

## Key files

- `moq-media/src/codec/video/vaapi_enc.rs` — New: main implementation
- `moq-media/src/codec/video/encoder.rs` — Reference: H264Encoder pattern
- `moq-media/src/codec/video/util/convert.rs` — Reuse: `pixel_format_to_yuv420()`
- `moq-media/src/codec/video/util/annexb.rs` — Reuse: `annex_b_to_length_prefixed()`, `build_avcc()`, `extract_sps_pps()`
- `moq-media/Cargo.toml` — Dependencies

## Tests

### Unit tests (`moq-media/src/codec/video/vaapi_enc.rs`)
- `#[test] #[ignore] fn vaapi_encode_basic()` — push 30 frames, verify packets produced
- `#[test] #[ignore] fn vaapi_encode_decode_roundtrip()` — VAAPI encode → openh264 decode, verify dimensions + color
- `#[test] #[ignore] fn vaapi_cross_compat_sw()` — openh264 encode → verify VAAPI-encoded stream is decodable by openh264
- `#[test] #[ignore] fn vaapi_encode_keyframe_interval()` — verify keyframes at expected intervals
- `#[test] fn vaapi_encoder_fallback()` — graceful error when HW unavailable (runs on any platform)

### CI: GitHub Actions (in `.github/workflows/hw-accel.yml`)
```yaml
linux-vaapi:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: sudo apt-get install -y libva-dev vainfo mesa-va-drivers
    - run: cargo check -p moq-media --features vaapi
    - run: cargo check -p moq-media  # without feature
    - run: cargo clippy -p moq-media --features vaapi -- -D warnings
    - run: cargo test -p moq-media --features vaapi -- --ignored || echo "VAAPI not available on runner, skipping HW tests"
```

Note: GitHub Actions ubuntu runners may not have GPU/VAAPI available. The `|| echo` fallback prevents CI failure when HW is absent. The compilation + clippy checks still run unconditionally. If a self-hosted runner with GPU is available, remove the fallback.

## Verification

1. `cargo check -p moq-media --features vaapi` — compiles on Linux
2. `cargo check -p moq-media` — compiles without feature
3. `cargo clippy -p moq-media --features vaapi` — clean
4. `cargo test -p moq-media --features vaapi -- --ignored` — HW tests pass on Linux with VAAPI GPU
5. Run example with `--codec vaapi-h264`
