// TODO: VideoToolbox H.264 hardware decoder (macOS)
//
// Implementation sketch:
// - Create VTDecompressionSession from avcC (SPS/PPS via
//   CMVideoFormatDescriptionCreateFromH264ParameterSets)
// - For each packet: build CMSampleBuffer from length-prefixed NAL data,
//   call VTDecompressionSessionDecodeFrame with synchronous flag
// - Callback receives CVPixelBuffer (backed by IOSurface when HW-accelerated)
// - VtbGpuFrame wraps CFRetained<CVPixelBuffer>, implements GpuFrameInner:
//   - download(): CVPixelBufferLockBaseAddress read-only, read NV12 planes,
//     convert via nv12_to_rgba_data(), unlock
//   - dimensions(): CVPixelBufferGetWidth/Height
//   - gpu_pixel_format(): GpuPixelFormat::Nv12
// - For wgpu zero-copy: CVPixelBuffer → IOSurface → MTLTexture → wgpu HAL import
// - Same callback Arc<Mutex<Vec>> pattern as vtb/encoder.rs
// - unsafe impl Send/Sync for VtbGpuFrame (CF objects are refcounted, thread-safe when retained)
