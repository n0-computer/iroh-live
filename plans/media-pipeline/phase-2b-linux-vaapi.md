# Phase 2b-Linux: VAAPI H.264 Encoder

**Status: Pending**

## Context

Linux provides hardware-accelerated H.264 encoding via VAAPI (Video Acceleration API). The `cros-codecs` crate (v0.0.6, maintained by ChromeOS/Google) provides a production-quality Rust stateless encoder built on `cros-libva`. This adds a `VaapiEncoder` behind the `vaapi` feature flag, implementing `VideoEncoder` + `VideoEncoderInner`.

## Dependencies

```toml
# moq-media/Cargo.toml
[target.'cfg(target_os = "linux")'.dependencies]
cros-codecs = { version = "0.0.6", optional = true, features = ["vaapi"] }

[features]
vaapi = ["dep:cros-codecs"]
```

System deps: `libva-dev` (Ubuntu/Debian) or `libva-devel` (Fedora) + a VA-API driver (mesa-va-drivers for AMD/Intel, nvidia-vaapi-driver for NVIDIA).

## cros-codecs Encoder API

The cros-codecs stateless encoder has this flow:

```rust
// 1. Open VAAPI display
let display = libva::Display::open()?;

// 2. Configure
let config = EncoderConfig {
    profile: Profile::Baseline,
    resolution: Resolution { width, height },
    initial_tunings: Tunings {
        rate_control: RateControl::ConstantBitrate(bitrate),
        framerate,
        ..Default::default()
    },
    ..Default::default()
};

// 3. Create encoder
let mut encoder = StatelessEncoder::<H264, _, _>::new_vaapi(
    display, config, fourcc, coded_size, low_power, BlockingMode::Blocking,
)?;

// 4. Encode loop
encoder.encode(FrameMetadata { ..  }, handle)?;  // submit frame
while let Some(coded) = encoder.poll()? {         // retrieve output
    // coded.bitstream contains Annex B NALs
    // coded.metadata has timestamp, keyframe info
}

// 5. Drain at end
encoder.drain()?;
while let Some(coded) = encoder.poll()? { ... }
```

Output is `CodedBitstreamBuffer` containing Annex B formatted NALs.

## Implementation: `moq-media/src/codec/video/vaapi_enc.rs`

```rust
use cros_codecs::encoder::stateless::StatelessEncoder;
use cros_codecs::encoder::{VideoEncoder as CrosVideoEncoder, FrameMetadata, Tunings, RateControl};
use cros_codecs::encoder::stateless::h264::H264;
use cros_codecs::codec::h264::parser::Profile;
use cros_codecs::{BlockingMode, Fourcc, Resolution};

#[derive(derive_more::Debug)]
pub struct VaapiEncoder {
    #[debug(skip)]
    encoder: StatelessEncoder<H264, ...>,  // exact generic params TBD during impl
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u64,
    frame_count: u64,
    avcc: Option<Vec<u8>>,
    packet_buf: Vec<hang::Frame>,
}
```

**Constructor:** `Display::open()` → build `EncoderConfig` with Baseline profile, CBR rate control, framerate → `StatelessEncoder::new_vaapi(display, config, NV12 fourcc, coded_size, false, BlockingMode::Blocking)`. Same bitrate formula as VTB: `(pixels * 0.07 * framerate_factor).round()`. Return `Err` if VAAPI device not found or H.264 encoding not supported.

**push_frame():**
1. `pixel_format_to_yuv420()` — convert RGBA/BGRA → YUV420 (I420)
2. Pack YUV data into a format cros-codecs accepts (NV12 surface or raw handle — exact mechanism TBD based on what `StatelessEncoder::encode()` accepts as its `Handle` type)
3. `encoder.encode(metadata, handle)` — submit frame
4. `while let Some(coded) = encoder.poll()` — drain any ready output:
   a. `coded.bitstream` contains Annex B NALs
   b. `annex_b_to_length_prefixed()` to convert to length-prefixed format
   c. On first keyframe: `parse_annex_b()` → `extract_sps_pps()` → `build_avcc()`
   d. Build `hang::Frame { payload, timestamp, keyframe }`, push to `packet_buf`

**pop_packet():** Same FIFO pattern as VtbEncoder.

**config():** `VideoCodec::H264(...)`, avcC description, dimensions, framerate. Same structure as VtbEncoder.

**Drop:** `encoder.drain()` + poll remaining, then drop.

### Key details

- **NAL format:** cros-codecs outputs Annex B NALs → convert via `annex_b_to_length_prefixed()` (same as openh264 SW path).
- **Keyframe detection:** Check NAL types in Annex B output — IDR (type 5) = keyframe.
- **Device detection:** `libva::Display::open()` probes `/dev/dri/renderD128` (and other render nodes). Fails gracefully if no VAAPI device available.
- **Blocking mode:** Use `BlockingMode::Blocking` for simplicity — `poll()` returns completed frames synchronously. Matches our synchronous `push_frame()`/`pop_packet()` API.
- **NV12 vs I420:** cros-codecs typically expects NV12 surfaces. May need I420→NV12 conversion, or cros-codecs may accept I420. Will be determined during implementation.

### Differences from VtbEncoder

| Aspect | VtbEncoder | VaapiEncoder |
|--------|-----------|-------------|
| NAL format | Length-prefixed (native) | Annex B → convert to length-prefixed |
| SPS/PPS extraction | From CMFormatDescription | From Annex B bitstream |
| Callback model | Async C callback + `Arc<Mutex<_>>` | Synchronous `poll()` loop |
| Surface management | CVPixelBufferPool | cros-codecs manages VAAPI surfaces |
| Thread safety | Manual `unsafe impl Send` | Likely `Send` natively |

## Key files

- `moq-media/src/codec/video/vaapi_enc.rs` — New: main implementation
- `moq-media/src/codec/video/vtb_enc.rs` — Reference: VtbEncoder pattern (closest analog)
- `moq-media/src/codec/video/encoder.rs` — Reference: H264Encoder (SW) for trait shape
- `moq-media/src/codec/video/util/convert.rs` — Reuse: `pixel_format_to_yuv420()`
- `moq-media/src/codec/video/util/annexb.rs` — Reuse: `annex_b_to_length_prefixed()`, `parse_annex_b()`, `extract_sps_pps()`, `build_avcc()`
- `moq-media/src/codec/video.rs` — Add feature-gated `mod vaapi_enc` + `pub use`
- `moq-media/src/av.rs` — Add `VaapiH264` variant to `VideoCodec` enum
- `moq-media/Cargo.toml` — Add cros-codecs dependency
- `iroh-live/Cargo.toml` — Forward `vaapi` feature
- `iroh-live/examples/publish.rs` — Add `--codec vaapi-h264` arm
- `iroh-live/examples/rooms.rs` — Add `--codec vaapi-h264` arm
- `.github/workflows/hw-accel.yml` — Add linux-vaapi job

## Implementation Steps

### Step 1: Dependencies + feature flag
- Add `cros-codecs` to `moq-media/Cargo.toml` (linux-only, optional)
- Add `vaapi` feature to `moq-media/Cargo.toml`
- Forward `vaapi` feature in `iroh-live/Cargo.toml`
- Verify: `cargo check -p moq-media --features vaapi` compiles on Linux

### Step 2: VaapiEncoder struct + constructor
- Create `moq-media/src/codec/video/vaapi_enc.rs`
- Implement `VaapiEncoder::new(width, height, framerate)` using cros-codecs API
- Add to `video.rs`: `#[cfg(all(target_os = "linux", feature = "vaapi"))] mod vaapi_enc;` + pub use
- Implement `VideoEncoder::with_preset()`

### Step 3: Encode pipeline
- Implement `VideoEncoderInner::push_frame()` — YUV conversion, surface upload, encode, poll output
- Implement `VideoEncoderInner::pop_packet()` — FIFO drain
- Implement `VideoEncoderInner::config()` — same pattern as VtbEncoder
- Implement `VideoEncoderInner::name()` — `"h264-vaapi"`
- Handle Annex B → length-prefixed conversion + avcC extraction on first keyframe

### Step 4: av.rs + examples
- Add `VaapiH264` variant to `VideoCodec` enum in `av.rs` with `#[strum(serialize = "vaapi-h264")]`
- Gate with `#[cfg(all(target_os = "linux", feature = "vaapi"))]`
- Add match arms in `publish.rs` and `rooms.rs` examples (mirroring VtbH264 pattern)

### Step 5: Tests
- `vaapi_encode_basic` — push 30 frames, verify packets
- `vaapi_encode_decode_roundtrip` — VAAPI encode → openh264 decode, verify dimensions
- `vaapi_encode_keyframe_interval` — verify keyframes at expected intervals
- `vaapi_timestamps_increase` — verify monotonic timestamps
- `vaapi_config_fields` — verify config struct populated correctly
- All HW tests `#[ignore]`d (require VAAPI GPU)

### Step 6: CI
- Add `linux-vaapi` job to `.github/workflows/hw-accel.yml`
- Install `libva-dev`, `vainfo`, `mesa-va-drivers`
- `cargo check` + `cargo clippy` (unconditional)
- `cargo test --ignored || echo "..."` (graceful fallback if no GPU)
- Add linux entry to `compile-check` matrix

## Tests

### Unit tests (`moq-media/src/codec/video/vaapi_enc.rs`)
- `#[test] #[ignore] fn vaapi_encode_basic()` — push 30 frames, verify packets produced
- `#[test] #[ignore] fn vaapi_encode_decode_roundtrip()` — VAAPI encode → openh264 decode, verify dimensions + color
- `#[test] #[ignore] fn vaapi_encode_keyframe_interval()` — verify keyframes at expected intervals
- `#[test] #[ignore] fn vaapi_timestamps_increase()` — verify monotonic timestamps
- `#[test] #[ignore] fn vaapi_config_fields()` — verify config struct populated correctly

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

Note: GitHub Actions ubuntu runners typically lack GPU/VAAPI. The `|| echo` fallback prevents CI failure when HW is absent. Compilation + clippy checks run unconditionally.

## Verification

1. `cargo check -p moq-media --features vaapi` — compiles on Linux
2. `cargo check -p moq-media` — compiles without feature
3. `cargo clippy -p moq-media --features vaapi` — clean
4. `cargo test -p moq-media --features vaapi -- --ignored` — HW tests pass on Linux with VAAPI GPU
5. Run example with `--codec vaapi-h264`

## Open Questions (to resolve during implementation)

1. **Handle type:** What concrete type does `StatelessEncoder::encode()` accept for the frame handle? Likely a VAAPI `Surface` or GBM buffer. May need to create surfaces from raw YUV data.
2. **NV12 vs I420:** cros-codecs VAAPI backend may expect NV12. If so, need I420→NV12 interleave step (trivial: interleave U and V planes into UV plane).
3. **cros-codecs version:** The crate is at 0.0.6 on crates.io. API may have rough edges. If the public API is insufficient, consider using `cros-libva` directly (lower-level but more control).
4. **Low-power mode:** Some Intel GPUs support low-power encoding (uses fixed-function instead of shader-based). Pass `false` by default, could expose as option later.
