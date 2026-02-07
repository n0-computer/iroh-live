# Media Pipeline Overhaul — Master Plan

## Goal

Replace ffmpeg with focused crates and build libwebrtc-grade media quality. Two parallel tracks:

- **Track A**: Codec replacement (remove ffmpeg dependency)
- **Track B**: Media resilience (jitter buffering, loss concealment, adaptive bitrate)

## Phase overview

| Phase | Track | Description | Status | Plan |
|-------|-------|-------------|--------|------|
| 1 | A | Codec swap — scaffolding, utilities, audio, video, integration | Pending | [phase-1-codec-swap.md](phase-1-codec-swap.md) |
| 2 | A | HW acceleration (VAAPI, VideoToolbox) + AV1 (rav1e/rav1d) | Pending | [phase-2-hw-accel-av1.md](phase-2-hw-accel-av1.md) |
| 3 | B | Audio resilience — Opus FEC/PLC/DTX, jitter buffer, comfort noise | Pending | [phase-3-audio-resilience.md](phase-3-audio-resilience.md) |
| 4 | B | Video resilience — adaptive bitrate, frame timing, temporal SVC | Pending | [phase-4-video-resilience.md](phase-4-video-resilience.md) |

## Current pipeline issues (reference)

### Codec bugs found in review
1. 3 unnecessary frame copies in video encode path
2. Viewport change consumed before decode result checked (lost on EAGAIN)
3. Audio encoder requires exact frame_size with only debug_assert guard
4. `.unwrap()` on audio resampler instead of `?`
5. `BufferTooSmall` silently swallowed as "no frame"
6. Resampler created from codec config, not actual decoded format
7. No Opus FEC or PLC — silence gaps on any packet loss

### Missing vs libwebrtc
- No jitter buffer (audio or video)
- No packet loss concealment
- No adaptive bitrate
- No forward error correction
- No DTX/VAD
- No comfort noise generation
- No temporal scalability
- No frame delay smoothing

## Architecture

### Replacement mapping

| FFmpeg function | Replacement | Type |
|---|---|---|
| H.264 SW encode/decode | `openh264` | Bundled C via `cc` |
| H.264 HW encode (VAAPI) | `cros-codecs` | feature: `vaapi` |
| H.264 HW encode (VTB) | `objc2-video-toolbox` | feature: `videotoolbox` |
| AV1 encode/decode | `rav1e` / `rav1d` | feature: `av1` |
| Opus encode/decode | `unsafe-libopus` | c2rust, no C compiler |
| MJPEG decode | `image` crate | Pure Rust (already dep) |
| RGBA↔YUV | `yuvutils-rs` | Pure Rust, SIMD |
| Image scaling | `pic-scale` | Pure Rust |
| Audio resampling | `rubato` | Pure Rust, SIMD |

### Layered backend selection

```
VideoEncoder: vaapi → videotoolbox → openh264 (fallback)
VideoDecoder: rav1d (for AV1) → openh264 (for H.264)
```

### Feature flags
```toml
[features]
default = []
vaapi = ["dep:cros-codecs"]
videotoolbox = ["dep:objc2-video-toolbox"]
av1 = ["dep:rav1e", "dep:rav1d"]
```

### Module structure
```
moq-media/src/codec/
├── mod.rs                    # Exports, DefaultDecoders
├── audio/
│   ├── encoder.rs            # Opus (unsafe-libopus, FEC, DTX, internal buffering)
│   └── decoder.rs            # Opus (unsafe-libopus, PLC, lazy resampler)
├── video/
│   ├── encoder.rs            # Backend selection + RGBA→YUV
│   ├── decoder.rs            # Backend routing + YUV→RGBA
│   ├── openh264_enc.rs       # openh264 (temporal SVC)
│   ├── openh264_dec.rs       # openh264
│   ├── vaapi_enc.rs          # (feature: vaapi)
│   ├── vtb_enc.rs            # (feature: videotoolbox)
│   ├── rav1e_enc.rs          # (feature: av1)
│   ├── rav1d_dec.rs          # (feature: av1)
│   └── util/
│       ├── convert.rs        # YUV↔RGBA (yuvutils-rs)
│       ├── scale.rs          # Bilinear scaling (pic-scale)
│       ├── mjpg.rs           # MJPEG (image crate)
│       └── annexb.rs         # Annex B ↔ avcC NAL helpers
└── resample.rs               # Audio resampling (rubato)
```
