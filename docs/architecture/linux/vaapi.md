# VAAPI on Linux

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | rusty-codecs |
| Platforms | Linux (Intel, AMD) |

## Codec library

The VAAPI encoder and decoder use cros-codecs, maintained by Google for ChromeOS. cros-codecs provides a stateless H.264 encoder and decoder on top of cros-libva, which wraps the VA-API C library. The `vaapi` feature flag gates all VAAPI-specific code in rusty-codecs.

System dependencies: `libva-dev` (development headers) and a VA-API driver (`intel-media-driver` for Intel, `mesa-va-drivers` for AMD).

## Encoder flow

`VaapiEncoder` in `rusty-codecs/src/codec/vaapi/encoder.rs` implements the `VideoEncoder` trait. The encode path:

1. `Display::open()` opens the VA display on the DRM render node.
2. `EncoderConfig` is configured for Baseline profile with CBR rate control.
3. `StatelessEncoder::new_vaapi()` creates the hardware encoder in blocking mode.
4. Input frames are submitted as `VaapiInputFrame`, which is either CPU NV12 data (uploaded via image mapping) or a DMA-BUF handle (imported directly as a VA surface, zero-copy).
5. `encode(metadata, handle)` submits the frame. `poll()` retrieves the output as Annex B NAL units.
6. SPS/PPS are extracted from the first keyframe to build the avcC configuration record for the catalog.

The encoder accepts NV12 surface input. When the capture source provides RGBA or I420, `pixel_format_to_nv12()` interleaves the chroma planes into NV12 format before submission.

Blocking mode is used for simplicity: `poll()` waits for the hardware to finish encoding before returning. At typical real-time frame rates (30 fps), the hardware finishes well within the frame budget.

## Decoder flow

`VaapiDecoder` uses the same cros-codecs stack. It receives length-prefixed H.264 packets, converts to Annex B, and submits to the hardware decoder. Decoded frames are available as VA surfaces, which can be:

- Mapped to CPU memory (NV12 planes) for software rendering or upload.
- Exported as DMA-BUF file descriptors for zero-copy rendering via Vulkan. See [dmabuf.md](dmabuf.md) for the import pipeline.

The decoder opens a separate VA display for frame mapping and DMA-BUF export, avoiding driver serialization when mapping frames while the decoder is active.

## Surface management

The decoder allocates a pool of VA surfaces: codec-required surfaces (DPB size) plus eight extra for downstream pipeline depth (three frames in transit plus five headroom for burst decoding). This sizing matches Chromium's WebRTC configuration.

## Open areas

**Low-power encoding mode**: Intel GPUs support a low-power encoding path (`VAEntrypointEncSliceLP`) that uses the media engine instead of the GPU execution units. cros-codecs does not currently expose this option. Enabling it could reduce power consumption on battery-powered devices.
