#[cfg(target_os = "linux")]
use std::os::unix::io::OwnedFd;
use std::{
    fmt,
    sync::{Arc, OnceLock},
    time::Duration,
};

use image::RgbaImage;
use strum::{Display, EnumString, VariantNames};

pub use crate::processing::scale::ScaleMode;

/// Describes an audio stream's sample rate and channel layout.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// Sample rate in Hz (e.g. 48 000).
    pub sample_rate: u32,
    /// Number of audio channels (1 = mono, 2 = stereo).
    pub channel_count: u32,
}

impl AudioFormat {
    /// Returns a mono 48 kHz format.
    pub fn mono_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 1,
        }
    }

    /// Returns a stereo 48 kHz format.
    pub fn stereo_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 2,
        }
    }

    /// Creates an [`AudioFormat`] from an [`AudioConfig`](crate::config::AudioConfig).
    pub fn from_config(config: &crate::config::AudioConfig) -> Self {
        Self {
            channel_count: config.channel_count,
            sample_rate: config.sample_rate,
        }
    }
}

/// Pixel byte ordering for CPU-resident frame data.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum PixelFormat {
    /// Red, green, blue, alpha (standard for most APIs).
    #[default]
    Rgba,
    /// Blue, green, red, alpha (common on Windows / some capture APIs).
    Bgra,
}

/// Describes the pixel layout and resolution of raw video frames.
#[derive(Clone, Debug)]
pub struct VideoFormat {
    /// Pixel byte ordering.
    pub pixel_format: PixelFormat,
    /// Frame dimensions as `[width, height]`.
    pub dimensions: [u32; 2],
}

/// Platform-specific handle for zero-copy GPU frame import/export.
///
/// Each variant is gated on the target platform. On platforms with no
/// supported handle type the enum is uninhabited and
/// `Option<&NativeFrameHandle>` is always `None`.
#[derive(Debug)]
#[non_exhaustive]
pub enum NativeFrameHandle {
    /// Linux DMA-BUF file descriptor and layout metadata.
    #[cfg(target_os = "linux")]
    DmaBuf(DmaBufInfo),
    /// Android HardwareBuffer handle for zero-copy GPU import.
    #[cfg(target_os = "android")]
    HardwareBuffer(HardwareBufferInfo),
    /// macOS CVPixelBuffer for zero-copy VideoToolbox encode and Metal render.
    #[cfg(all(target_os = "macos", feature = "apple-gpu"))]
    CvPixelBuffer(CvPixelBufferInfo),
    // Future variants:
    // #[cfg(target_os = "windows")] D3D11Texture(D3D11TextureInfo),
}

/// Android HardwareBuffer metadata for zero-copy GPU frame import.
///
/// Contains a reference-counted [`ndk::hardware_buffer::HardwareBufferRef`]
/// along with NV12 plane layout info needed for GL/Vulkan import on the
/// consumer side.
#[cfg(target_os = "android")]
#[derive(Debug, Clone)]
pub struct HardwareBufferInfo {
    /// Reference-counted handle to the underlying `AHardwareBuffer`.
    pub buffer: ndk::hardware_buffer::HardwareBufferRef,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Row stride of the Y plane in bytes.
    pub y_stride: u32,
    /// Byte offset of the UV plane within the locked buffer.
    pub uv_offset: u32,
    /// Row stride of the UV plane in bytes.
    pub uv_stride: u32,
}

/// macOS CVPixelBuffer handle for zero-copy GPU frame passing.
///
/// Wraps a `CFRetained<CVPixelBuffer>` from `objc2-core-video`. Both
/// `screencapturekit::CVPixelBuffer` and `objc2_core_video::CVPixelBuffer`
/// are wrappers around the same CF type, bridged via raw pointer.
#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
pub struct CvPixelBufferInfo {
    pixel_buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Native GPU pixel format (typically BGRA from capture, NV12 from decoder).
    pub pixel_format: GpuPixelFormat,
}

#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
impl CvPixelBufferInfo {
    /// Creates a new info from a raw `CVPixelBufferRef`, retaining it.
    ///
    /// # Safety
    ///
    /// `raw` must be a valid, non-null `CVPixelBufferRef`.
    pub unsafe fn from_raw(
        raw: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        pixel_format: GpuPixelFormat,
    ) -> Self {
        let ptr = std::ptr::NonNull::new(raw.cast::<objc2_core_video::CVPixelBuffer>()).unwrap();
        let pixel_buffer = unsafe { objc2_core_foundation::CFRetained::retain(ptr) };
        Self {
            pixel_buffer,
            width,
            height,
            pixel_format,
        }
    }

    /// Creates from an already-retained `CFRetained<CVPixelBuffer>`.
    pub fn from_retained(
        pixel_buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
        width: u32,
        height: u32,
        pixel_format: GpuPixelFormat,
    ) -> Self {
        Self {
            pixel_buffer,
            width,
            height,
            pixel_format,
        }
    }

    /// Returns a reference to the inner `CVPixelBuffer`.
    pub fn pixel_buffer(&self) -> &objc2_core_video::CVPixelBuffer {
        &self.pixel_buffer
    }

    /// Returns the raw `CVPixelBufferRef` pointer. Valid for the lifetime of this struct.
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        ((&*self.pixel_buffer) as *const objc2_core_video::CVPixelBuffer)
            .cast_mut()
            .cast()
    }
}

#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
impl fmt::Debug for CvPixelBufferInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CvPixelBufferInfo")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixel_format", &self.pixel_format)
            .finish()
    }
}

// Safety: CVPixelBuffer is reference-counted and thread-safe.
#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
unsafe impl Send for CvPixelBufferInfo {}
#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
unsafe impl Sync for CvPixelBufferInfo {}

/// GPU-resident frame backed by a macOS CVPixelBuffer.
///
/// Used for zero-copy capture→encode (ScreenCaptureKit/AVFoundation → VTB)
/// and zero-copy decode→render (VTB → CVMetalTextureCache → wgpu).
/// Falls back to CPU readback via `download_rgba()` when needed.
#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
pub struct AppleGpuFrame {
    info: CvPixelBufferInfo,
}

#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
impl AppleGpuFrame {
    /// Creates a GPU frame from a raw CVPixelBufferRef, retaining it.
    ///
    /// # Safety
    ///
    /// `raw` must be a valid, non-null `CVPixelBufferRef`.
    pub unsafe fn from_raw(
        raw: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        pixel_format: GpuPixelFormat,
    ) -> Self {
        Self {
            info: unsafe { CvPixelBufferInfo::from_raw(raw, width, height, pixel_format) },
        }
    }

    /// Creates from an already-retained `CFRetained<CVPixelBuffer>`.
    pub fn from_retained(
        pixel_buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
        width: u32,
        height: u32,
        pixel_format: GpuPixelFormat,
    ) -> Self {
        Self {
            info: CvPixelBufferInfo::from_retained(pixel_buffer, width, height, pixel_format),
        }
    }

    /// Returns a reference to the inner `CVPixelBuffer`.
    pub fn pixel_buffer(&self) -> &objc2_core_video::CVPixelBuffer {
        self.info.pixel_buffer()
    }

    /// Returns the raw `CVPixelBufferRef` pointer.
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.info.as_ptr()
    }

    /// Downloads BGRA pixel data from a packed CVPixelBuffer.
    fn download_bgra(&self) -> anyhow::Result<Vec<u8>> {
        use objc2_core_video::*;
        let pb = self.info.pixel_buffer();
        let status = unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags(1)) };
        if status != 0 {
            anyhow::bail!("CVPixelBufferLockBaseAddress failed: {status}");
        }

        let result = unsafe {
            let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
            let stride = CVPixelBufferGetBytesPerRow(pb);
            let w = CVPixelBufferGetWidth(pb);
            let h = CVPixelBufferGetHeight(pb);
            let row_bytes = w * 4;
            let mut rgba = vec![0u8; row_bytes * h];
            for y in 0..h {
                let src = std::slice::from_raw_parts(base.add(y * stride), row_bytes);
                let dst = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
                for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                    d[0] = s[2]; // R ← B
                    d[1] = s[1]; // G
                    d[2] = s[0]; // B ← R
                    d[3] = s[3]; // A
                }
            }
            rgba
        };

        unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags(1)) };
        Ok(result)
    }

    /// Downloads NV12 plane data from a planar CVPixelBuffer.
    fn download_nv12_planes(&self) -> anyhow::Result<Nv12Planes> {
        use objc2_core_video::*;
        let pb = self.info.pixel_buffer();
        let status = unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags(1)) };
        if status != 0 {
            anyhow::bail!("CVPixelBufferLockBaseAddress failed: {status}");
        }

        let result = unsafe {
            let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *const u8;
            let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
            let y_height = CVPixelBufferGetHeightOfPlane(pb, 0);
            let y_data = std::slice::from_raw_parts(y_base, y_stride * y_height).to_vec();

            let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *const u8;
            let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
            let uv_height = CVPixelBufferGetHeightOfPlane(pb, 1);
            let uv_data = std::slice::from_raw_parts(uv_base, uv_stride * uv_height).to_vec();

            Nv12Planes {
                y_data,
                y_stride: y_stride as u32,
                uv_data,
                uv_stride: uv_stride as u32,
                width: self.info.width,
                height: self.info.height,
            }
        };

        unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags(1)) };
        Ok(result)
    }
}

#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
impl fmt::Debug for AppleGpuFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppleGpuFrame")
            .field("width", &self.info.width)
            .field("height", &self.info.height)
            .field("pixel_format", &self.info.pixel_format)
            .finish()
    }
}

// Safety: CVPixelBuffer is reference-counted and thread-safe when retained.
#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
unsafe impl Send for AppleGpuFrame {}
#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
unsafe impl Sync for AppleGpuFrame {}

#[cfg(all(target_os = "macos", feature = "apple-gpu"))]
impl GpuFrameInner for AppleGpuFrame {
    fn download_rgba(&self) -> anyhow::Result<RgbaImage> {
        match self.info.pixel_format {
            GpuPixelFormat::Bgra | GpuPixelFormat::Bgrx => {
                let rgba = self.download_bgra()?;
                RgbaImage::from_raw(self.info.width, self.info.height, rgba)
                    .ok_or_else(|| anyhow::anyhow!("RGBA data size mismatch"))
            }
            GpuPixelFormat::Nv12 => {
                #[cfg(any(feature = "h264", feature = "av1"))]
                {
                    let planes = self.download_nv12_planes()?;
                    let rgba = crate::processing::convert::nv12_to_rgba_data(
                        &planes.y_data,
                        planes.y_stride,
                        &planes.uv_data,
                        planes.uv_stride,
                        planes.width,
                        planes.height,
                    )?;
                    RgbaImage::from_raw(planes.width, planes.height, rgba)
                        .ok_or_else(|| anyhow::anyhow!("NV12→RGBA data size mismatch"))
                }
                #[cfg(not(any(feature = "h264", feature = "av1")))]
                anyhow::bail!("NV12→RGBA conversion requires h264 or av1 feature")
            }
        }
    }

    fn gpu_pixel_format(&self) -> GpuPixelFormat {
        self.info.pixel_format
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.info.width, self.info.height)
    }

    fn download_nv12(&self) -> Option<anyhow::Result<Nv12Planes>> {
        if self.info.pixel_format == GpuPixelFormat::Nv12 {
            Some(self.download_nv12_planes())
        } else {
            None
        }
    }

    fn native_handle(&self) -> Option<NativeFrameHandle> {
        let info = unsafe {
            CvPixelBufferInfo::from_raw(
                self.info.as_ptr(),
                self.info.width,
                self.info.height,
                self.info.pixel_format,
            )
        };
        Some(NativeFrameHandle::CvPixelBuffer(info))
    }
}

/// Encoded media packet, independent of transport.
#[derive(Clone, Debug)]
pub struct MediaPacket {
    /// Presentation timestamp.
    pub timestamp: Duration,
    /// Encoded payload (scatter-gather buffer, zero-copy from MoQ transport).
    pub payload: buf_list::BufList,
    /// Whether this is a keyframe (first frame of a new group).
    pub is_keyframe: bool,
}

impl MediaPacket {
    /// Get contiguous bytes from the payload.
    /// Zero-copy for single-chunk payloads (common case from MoQ transport).
    pub fn into_payload_bytes(mut self) -> bytes::Bytes {
        use bytes::Buf;
        self.payload.copy_to_bytes(self.payload.remaining())
    }
}

/// Single compressed video or audio frame produced by an encoder.
#[derive(Debug)]
pub struct EncodedFrame {
    /// Whether this frame can be decoded independently (I-frame / IDR).
    pub is_keyframe: bool,
    /// Presentation timestamp.
    pub timestamp: Duration,
    /// Compressed bitstream data.
    pub payload: bytes::Bytes,
}

/// GPU-resident frame from a hardware decoder or capture device.
#[derive(Clone)]
pub struct GpuFrame {
    inner: Arc<dyn GpuFrameInner>,
}

impl fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuFrame")
            .field("dimensions", &self.inner.dimensions())
            .field("pixel_format", &self.inner.gpu_pixel_format())
            .finish()
    }
}

impl GpuFrame {
    /// Wraps a platform-specific GPU frame implementation.
    pub fn new(inner: Arc<dyn GpuFrameInner>) -> Self {
        Self { inner }
    }

    /// Downloads the GPU frame to a CPU RGBA image.
    pub fn download_rgba(&self) -> anyhow::Result<RgbaImage> {
        self.inner.download_rgba()
    }

    /// Returns the frame dimensions as `(width, height)`.
    pub fn dimensions(&self) -> (u32, u32) {
        self.inner.dimensions()
    }

    /// Returns the native GPU pixel format.
    pub fn gpu_pixel_format(&self) -> GpuPixelFormat {
        self.inner.gpu_pixel_format()
    }

    /// Downloads raw NV12 plane data, if the GPU frame supports it.
    pub fn download_nv12(&self) -> Option<anyhow::Result<Nv12Planes>> {
        self.inner.download_nv12()
    }

    /// Returns a platform-specific native handle for zero-copy import, if available.
    ///
    /// The handle is computed on demand — no file descriptors are held per frame.
    pub fn native_handle(&self) -> Option<NativeFrameHandle> {
        self.inner.native_handle()
    }
}

/// NV12 plane data downloaded from GPU.
#[derive(Debug, Clone)]
pub struct Nv12Planes {
    pub y_data: Vec<u8>,
    pub y_stride: u32,
    pub uv_data: Vec<u8>,
    pub uv_stride: u32,
    pub width: u32,
    pub height: u32,
}

/// Platform-specific GPU frame operations.
pub trait GpuFrameInner: Send + Sync + fmt::Debug + 'static {
    /// Downloads the GPU frame to a CPU RGBA image.
    fn download_rgba(&self) -> anyhow::Result<RgbaImage>;
    /// Returns the native pixel format on the GPU (NV12, I420, etc.).
    fn gpu_pixel_format(&self) -> GpuPixelFormat;
    /// Returns the frame dimensions as `(width, height)`.
    fn dimensions(&self) -> (u32, u32);
    /// Downloads NV12 plane data for GPU-side color conversion.
    ///
    /// Returns `None` if the frame is not NV12 or the implementation does
    /// not support plane download.
    fn download_nv12(&self) -> Option<anyhow::Result<Nv12Planes>> {
        None
    }
    /// Returns a platform-specific native handle for zero-copy import/export.
    ///
    /// Returns an owned handle so that implementations can extract DMA-BUF
    /// file descriptors on demand (transient export on access, rather than
    /// storing one FD per buffered frame).
    fn native_handle(&self) -> Option<NativeFrameHandle> {
        None
    }
}

/// DMA-BUF metadata for zero-copy GPU frame import.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct DmaBufInfo {
    pub fd: OwnedFd,
    pub modifier: u64,
    pub drm_format: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub display_width: u32,
    pub display_height: u32,
    pub planes: Vec<DmaBufPlaneInfo>,
}

/// Per-plane DMA-BUF layout info.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct DmaBufPlaneInfo {
    pub offset: u32,
    pub pitch: u32,
}

/// Native GPU pixel formats for DMA-BUF frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuPixelFormat {
    /// Semi-planar YUV 4:2:0 — VAAPI decoders and some camera sources.
    Nv12,
    /// Packed 32-bit BGRx (X = padding) — compositor screen capture.
    Bgrx,
    /// Packed 32-bit BGRA — macOS ScreenCaptureKit / AVFoundation.
    Bgra,
}

/// Backing storage for a video frame.
///
/// Variants cover the common pixel layouts produced by capture devices and
/// hardware decoders. Encoders match on the variant to pick the cheapest
/// conversion path (e.g. a VAAPI encoder can consume [`Nv12`](Self::Nv12) or
/// [`Gpu`](Self::Gpu) directly, avoiding an extra color-space round-trip).
#[derive(derive_more::Debug, Clone)]
pub enum FrameData {
    /// Packed RGBA or BGRA pixel data in CPU memory.
    Packed {
        pixel_format: PixelFormat,
        #[debug(skip)]
        data: bytes::Bytes,
    },
    /// Planar I420 (YUV 4:2:0) in CPU memory.
    I420 {
        #[debug(skip)]
        y: bytes::Bytes,
        #[debug(skip)]
        u: bytes::Bytes,
        #[debug(skip)]
        v: bytes::Bytes,
    },
    /// Semi-planar NV12 in CPU memory.
    Nv12(Nv12Planes),
    /// GPU-resident frame from a hardware decoder or capture device.
    #[debug("Gpu({:?})", _0)]
    Gpu(GpuFrame),
}

/// Video frame that may reside in CPU or GPU memory.
///
/// Unifies the capture and decode paths into a single type. Capture sources
/// produce `Packed` RGBA frames, software decoders produce `Packed` or `I420`
/// frames, and hardware decoders produce `Gpu` frames. Encoders inspect
/// [`FrameData`] to pick the cheapest input path.
/// Timing annotations accumulated as a frame moves through the pipeline.
#[derive(derive_more::Debug)]
pub struct VideoFrame {
    /// Frame dimensions as `[width, height]`.
    pub dimensions: [u32; 2],
    /// Backing pixel data.
    pub data: FrameData,
    /// Presentation timestamp (`Duration::ZERO` for capture frames before
    /// the encoder assigns a PTS).
    pub timestamp: Duration,
    /// Lazy RGBA cache for rendering and legacy accessors.
    #[debug(skip)]
    cached_rgba: OnceLock<RgbaImage>,
}

impl Clone for VideoFrame {
    fn clone(&self) -> Self {
        Self {
            dimensions: self.dimensions,
            data: self.data.clone(),
            timestamp: self.timestamp,
            cached_rgba: OnceLock::new(),
        }
    }
}

impl VideoFrame {
    /// Creates a packed RGBA frame (common path for camera/screen capture).
    pub fn new_rgba(data: bytes::Bytes, width: u32, height: u32, timestamp: Duration) -> Self {
        Self {
            dimensions: [width, height],
            data: FrameData::Packed {
                pixel_format: PixelFormat::Rgba,
                data,
            },
            timestamp,
            cached_rgba: OnceLock::new(),
        }
    }

    /// Creates a packed frame with an explicit pixel format and timestamp.
    pub fn new_packed(
        data: bytes::Bytes,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        timestamp: Duration,
    ) -> Self {
        Self {
            dimensions: [width, height],
            data: FrameData::Packed { pixel_format, data },
            timestamp,
            cached_rgba: OnceLock::new(),
        }
    }

    /// Creates a CPU-backed frame from raw pixel data and dimensions.
    ///
    /// Equivalent to the old `DecodedVideoFrame::new_cpu_with_format`.
    pub fn new_cpu_with_format(
        data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp: Duration,
        pixel_format: PixelFormat,
    ) -> Self {
        Self {
            dimensions: [width, height],
            data: FrameData::Packed {
                pixel_format,
                data: data.into(),
            },
            timestamp,
            cached_rgba: OnceLock::new(),
        }
    }

    /// Creates a CPU-backed RGBA frame from raw pixel data.
    pub fn new_cpu(data: Vec<u8>, width: u32, height: u32, timestamp: Duration) -> Self {
        Self::new_cpu_with_format(data, width, height, timestamp, PixelFormat::Rgba)
    }

    /// Creates a frame from an existing [`RgbaImage`].
    pub fn from_image(image: RgbaImage, timestamp: Duration) -> Self {
        let width = image.width();
        let height = image.height();
        Self {
            dimensions: [width, height],
            data: FrameData::Packed {
                pixel_format: PixelFormat::Rgba,
                data: image.into_raw().into(),
            },
            timestamp,
            cached_rgba: OnceLock::new(),
        }
    }

    /// Creates a frame with NV12 (semi-planar YUV 4:2:0) CPU data.
    pub fn new_nv12(planes: Nv12Planes, timestamp: Duration) -> Self {
        let w = planes.width;
        let h = planes.height;
        Self {
            dimensions: [w, h],
            data: FrameData::Nv12(planes),
            timestamp,
            cached_rgba: OnceLock::new(),
        }
    }

    /// Creates a frame with I420 (planar YUV 4:2:0) CPU data.
    pub fn new_i420(
        y: bytes::Bytes,
        u: bytes::Bytes,
        v: bytes::Bytes,
        width: u32,
        height: u32,
        timestamp: Duration,
    ) -> Self {
        Self {
            dimensions: [width, height],
            data: FrameData::I420 { y, u, v },
            timestamp,
            cached_rgba: OnceLock::new(),
        }
    }

    /// Creates a GPU-backed frame.
    pub fn new_gpu(gpu: GpuFrame, timestamp: Duration) -> Self {
        let (w, h) = gpu.dimensions();
        Self {
            dimensions: [w, h],
            data: FrameData::Gpu(gpu),
            timestamp,
            cached_rgba: OnceLock::new(),
        }
    }

    /// Returns the frame width.
    pub fn width(&self) -> u32 {
        self.dimensions[0]
    }

    /// Returns the frame height.
    pub fn height(&self) -> u32 {
        self.dimensions[1]
    }

    /// Whether this frame lives on the GPU.
    pub fn is_gpu(&self) -> bool {
        matches!(&self.data, FrameData::Gpu(_))
    }

    /// Returns the GPU frame, if this is a GPU-backed frame.
    pub fn gpu_frame(&self) -> Option<&GpuFrame> {
        match &self.data {
            FrameData::Gpu(f) => Some(f),
            _ => None,
        }
    }

    /// Returns a platform-specific native handle for zero-copy import, if available.
    pub fn native_handle(&self) -> Option<NativeFrameHandle> {
        match &self.data {
            FrameData::Gpu(gpu) => gpu.native_handle(),
            _ => None,
        }
    }

    /// Returns the frame as a CPU RGBA image.
    ///
    /// For packed RGBA frames the data is wrapped without copying. For GPU
    /// frames the pixels are downloaded on first call and cached. NV12
    /// frames are converted via BT.601 on first call and cached. I420
    /// frames are not yet supported and will panic.
    pub fn rgba_image(&self) -> &RgbaImage {
        self.cached_rgba.get_or_init(|| {
            let [w, h] = self.dimensions;
            match &self.data {
                FrameData::Packed {
                    pixel_format: PixelFormat::Rgba,
                    data,
                } => RgbaImage::from_raw(w, h, data.to_vec())
                    .expect("pixel data size does not match dimensions"),
                FrameData::Packed {
                    pixel_format: PixelFormat::Bgra,
                    data,
                } => {
                    let mut rgba = data.to_vec();
                    for chunk in rgba.chunks_exact_mut(4) {
                        chunk.swap(0, 2);
                    }
                    RgbaImage::from_raw(w, h, rgba)
                        .expect("pixel data size does not match dimensions")
                }
                FrameData::Gpu(gpu) => gpu.download_rgba().expect("GPU frame download failed"),
                FrameData::Nv12(planes) => {
                    #[cfg(any(feature = "h264", feature = "av1"))]
                    {
                        let rgba = crate::processing::convert::nv12_to_rgba_data(
                            &planes.y_data,
                            planes.y_stride,
                            &planes.uv_data,
                            planes.uv_stride,
                            w,
                            h,
                        )
                        .expect("NV12→RGBA conversion failed");
                        RgbaImage::from_raw(w, h, rgba)
                            .expect("RGBA data size does not match dimensions")
                    }
                    #[cfg(not(any(feature = "h264", feature = "av1")))]
                    {
                        let _ = planes;
                        unimplemented!("rgba_image() for NV12 requires the `h264` or `av1` feature")
                    }
                }
                FrameData::I420 { y, u, v } => {
                    #[cfg(any(feature = "h264", feature = "av1"))]
                    {
                        let y_stride = w;
                        let uv_stride = w.div_ceil(2);
                        let rgba = crate::processing::convert::yuv420_to_rgba_from_slices(
                            y, y_stride, u, uv_stride, v, uv_stride, w, h,
                        )
                        .expect("I420→RGBA conversion failed");
                        RgbaImage::from_raw(w, h, rgba)
                            .expect("RGBA data size does not match dimensions")
                    }
                    #[cfg(not(any(feature = "h264", feature = "av1")))]
                    {
                        let _ = (y, u, v);
                        unimplemented!("rgba_image() for I420 requires the `h264` or `av1` feature")
                    }
                }
            }
        })
    }
}

/// Standard video resolution presets.
///
/// Each preset defines a width, height, and framerate suitable for
/// common streaming scenarios.
#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq, Ord, PartialOrd)]
pub enum VideoPreset {
    #[strum(serialize = "180p")]
    P180,
    #[strum(serialize = "360p")]
    P360,
    #[strum(serialize = "720p")]
    P720,
    #[strum(serialize = "1080p")]
    P1080,
}

impl VideoPreset {
    /// Returns all presets ordered from lowest to highest resolution.
    pub fn all() -> [Self; 4] {
        [Self::P180, Self::P360, Self::P720, Self::P1080]
    }

    /// Returns the `(width, height)` for this preset.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::P180 => (320, 180),
            Self::P360 => (640, 360),
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
        }
    }

    /// Returns the frame width in pixels.
    pub fn width(&self) -> u32 {
        self.dimensions().0
    }

    /// Returns the frame height in pixels.
    pub fn height(&self) -> u32 {
        self.dimensions().1
    }

    /// Returns the target framerate for this preset.
    pub fn fps(&self) -> u32 {
        30
    }

    /// Parses a preset name, returning a helpful error listing valid presets on failure.
    pub fn parse_or_list(s: &str) -> anyhow::Result<Self> {
        s.parse().map_err(|_| {
            let names: Vec<_> = Self::all().iter().map(|p| p.to_string()).collect();
            anyhow::anyhow!(
                "unknown video preset '{s}'. Available: {}",
                names.join(", ")
            )
        })
    }
}

/// Audio quality presets controlling encoder bitrate.
#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq)]
#[strum(serialize_all = "lowercase")]
pub enum AudioPreset {
    /// High quality (128 kbps).
    Hq,
    /// Low quality (32 kbps).
    Lq,
}

impl AudioPreset {
    /// Parses a preset name, returning a helpful error listing valid presets on failure.
    pub fn parse_or_list(s: &str) -> anyhow::Result<Self> {
        s.parse()
            .map_err(|_| anyhow::anyhow!("unknown audio preset '{s}'. Available: hq, lq"))
    }
}

/// Playback quality preference for decoder/renderer selection.
#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq, Default)]
#[strum(serialize_all = "lowercase")]
pub enum Quality {
    /// Maximum quality, no downscaling.
    Highest,
    /// Default quality, minor optimizations allowed.
    #[default]
    High,
    /// Balanced quality and performance.
    Mid,
    /// Favor performance over visual fidelity.
    Low,
}

/// Which decoder backend to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecoderBackend {
    /// Try hardware decoder first, fall back to software.
    #[default]
    Auto,
    /// Force software decoder.
    Software,
}

/// Configuration passed to video decoders at construction time.
#[derive(Clone, Debug, Default)]
pub struct DecodeConfig {
    /// Desired output pixel format.
    pub pixel_format: PixelFormat,
    /// Decoder backend selection strategy.
    pub backend: DecoderBackend,
}

/// Combined decoding and quality settings for media playback.
#[derive(Clone, Debug, Default)]
pub struct PlaybackConfig {
    /// Decoder configuration (backend, pixel format).
    pub decode_config: DecodeConfig,
    /// Quality preference for rendering.
    pub quality: Quality,
}

/// H.264 NAL unit framing format.
///
/// Controls whether the encoder outputs Annex B start codes (`0x00000001`)
/// or length-prefixed (avcC-style, 4-byte big-endian) NAL units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NalFormat {
    /// Annex B start codes. SPS/PPS are inline in keyframes.
    #[default]
    AnnexB,
    /// 4-byte length-prefixed NALs (ISO 14496-15 / avcC style).
    Avcc,
}

/// Configuration for creating a video encoder.
///
/// Construct from a [`VideoPreset`] via [`from_preset`](Self::from_preset),
/// then override individual fields with the builder methods.
///
/// ```
/// # use rusty_codecs::format::{VideoEncoderConfig, VideoPreset};
/// let config = VideoEncoderConfig::from_preset(VideoPreset::P720)
///     .bitrate(2_000_000)
///     .framerate(60);
/// ```
#[derive(Clone, Debug)]
pub struct VideoEncoderConfig {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Frames per second.
    pub framerate: u32,
    /// Target bitrate in bits per second.
    /// `None` uses a codec-specific default based on resolution and framerate.
    pub bitrate: Option<u64>,
    /// How source frames are scaled to match the encoder dimensions.
    pub scale_mode: crate::processing::scale::ScaleMode,
    /// Keyframe (IDR) interval in frames.
    /// `None` defaults to one keyframe per second (`framerate` frames).
    pub keyframe_interval: Option<u32>,
    /// H.264 NAL framing format. Ignored by non-H.264 codecs.
    #[cfg_attr(
        not(any(
            feature = "h264",
            feature = "vaapi",
            feature = "v4l2",
            feature = "videotoolbox",
            feature = "android"
        )),
        allow(dead_code)
    )]
    pub(crate) nal_format: NalFormat,
}

impl VideoEncoderConfig {
    /// Creates a config from a [`VideoPreset`] with automatic bitrate.
    pub fn from_preset(preset: VideoPreset) -> Self {
        let (width, height) = preset.dimensions();
        Self {
            width,
            height,
            framerate: preset.fps(),
            bitrate: None,
            scale_mode: Default::default(),
            keyframe_interval: None,
            nal_format: NalFormat::default(),
        }
    }

    /// Sets the frame width.
    pub fn width(mut self, width: u32) -> Self {
        self.width = width;
        self
    }

    /// Sets the frame height.
    pub fn height(mut self, height: u32) -> Self {
        self.height = height;
        self
    }

    /// Sets the framerate.
    pub fn framerate(mut self, framerate: u32) -> Self {
        self.framerate = framerate;
        self
    }

    /// Sets the target bitrate in bits per second.
    pub fn bitrate(mut self, bitrate: u64) -> Self {
        self.bitrate = Some(bitrate);
        self
    }

    /// Sets the scale mode.
    pub fn scale_mode(mut self, mode: crate::processing::scale::ScaleMode) -> Self {
        self.scale_mode = mode;
        self
    }

    /// Sets the keyframe (IDR) interval in frames.
    ///
    /// Defaults to one keyframe per second if not set.
    pub fn keyframe_interval(mut self, frames: u32) -> Self {
        self.keyframe_interval = Some(frames);
        self
    }

    /// Returns the keyframe interval, defaulting to one per second.
    pub fn keyframe_interval_or_default(&self) -> u32 {
        self.keyframe_interval.unwrap_or(self.framerate)
    }

    /// Adjusts width and height for the given source dimensions using the
    /// configured [`ScaleMode`](crate::processing::scale::ScaleMode).
    ///
    /// Call this after creating the config from a preset and before creating
    /// the encoder so that the encoder is sized correctly for the source.
    pub fn resolve_for_source(mut self, source_width: u32, source_height: u32) -> Self {
        let (w, h) = self
            .scale_mode
            .resolve((source_width, source_height), (self.width, self.height));
        self.width = w;
        self.height = h;
        self
    }

    /// Computes a default bitrate for H.264-class codecs.
    ///
    /// Uses `pixels * factor * framerate_adjustment` where `factor` controls
    /// quality (0.07 for H.264, 0.05 for AV1).
    pub fn default_bitrate(&self, bits_per_pixel: f32) -> u64 {
        let pixels = self.width * self.height;
        let framerate_factor = 30.0 + (self.framerate as f32 - 30.) / 2.;
        (pixels as f32 * bits_per_pixel * framerate_factor).round() as u64
    }

    /// Returns the configured bitrate, or a default based on resolution and framerate.
    pub fn bitrate_or_default(&self, bits_per_pixel: f32) -> u64 {
        self.bitrate
            .unwrap_or_else(|| self.default_bitrate(bits_per_pixel))
    }
}

/// Configuration for creating an audio encoder.
///
/// Construct from an [`AudioFormat`] and [`AudioPreset`] via
/// [`from_preset`](Self::from_preset), then override with builder methods.
///
/// ```
/// # use rusty_codecs::format::{AudioEncoderConfig, AudioFormat, AudioPreset};
/// let config =
///     AudioEncoderConfig::from_preset(AudioFormat::stereo_48k(), AudioPreset::Hq).bitrate(96_000);
/// ```
#[derive(Clone, Debug)]
pub struct AudioEncoderConfig {
    /// Input sample rate in Hz.
    pub sample_rate: u32,
    /// Number of audio channels.
    pub channel_count: u32,
    /// Target bitrate in bits per second.
    pub bitrate: u64,
}

impl AudioEncoderConfig {
    /// Creates a config from an [`AudioFormat`] and [`AudioPreset`].
    pub fn from_preset(format: AudioFormat, preset: AudioPreset) -> Self {
        Self {
            sample_rate: format.sample_rate,
            channel_count: format.channel_count,
            bitrate: match preset {
                AudioPreset::Hq => 128_000,
                AudioPreset::Lq => 32_000,
            },
        }
    }

    /// Sets the sample rate in Hz.
    pub fn sample_rate(mut self, rate: u32) -> Self {
        self.sample_rate = rate;
        self
    }

    /// Sets the channel count.
    pub fn channel_count(mut self, count: u32) -> Self {
        self.channel_count = count;
        self
    }

    /// Sets the target bitrate in bits per second.
    pub fn bitrate(mut self, bitrate: u64) -> Self {
        self.bitrate = bitrate;
        self
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn new_cpu_with_format_rgba() {
        let data = vec![128u8; 4 * 4 * 4];
        let frame = VideoFrame::new_cpu_with_format(
            data.clone(),
            4,
            4,
            Duration::from_millis(100),
            PixelFormat::Rgba,
        );
        assert_eq!(frame.timestamp, Duration::from_millis(100));
        assert_eq!(frame.dimensions, [4, 4]);
        assert!(matches!(
            frame.data,
            FrameData::Packed {
                pixel_format: PixelFormat::Rgba,
                ..
            }
        ));
    }

    #[test]
    fn new_cpu_with_format_bgra() {
        let data = vec![64u8; 8 * 8 * 4];
        let frame = VideoFrame::new_cpu_with_format(data, 8, 8, Duration::ZERO, PixelFormat::Bgra);
        assert!(matches!(
            frame.data,
            FrameData::Packed {
                pixel_format: PixelFormat::Bgra,
                ..
            }
        ));
    }

    #[test]
    fn new_cpu_defaults_to_rgba() {
        let data = vec![0u8; 2 * 2 * 4];
        let frame = VideoFrame::new_cpu(data, 2, 2, Duration::ZERO);
        assert!(matches!(
            frame.data,
            FrameData::Packed {
                pixel_format: PixelFormat::Rgba,
                ..
            }
        ));
    }

    #[test]
    fn from_image_defaults_to_rgba() {
        let img = image::RgbaImage::new(4, 4);
        let frame = VideoFrame::from_image(img, Duration::from_secs(1));
        assert!(matches!(
            frame.data,
            FrameData::Packed {
                pixel_format: PixelFormat::Rgba,
                ..
            }
        ));
        assert_eq!(frame.dimensions, [4, 4]);
    }

    #[test]
    fn new_rgba_capture_frame() {
        let data = vec![255u8; 8 * 4 * 4];
        let frame = VideoFrame::new_rgba(data.into(), 8, 4, Duration::ZERO);
        assert_eq!(frame.timestamp, Duration::ZERO);
        assert_eq!(frame.dimensions, [8, 4]);
        assert!(!frame.is_gpu());
    }

    #[test]
    fn rgba_image_round_trip() {
        let data = vec![42u8; 2 * 2 * 4];
        let frame = VideoFrame::new_rgba(data.clone().into(), 2, 2, Duration::ZERO);
        let img = frame.rgba_image();
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
        assert_eq!(img.as_raw(), &data);
    }

    #[test]
    fn new_i420_frame() {
        let w = 4u32;
        let h = 4u32;
        let y = vec![16u8; (w * h) as usize];
        let u = vec![128u8; (w * h / 4) as usize];
        let v = vec![128u8; (w * h / 4) as usize];
        let frame = VideoFrame::new_i420(
            y.into(),
            u.into(),
            v.into(),
            w,
            h,
            Duration::from_millis(100),
        );
        assert_eq!(frame.dimensions, [4, 4]);
        assert_eq!(frame.timestamp, Duration::from_millis(100));
        assert!(!frame.is_gpu());
        // Should be able to convert to RGBA.
        let img = frame.rgba_image();
        assert_eq!(img.width(), 4);
        assert_eq!(img.height(), 4);
    }

    #[test]
    fn new_nv12_frame() {
        let w = 4u32;
        let h = 4u32;
        let nv12 = crate::format::Nv12Planes {
            y_data: vec![16u8; (w * h) as usize],
            y_stride: w,
            uv_data: vec![128u8; (w * h / 2) as usize],
            uv_stride: w,
            width: w,
            height: h,
        };
        let frame = VideoFrame::new_nv12(nv12, Duration::ZERO);
        assert_eq!(frame.dimensions, [4, 4]);
        assert!(!frame.is_gpu());
        let img = frame.rgba_image();
        assert_eq!(img.width(), 4);
    }
}
