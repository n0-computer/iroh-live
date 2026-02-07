# Phase 1: Codec Swap — Remove ffmpeg

## Goal
Replace all ffmpeg usage with focused crates. After this phase, `cargo build` works without ffmpeg installed.

## Dependencies to add
- `openh264` — H.264 encode + decode (bundled C, compiles via `cc`)
- `unsafe-libopus` — Opus encode + decode (c2rust, no C compiler)
- `yuv` (yuvutils-rs) — YUV↔RGBA conversion (pure Rust, SIMD)
- `pic-scale` — image scaling (pure Rust)
- `rubato` — audio resampling (pure Rust, SIMD)

## Dependencies to remove
- `ffmpeg-next`
- `ffmpeg-sys-next` (+ all `[target.*.dependencies]` sections)
- `static` feature and all platform-specific ffmpeg features

## Steps

### 1. Cargo.toml & scaffolding
- Update `moq-media/Cargo.toml`: remove ffmpeg deps, add new deps
- Update `iroh-live/Cargo.toml`: remove `static` feature
- Create `moq-media/src/codec/` module tree with mod.rs declarations
- Files: `moq-media/Cargo.toml`, `iroh-live/Cargo.toml`, `moq-media/src/codec/mod.rs`

### 2. Video utilities

#### 2a. `codec/video/util/convert.rs` — YUV↔RGBA
- `rgba_to_yuv420(src: &[u8], w: u32, h: u32) -> YuvData`
  - `YuvData` holds Y/U/V planes + strides + dimensions
  - Uses `yuvutils-rs` `rgba_to_yuv420` function
  - Operates directly on source `Bytes` — no intermediate frame
- `yuv420_to_rgba(yuv: &DecodedYUV, w: u32, h: u32) -> Vec<u8>`
  - Converts openh264 `DecodedYUV` Y/U/V planes to packed RGBA
  - Single allocation, single pass
- Also support BGRA input (detect from `PixelFormat` enum)

#### 2b. `codec/video/util/scale.rs` — bilinear scaling
- Wraps `pic-scale` with same lazy-context pattern as current `Rescaler`
- Interface: `Rescaler::new(target_format, optional_dimensions)`
- `process(rgba: &[u8], w: u32, h: u32) -> Vec<u8>` — scales if needed, pass-through if no-op
- `set_target_dimensions(w, h)` — update target

#### 2c. `codec/video/util/mjpg.rs` — MJPEG decode
- Replace full ffmpeg codec context with:
  ```rust
  image::load_from_memory_with_format(data, ImageFormat::Jpeg)?.to_rgba8()
  ```
- Convert `RgbaImage` → `VideoFrame` (wrap bytes + dimensions)
- No YUVJ fixup needed — image crate outputs RGBA directly

#### 2d. `codec/video/util/annexb.rs` — NAL framing helpers
- `parse_annex_b(data: &[u8]) -> Vec<&[u8]>` — split on start codes (00 00 00 01 / 00 00 01)
- `extract_sps_pps(nals: &[&[u8]]) -> Option<(Vec<u8>, Vec<u8>)>` — find SPS (type 7) + PPS (type 8)
- `build_avcc(sps: &[u8], pps: &[u8], profile: u8, compat: u8, level: u8) -> Vec<u8>` — pack avcC box:
  `[1, profile, compat, level, 0xFF, 0xE1, sps_len(2be), sps..., 1, pps_len(2be), pps...]`
- ~30 lines total

### 3. Audio resampling

#### `codec/resample.rs`
- Wraps `rubato::SincFixedIn` or `rubato::SincFixedOut`
- `Resampler::new(from_rate, to_rate, channels) -> Self`
- `process(input: &[f32]) -> Vec<f32>` — resample + return
- Channel conversion: stereo→mono (average L+R), mono→stereo (duplicate)
- Lazy creation: only create rubato context when rates actually differ

### 4. Audio encoder

#### `codec/audio/encoder.rs` — Opus via unsafe-libopus
- Create `opus_encoder_create(48000, channels, APPLICATION_VOIP)`
- Configure:
  - `OPUS_SET_BITRATE(bitrate)` — 128kbps (HQ) or 32kbps (LQ)
  - `OPUS_SET_INBAND_FEC(1)` — enable FEC from day 1
  - `OPUS_SET_PACKET_LOSS_PERC(10)` — assume 10% loss for FEC threshold
  - `OPUS_SET_DTX(1)` — enable discontinuous transmission
- **Internal sample buffer**: accept arbitrary `&[f32]` via `push_samples()`, buffer to 960-sample boundaries (20ms at 48kHz), call `opus_encode_float()` when full
- **Extradata**: manually construct 19-byte Opus ID header:
  `"OpusHead"(8) + version(1:0x01) + channels(1) + pre_skip(2:LE) + sample_rate(4:LE) + gain(2:LE:0) + mapping_family(1:0)`
- Implement `AudioEncoder` + `AudioEncoderInner` traits from `av.rs`
- `pop_packet()` returns `hang::Frame { payload, timestamp, keyframe: true }`

### 5. Audio decoder

#### `codec/audio/decoder.rs` — Opus via unsafe-libopus
- Create `opus_decoder_create(sample_rate, channels)`
- `push_packet()`: store packet for decode
- `pop_samples()`: call `opus_decode_float(data, len, output, frame_size, decode_fec=0)`
- **PLC support**: when packet is missing, call `opus_decode_float(null, 0, output, frame_size, 0)` for comfort noise
- **Lazy resampler**: create `rubato` resampler after first decode if target sample rate differs from 48kHz
- Return `&[f32]` interleaved samples
- Implement `AudioDecoder` trait from `av.rs`

### 6. Video encoder

#### `codec/video/openh264_enc.rs` — openh264 backend
- `openh264::Encoder::with_config(EncoderConfig)`:
  - `.bitrate(heuristic)` — `pixels * 0.07 * (30 + (fps-30)/2)`
  - `.max_frame_rate(fps)`
  - `.rate_control_mode(Quality)` or `Bitrate`
  - `.profile(Baseline)`
  - `.intra_frame_period(fps)` — keyframe every 1 second
  - `.usage_type(CameraVideoRealTime)` for low latency
- Input: `&YuvData` (from convert.rs) — openh264 accepts I420 YUV
- Output: iterate `EncodedBitStream` layers → collect NAL bytes
- **avcC**: on first encode, extract SPS/PPS from NAL output via annexb.rs, build avcC for `VideoConfig::description`
- Implement internal `VideoEncoderBackend` trait

#### `codec/video/encoder.rs` — public VideoEncoder
- Backend selection: try candidates in order, log selection, fallback
- Shared layer: RGBA→YUV via convert.rs (one pass, no intermediate frame)
- Timestamp management, `hang::Frame` packaging
- Implement `VideoEncoder` + `VideoEncoderInner` traits from `av.rs`

### 7. Video decoder

#### `codec/video/openh264_dec.rs` — openh264 backend
- `openh264::Decoder::new()`
- `decode(packet_data)` → `Option<DecodedYUV>`
- Returns Y/U/V plane accessors
- Implement internal decoder backend trait

#### `codec/video/decoder.rs` — public VideoDecoder
- Route to openh264 based on `VideoConfig` codec field (H.264)
- YUV→RGBA via convert.rs (single pass into Vec)
- Viewport scaling via scale.rs
- **Fix viewport bug**: only consume `viewport_changed` after successful decode
- **Fix BufferTooSmall**: propagate as error
- Frame delay: `StreamClock` with EMA smoothing (α=0.1)
- Implement `VideoDecoder` trait from `av.rs`

### 8. Integration & wiring

- `codec/mod.rs`: `DefaultDecoders` struct implementing `Decoders` trait
- Update `moq-media/src/lib.rs`: `pub mod codec` replaces `pub mod ffmpeg`
- Update `capture.rs`: import `codec::video::util::MjpgDecoder`
- Update `subscribe.rs`: import `codec::video::util::Rescaler`
- Remove `ffmpeg/` directory entirely
- Remove `ffmpeg_log_init()` calls

## Files modified (outside codec/)

| File | Change |
|---|---|
| `moq-media/Cargo.toml` | Remove ffmpeg deps, add new deps |
| `moq-media/src/lib.rs` | `pub mod codec` replaces `pub mod ffmpeg` |
| `moq-media/src/capture.rs` | Import MjpgDecoder from codec |
| `moq-media/src/subscribe.rs` | Import Rescaler from codec |
| `iroh-live/Cargo.toml` | Remove `static` feature |

## Testing

### 9. Unit tests — `moq-media/src/codec/tests/`

#### 9a. YUV conversion tests (`test_convert.rs`)
- **Roundtrip RGBA→YUV→RGBA**: encode a known pixel pattern (gradient, checkerboard, solid color) to YUV420, convert back to RGBA. Assert pixel values within ±2 of originals (lossy conversion tolerance).
- **BGRA input**: same roundtrip with BGRA pixel format, verify channel ordering is correct.
- **Odd dimensions**: test 7×5, 1×1, 3×3 — YUV420 requires even chroma planes, verify correct padding/rounding.
- **Large frame**: 1920×1080 full-size frame, verify no panics or OOB.
- **Empty/zero-size**: 0×0 should return error, not panic.
- **Known-value test**: single pure-red pixel RGBA `[255,0,0,255]` → verify Y/U/V match BT.601 expected values (Y≈81, U≈90, V≈240).

#### 9b. Scaling tests (`test_scale.rs`)
- **Identity scaling**: 640×480 → 640×480 should be pass-through (no allocation).
- **Downscale**: 1920×1080 → 640×360. Output dimensions correct. Output is valid RGBA.
- **Aspect ratio preservation**: 1920×1080 into 640×640 viewport → should produce 640×360 (fit within bounds, no stretch).
- **No upscale**: 320×240 into 1920×1080 viewport → should remain 320×240 (never upscale policy).
- **Minimum size**: 2×2 → 1×1 should not panic.
- **Visual sanity**: downscale a 4×4 checkerboard, verify output is not blank.

#### 9c. MJPEG decode tests (`test_mjpg.rs`)
- **Valid JPEG**: encode a known image to JPEG bytes, decode via `MjpgDecoder`, verify dimensions and pixel values match.
- **Corrupt JPEG**: truncated file, random bytes, empty bytes — should return `Err`, not panic.
- **Grayscale JPEG**: verify output is still RGBA (expanded to RGBA with A=255).
- **Various sizes**: 1×1, 16×16, 1920×1080 JPEG decode works.

#### 9d. Annex B / avcC tests (`test_annexb.rs`)
- **Parse start codes**: `[0,0,0,1,0x67,...]` → one NAL. Multiple NALs with both 3-byte and 4-byte start codes.
- **Extract SPS/PPS**: known H.264 stream start with SPS (type 7) + PPS (type 8) → verify extraction.
- **Build avcC**: given known SPS/PPS, build avcC, verify byte layout matches ISO 14496-15 spec.
- **No SPS/PPS**: stream without SPS/PPS → `extract_sps_pps` returns `None`.
- **Roundtrip**: build avcC → parse back → verify fields match.
- **Multiple SPS**: stream with duplicate SPS NALs → picks first valid one.

#### 9e. Audio resampling tests (`test_resample.rs`)
- **Identity resample**: 48kHz → 48kHz should return input unchanged (no processing).
- **440Hz sine roundtrip**: generate 440Hz sine at 48kHz, resample to 16kHz, resample back to 48kHz. Verify frequency content preserved (correlation > 0.95 with original).
- **Stereo→Mono→Stereo**: verify mono is average of L+R, stereo duplicate is correct.
- **Empty input**: zero samples in → zero samples out, no panic.
- **Non-standard rates**: 44100 → 48000 (common case), verify output length = `input_len * 48000 / 44100 ± 1`.
- **Large buffer**: 10 seconds of audio, verify no drift or growing latency.

#### 9f. Opus encoder tests (`test_opus_enc.rs`)
- **Encode silence**: 960 samples of silence → should produce valid packet (small size due to DTX if enabled, but still valid).
- **Encode sine wave**: 440Hz sine, 20ms frame → packet size is reasonable (50-200 bytes at 128kbps).
- **Sample buffer accumulation**: push 480 samples (10ms) twice → should produce one 960-sample frame output.
- **Extradata format**: verify 19-byte OpusHead header structure: magic, version, channels, pre-skip, sample rate.
- **HQ vs LQ preset**: HQ (128kbps) packets should be larger than LQ (32kbps) packets for same input.
- **Mono and Stereo**: both channel configurations encode without error.
- **Trait compliance**: verify `AudioEncoder` and `AudioEncoderInner` trait methods all work.

#### 9g. Opus decoder tests (`test_opus_dec.rs`)
- **Encode→Decode roundtrip**: encode 1 second of 440Hz sine, decode all packets, verify output resembles input (correlation > 0.9, allowing for codec loss).
- **Multi-frame roundtrip**: encode 5 seconds of speech-like signal (varying amplitude), decode, verify no silence gaps.
- **PLC (packet loss concealment)**: skip every 5th packet, call PLC, verify output has no silence holes — PLC should produce comfort noise.
- **Wrong sample rate**: try decoding packet at wrong sample rate → should error gracefully.
- **Corrupt packet**: random bytes as input → should return error, not panic/crash.
- **Lazy resampler**: decoder at 16kHz target, verify resampler is created and output is correct rate.

#### 9h. H.264 encoder tests (`test_h264_enc.rs`)
- **Encode single frame**: 320×240 RGBA → YUV → encode → verify output contains NAL units.
- **Encode sequence**: 30 frames → verify at least one IDR (keyframe) is emitted.
- **Keyframe interval**: with `intra_frame_period(30)`, verify IDR at frame 0, frame 30, etc.
- **avcC generation**: after first encode, `extradata()` returns valid avcC with SPS/PPS.
- **Bitrate heuristic**: verify calculated bitrate matches `pixels * 0.07 * framerate_factor` formula.
- **Multiple presets**: encode at 180p, 360p, 720p, 1080p — all produce valid output.
- **Backend trait compliance**: verify `VideoEncoderBackend` trait methods all work.

#### 9i. H.264 decoder tests (`test_h264_dec.rs`)
- **Encode→Decode roundtrip**: encode 10 frames, decode all, verify output RGBA frames have correct dimensions.
- **Visual roundtrip**: encode solid red frame, decode, verify output pixels are approximately red (within codec loss tolerance).
- **Viewport scaling**: decode 1920×1080, viewport set to 640×480 → output fits within 640×480.
- **Viewport change mid-stream**: change viewport after frame 5, verify subsequent frames use new viewport.
- **Corrupt NAL**: feed random bytes → error, not crash.
- **Incomplete frame**: feed partial NAL → handled gracefully (returns None or error).
- **Buffer too small**: trigger and verify it propagates as error (not silently swallowed).

### 10. Integration tests — `moq-media/tests/`

#### 10a. Full pipeline test (`test_pipeline.rs`)
- **Video end-to-end**: RGBA frames → VideoEncoder → bytes → VideoDecoder → RGBA frames. Verify 30 frames roundtrip, correct dimensions, no crashes.
- **Audio end-to-end**: f32 samples → AudioEncoder → bytes → AudioDecoder → f32 samples. Verify 1 second roundtrip, output resembles input.
- **Mixed pipeline**: simultaneous video + audio encode/decode, verify no cross-contamination.
- **Decoders factory**: `DefaultDecoders` creates correct encoder/decoder types based on config.

#### 10b. Trait compatibility test (`test_traits.rs`)
- Verify `DefaultDecoders` implements `Decoders` trait.
- Verify all encoders/decoders implement their respective `av.rs` traits.
- Verify `VideoFrame`, `DecodedFrame`, `AudioFormat` types work correctly with new codec module.

#### 10c. No-ffmpeg build test
- `cargo build` without any ffmpeg system library → must succeed.
- `cargo test` → all tests pass.
- Verify no linker references to ffmpeg/avcodec/avutil symbols.

## Verification (manual)

1. Manual: camera → H.264 encode → transmit → decode → display
2. Manual: audio capture → Opus encode → transmit → decode → playback
3. Manual: screen capture works
4. Manual: video presets 180p–1080p work
5. No ffmpeg system dependency required
