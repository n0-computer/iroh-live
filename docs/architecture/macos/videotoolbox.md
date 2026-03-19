# VideoToolbox on macOS

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | untested |
| Applies to | rusty-codecs |
| Platforms | macOS |

## Encoder

`VtbEncoder` in `rusty-codecs/src/codec/vtb/encoder.rs` uses `VTCompressionSession` for hardware H.264 encoding via the objc2 bindings.

### Session setup

The compression session is created with the target codec type (H.264) and output dimensions. Properties are set after creation:

- `kVTCompressionPropertyKey_RealTime`: true (low-latency mode).
- `kVTCompressionPropertyKey_AverageBitRate`: target bitrate, calculated from dimensions and a bits-per-pixel factor if not specified explicitly.
- `kVTCompressionPropertyKey_MaxKeyFrameInterval`: keyframe interval in frames.
- `kVTCompressionPropertyKey_ProfileLevel`: Baseline with auto-level selection.
- `kVTCompressionPropertyKey_AllowFrameReordering`: false (no B-frames, reduces latency).

### Frame encoding

`push_frame()` converts the input `VideoFrame` from its native pixel format to YUV420 planar (`kCVPixelFormatType_420YpCbCr8Planar`). A `CVPixelBuffer` is obtained from the session's pixel buffer pool (pre-allocated by VideoToolbox), and Y, U, and V planes are copied into it, respecting each plane's row stride. The frame is submitted with a `CMTime` timestamp.

### Output callback

VideoToolbox invokes the output callback on its own internal thread. The callback extracts the encoded NAL units from the `CMSampleBuffer`:

- SPS and PPS are read from the `CMVideoFormatDescription` via `CMVideoFormatDescriptionGetH264ParameterSetAtIndex`.
- The encoded data is read from the `CMBlockBuffer` as length-prefixed NAL units.

VideoToolbox outputs NAL units in AVCC format (length-prefixed) natively. No Annex B conversion is needed for the MoQ transport, which expects length-prefixed NALs. The `avcC` configuration record is built from the SPS/PPS extracted from the first keyframe.

Encoded frames are queued in a `VecDeque` behind a `Mutex`, shared between the callback thread and the caller thread. `pop_packet()` drains this queue.

### Thread safety

`VTCompressionSession` is documented as thread-safe by Apple. The `VtbEncoder` is `Send` (explicitly implemented via `unsafe impl Send`) because the session and the shared callback state (Arc<Mutex<_>>) are safe to move between threads.

## Decoder

`VtbDecoder` uses `VTDecompressionSession` for hardware H.264 decoding. It accepts length-prefixed NAL units, wraps them in `CMSampleBuffer`s, and submits them for decoding. The output callback receives `CVPixelBuffer`s in NV12 format, wrapped as `GpuFrame` for deferred CPU readback.

## Testing status

Neither the encoder nor the decoder has been tested on macOS hardware. The code compiles and the API integration is complete, but functional verification requires a macOS device with VideoToolbox support.

## NAL format

VideoToolbox is one of the few codec APIs that outputs length-prefixed (AVCC) NALs natively. Most other H.264 APIs (openh264, VAAPI, V4L2) output Annex B byte streams and require conversion. This means the VTB encoder avoids the NAL conversion allocation that other encoders incur.
