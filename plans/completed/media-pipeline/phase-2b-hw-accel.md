# Phase 2b: Hardware-Accelerated H.264 Encoding

**Status: In Progress**

## Goal

Add optional hardware-accelerated H.264 encoding on all three platforms, behind feature flags. Each platform encoder implements the same `VideoEncoder`/`VideoEncoderInner` traits as the existing software encoders. If HW init fails, callers fall back to `H264Encoder` (software).

| Platform | API | Rust crate | Feature flag | Plan |
|----------|-----|------------|--------------|------|
| macOS | VideoToolbox | `objc2-video-toolbox` 0.3 | `videotoolbox` | phase-2b-macos-videotoolbox.md |
| Linux | VAAPI (libva) | `cros-codecs` (ChromeOS/Google) | `vaapi` | phase-2b-linux-vaapi.md |
| Windows | Media Foundation | `windows` (Microsoft) | `media-foundation` | phase-2b-windows-media-foundation.md |

**Implementation order:** macOS first (dev machine), then Linux, then Windows. Each is an independent commit.

## Shared across all platforms

**Reused utilities:**
- `pixel_format_to_yuv420()` from `util/convert.rs` — RGBA/BGRA → YUV420
- `build_avcc()`, `extract_sps_pps()` from `util/annexb.rs` — avcC construction
- `annex_b_to_length_prefixed()` from `util/annexb.rs` — NAL format conversion (VAAPI + MF)
- Same bitrate formula as H264Encoder: `(pixels * 0.07 * framerate_factor).round()`
- Same `VideoConfig` structure: `VideoCodec::H264(...)`, avcC description, dimensions, framerate

**Shared changes (done with the first platform implemented):**

### `moq-media/src/codec/video/mod.rs` — Feature-gated exports
```rust
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
mod vtb_enc;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
pub use vtb_enc::VtbEncoder;

#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi_enc;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub use vaapi_enc::VaapiEncoder;

#[cfg(all(target_os = "windows", feature = "media-foundation"))]
mod mf_enc;
#[cfg(all(target_os = "windows", feature = "media-foundation"))]
pub use mf_enc::MfEncoder;
```

### `moq-media/src/lib.rs` — Conditional re-exports
Same `#[cfg(...)]` pattern for each encoder.

### `iroh-live/Cargo.toml` — Feature forwarding
```toml
[features]
videotoolbox = ["moq-media/videotoolbox"]
vaapi = ["moq-media/vaapi"]
media-foundation = ["moq-media/media-foundation"]
```

### Example updates (`iroh-live/examples/`)
Add `--codec` option:
- `h264` → `H264Encoder` (software, default, all platforms)
- `vtb-h264` → `VtbEncoder` (macOS)
- `vaapi-h264` → `VaapiEncoder` (Linux)
- `mf-h264` → `MfEncoder` (Windows)
- `av1` → `Av1Encoder` (software, all platforms)

### Test pattern (per encoder file)
- `#[test] #[ignore] fn hw_encode_basic()` — push 30 frames, verify packets
- `#[test] #[ignore] fn hw_encode_valid_h264()` — encode → decode with openh264, verify dimensions match
- `#[test] #[ignore] fn hw_decode_sw_encoded()` — decode SW-encoded (openh264) H.264 with HW decoder, verify compat
- `#[test] #[ignore] fn hw_encode_keyframe_interval()` — verify keyframes at expected intervals
- `#[test] fn hw_encoder_fallback()` — graceful error when HW unavailable (runs anywhere)

### CI: GitHub Actions (`.github/workflows/hw-accel.yml`)

A new workflow that runs HW encode/decode roundtrip tests on platform-specific runners:

```yaml
name: HW Acceleration Tests
on: [push, pull_request]
jobs:
  macos-videotoolbox:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo clippy -p moq-media --features videotoolbox -- -D warnings
      - run: cargo test -p moq-media --features videotoolbox -- --ignored
      # Cross-compat: HW-encoded → SW-decoded and vice versa
      - run: cargo test -p moq-media --features videotoolbox -- vtb_cross_compat --ignored

  linux-vaapi:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: sudo apt-get install -y libva-dev vainfo mesa-va-drivers
      - run: cargo clippy -p moq-media --features vaapi -- -D warnings
      - run: cargo test -p moq-media --features vaapi -- --ignored || echo "VAAPI not available on runner, skipping HW tests"

  windows-media-foundation:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo clippy -p moq-media --features media-foundation -- -D warnings
      - run: cargo test -p moq-media --features media-foundation -- --ignored

  # Compilation check on all platforms (no HW required)
  compile-check:
    strategy:
      matrix:
        include:
          - os: macos-latest
            features: videotoolbox
          - os: ubuntu-latest
            features: vaapi
          - os: windows-latest
            features: media-foundation
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo check -p moq-media --features ${{ matrix.features }}
      - run: cargo check -p moq-media  # also without feature
```

Key test categories for CI:
1. **Compilation** — feature compiles on target platform, and without feature on all platforms
2. **HW encode roundtrip** — encode with HW, decode with openh264 (SW), verify frames match
3. **Cross-compat** — encode with openh264 (SW), verify HW encoder produces decodable output
4. **Fallback** — runs on all platforms, verifies graceful error when HW unavailable
