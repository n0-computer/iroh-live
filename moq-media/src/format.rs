use std::cell::OnceCell;
use std::fmt;
#[cfg(target_os = "linux")]
use std::os::unix::io::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use hang::catalog::AudioConfig;
use image::RgbaImage;
use strum::{Display, EnumString, VariantNames};

#[derive(Copy, Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channel_count: u32,
}

impl AudioFormat {
    pub fn mono_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 1,
        }
    }

    pub fn stereo_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channel_count: 2,
        }
    }

    pub fn from_hang_config(config: &AudioConfig) -> Self {
        Self {
            channel_count: config.channel_count,
            sample_rate: config.sample_rate,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub enum PixelFormat {
    #[default]
    Rgba,
    Bgra,
}

#[derive(Clone, Debug)]
pub struct VideoFormat {
    pub pixel_format: PixelFormat,
    pub dimensions: [u32; 2],
}

#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub format: VideoFormat,
    pub raw: bytes::Bytes,
}

/// An encoded media packet, independent of transport.
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

impl From<hang::container::OrderedFrame> for MediaPacket {
    fn from(f: hang::container::OrderedFrame) -> Self {
        let is_keyframe = f.is_keyframe();
        Self {
            timestamp: f.timestamp.into(),
            payload: f.payload,
            is_keyframe,
        }
    }
}

#[derive(Debug)]
pub struct EncodedFrame {
    pub is_keyframe: bool,
    pub timestamp: Duration,
    pub payload: bytes::Bytes,
}

impl EncodedFrame {
    /// Convert to a hang `Frame` for MoQ transport.
    pub fn to_hang_frame(&self) -> hang::container::Frame {
        hang::container::Frame {
            timestamp: hang::container::Timestamp::from_micros(self.timestamp.as_micros() as u64)
                .expect("timestamp overflow"),
            payload: self.payload.clone().into(),
        }
    }
}

/// CPU-resident RGBA pixel data backed by an [`RgbaImage`].
#[derive(derive_more::Debug, Clone)]
pub struct CpuFrame {
    #[debug(skip)]
    pub image: RgbaImage,
    pub pixel_format: PixelFormat,
}

impl CpuFrame {
    /// Returns the frame width.
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    /// Returns the frame height.
    pub fn height(&self) -> u32 {
        self.image.height()
    }
}

/// GPU-resident frame from a hardware decoder.
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
    pub fn new(inner: Arc<dyn GpuFrameInner>) -> Self {
        Self { inner }
    }

    pub fn download(&self) -> anyhow::Result<CpuFrame> {
        self.inner.download()
    }

    pub fn dimensions(&self) -> (u32, u32) {
        self.inner.dimensions()
    }

    pub fn gpu_pixel_format(&self) -> GpuPixelFormat {
        self.inner.gpu_pixel_format()
    }

    pub fn download_nv12(&self) -> Option<anyhow::Result<Nv12Planes>> {
        self.inner.download_nv12()
    }

    #[cfg(target_os = "linux")]
    pub fn dma_buf_info(&self) -> Option<&DmaBufInfo> {
        self.inner.dma_buf_info()
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
    /// Download the GPU frame to a CPU RGBA buffer.
    fn download(&self) -> anyhow::Result<CpuFrame>;
    /// Native pixel format on the GPU (NV12, I420, etc.).
    fn gpu_pixel_format(&self) -> GpuPixelFormat;
    /// Frame dimensions.
    fn dimensions(&self) -> (u32, u32);
    /// Download NV12 plane data for GPU-side color conversion.
    /// Returns None if the frame is not NV12 or doesn't support plane download.
    fn download_nv12(&self) -> Option<anyhow::Result<Nv12Planes>> {
        None
    }
    /// DMA-BUF info for zero-copy GPU import, if available.
    #[cfg(target_os = "linux")]
    fn dma_buf_info(&self) -> Option<&DmaBufInfo> {
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

/// Native GPU pixel formats from hardware decoders.
#[derive(Debug, Clone, Copy)]
pub enum GpuPixelFormat {
    Nv12,
}

/// Backing storage for a decoded frame.
#[derive(derive_more::Debug)]
pub enum FrameBuffer {
    Cpu(CpuFrame),
    #[debug("Gpu({:?})", _0)]
    Gpu(GpuFrame),
}

/// A decoded video frame that may live on CPU or GPU.
#[derive(derive_more::Debug)]
pub struct DecodedVideoFrame {
    pub buffer: FrameBuffer,
    pub timestamp: Duration,
    /// Lazy CPU image cache for backward-compat `img()`.
    #[debug(skip)]
    cached_rgba: OnceCell<RgbaImage>,
}

impl DecodedVideoFrame {
    /// Creates a new CPU-backed frame from raw RGBA data.
    pub fn new_cpu(data: Vec<u8>, width: u32, height: u32, timestamp: Duration) -> Self {
        let image = RgbaImage::from_raw(width, height, data)
            .expect("RGBA data size does not match dimensions");
        Self::from_image(image, timestamp)
    }

    /// Creates a new CPU-backed frame from an existing [`RgbaImage`].
    pub fn from_image(image: RgbaImage, timestamp: Duration) -> Self {
        Self {
            buffer: FrameBuffer::Cpu(CpuFrame {
                image,
                pixel_format: PixelFormat::Rgba,
            }),
            timestamp,
            cached_rgba: OnceCell::new(),
        }
    }

    /// Creates a new GPU-backed frame.
    pub fn new_gpu(gpu: GpuFrame, timestamp: Duration) -> Self {
        Self {
            buffer: FrameBuffer::Gpu(gpu),
            timestamp,
            cached_rgba: OnceCell::new(),
        }
    }

    /// Returns the frame dimensions as `(width, height)`.
    pub fn dimensions(&self) -> (u32, u32) {
        match &self.buffer {
            FrameBuffer::Cpu(f) => (f.width(), f.height()),
            FrameBuffer::Gpu(f) => f.dimensions(),
        }
    }

    /// Whether this frame lives on the GPU.
    pub fn is_gpu(&self) -> bool {
        matches!(&self.buffer, FrameBuffer::Gpu(_))
    }

    /// Access the GPU frame directly for zero-copy rendering.
    pub fn gpu_frame(&self) -> Option<&GpuFrame> {
        match &self.buffer {
            FrameBuffer::Gpu(f) => Some(f),
            _ => None,
        }
    }

    /// Returns the frame as a CPU RGBA image.
    ///
    /// For CPU frames, returns a zero-copy reference to the underlying image.
    /// For GPU frames, downloads from the GPU on first call and caches.
    pub fn img(&self) -> &RgbaImage {
        match &self.buffer {
            FrameBuffer::Cpu(cpu) => &cpu.image,
            FrameBuffer::Gpu(_) => self.cached_rgba.get_or_init(|| {
                let FrameBuffer::Gpu(gpu) = &self.buffer else {
                    unreachable!()
                };
                let cpu = gpu.download().expect("GPU frame download failed");
                cpu.image
            }),
        }
    }
}

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
    pub fn all() -> [Self; 4] {
        [Self::P180, Self::P360, Self::P720, Self::P1080]
    }

    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::P180 => (320, 180),
            Self::P360 => (640, 360),
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
        }
    }

    pub fn width(&self) -> u32 {
        self.dimensions().0
    }

    pub fn height(&self) -> u32 {
        self.dimensions().1
    }

    pub fn fps(&self) -> u32 {
        30
    }
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq)]
#[strum(serialize_all = "lowercase")]
pub enum AudioPreset {
    Hq,
    Lq,
}

#[derive(Debug, Clone, Copy, Display, EnumString, VariantNames, Eq, PartialEq, Default)]
#[strum(serialize_all = "lowercase")]
pub enum Quality {
    Highest,
    #[default]
    High,
    Mid,
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

#[derive(Clone, Debug, Default)]
pub struct DecodeConfig {
    pub pixel_format: PixelFormat,
    pub backend: DecoderBackend,
}

#[derive(Clone, Debug, Default)]
pub struct PlaybackConfig {
    pub decode_config: DecodeConfig,
    pub quality: Quality,
}

/// Configuration for creating a video encoder.
///
/// Construct from a [`VideoPreset`] via [`from_preset`](Self::from_preset),
/// then override individual fields with the builder methods.
///
/// ```
/// # use moq_media::format::{VideoEncoderConfig, VideoPreset};
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
/// # use moq_media::format::{AudioEncoderConfig, AudioFormat, AudioPreset};
/// let config = AudioEncoderConfig::from_preset(AudioFormat::stereo_48k(), AudioPreset::Hq)
///     .bitrate(96_000);
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
