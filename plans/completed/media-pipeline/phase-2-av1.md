# Phase 2: AV1 Codec Support

**Status: Done** — 84 tests passing, 0 clippy warnings.

## Goal
Add AV1 encode (rav1e) and decode (dav1d) behind a default-on `av1` feature flag. Add a `DynamicVideoDecoder` that auto-routes based on stream codec.

## Dependencies added

| Crate | Version | Purpose |
|-------|---------|---------|
| `rav1e` | 0.8 | AV1 encoder, pure Rust |
| `dav1d` | 0.11 | AV1 decoder, Rust bindings to libdav1d |

## Feature flags
```toml
# moq-media/Cargo.toml
[features]
default = ["av1"]
av1 = ["dep:rav1e", "dep:dav1d"]

# iroh-live/Cargo.toml
[features]
default = ["av1"]
av1 = ["moq-media/av1"]
```

## What was built

### Av1Encoder (`rav1e_enc.rs`)
- Speed preset 10 (fastest) with `low_latency=true` and `error_resilient=true`
- Bitrate formula: `pixels * 0.05 * framerate_factor` (~30% less than H.264)
- Uses `Plane::copy_from_raw_u8()` for correct stride handling
- Sequence header via `container_sequence_header()` (ISOBMFF/Matroska format, NOT raw OBU)
- Implements `VideoEncoder` + `VideoEncoderInner` traits

### Av1VideoDecoder (`dav1d_dec.rs`)
- Extracts Y/U/V planes from `dav1d::Picture` → `YuvData` → `yuv420_to_rgba_data()`
- Viewport scaling via shared `Scaler`
- Does NOT feed `container_sequence_header()` to dav1d (it's container metadata, not OBU data; sequence header is in keyframe packets)
- Implements `VideoDecoder` trait

### DynamicVideoDecoder (`decoder.rs`)
- Enum dispatch: `H264(H264VideoDecoder)` | `Av1(Av1VideoDecoder)`
- Routes based on `VideoConfig::codec` — zero-cost, no boxing
- AV1 variant gated with `#[cfg(feature = "av1")]`

### DefaultDecoders
- Changed from `type Video = H264VideoDecoder` to `type Video = DynamicVideoDecoder`
- Callers get automatic codec routing with no code changes

### Examples
- `publish.rs`: `--codec` arg selects `H264Encoder` or `Av1Encoder`
- `rooms.rs`: `--codec` arg + `DynamicVideoDecoder` for rendition switching
- `watch.rs`: `DynamicVideoDecoder` for automatic codec routing

### YUV→RGBA utility
- `yuv420_to_rgba_data()` in `convert.rs` using `yuvutils_rs::yuv420_to_rgba`

## Key lessons

- **rav1e buffering**: Even with `low_latency=true` and speed 10, rav1e buffers frames. Tests need 30-60 frames to reliably produce packets. Use `ctx.flush()` to drain at end of stream.
- **container_sequence_header() is NOT raw OBU**: Returns ISOBMFF/Matroska av1C box format. Do NOT feed to dav1d. The actual OBU sequence header is embedded in keyframe packets.
- **dav1d over rav1d**: `rav1d` has no Rust API yet (C API only, issue #1252). `dav1d` has clean Rust bindings. Trade-off: adds C dependency (libdav1d), but ergonomic and battle-tested.

## Commits

1. `e20a3e7` feat(moq-media): add av1 feature with rav1e and dav1d deps
2. `c42d445` feat(codec): add yuv420_to_rgba conversion
3. `f80b2d3` feat(codec): add AV1 encoder via rav1e
4. `d01de3a` feat(codec): add AV1 decoder via dav1d
5. `9737c44` feat(codec): add DynamicVideoDecoder for codec routing
6. `d96439e` feat(codec): route DefaultDecoders through DynamicVideoDecoder
7. `ec39946` feat: add av1 encoder option to examples
8. `d001cb3` test(codec): add AV1 encoder/decoder tests

## Tests (13 new, all `#[cfg(feature = "av1")]`)

Encoder: encode_produces_packets, sequence_header_after_creation, encode_sequence_has_keyframes, timestamps_increase, multiple_presets, config_fields

Decoder: encode_decode_roundtrip, solid_red_visual_roundtrip, viewport_scaling, unsupported_codec_errors

Integration: av1_roundtrip_p180_red, av1_roundtrip_p360_green, dynamic_routes_h264, dynamic_routes_av1
