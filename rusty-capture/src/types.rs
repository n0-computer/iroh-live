//! Capture device metadata and configuration types.

/// Describes a display monitor available for screen capture.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    /// Platform-specific identifier (e.g. X11 screen number, PipeWire node id).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Position in the virtual desktop coordinate space `[x, y]`.
    pub position: [i32; 2],
    /// Native pixel dimensions `[width, height]` (before scaling).
    pub dimensions: [u32; 2],
    /// HiDPI scale factor (1.0 = no scaling, 2.0 = Retina).
    pub scale_factor: f32,
    /// Refresh rate in Hz, if known.
    pub refresh_rate_hz: Option<f32>,
    /// Whether this is the primary display.
    pub is_primary: bool,
}

/// Describes a camera device available for capture.
#[derive(Debug, Clone)]
pub struct CameraInfo {
    /// Platform-specific identifier (e.g. `/dev/video0`, AVFoundation uniqueID).
    pub id: String,
    /// Human-readable device name.
    pub name: String,
    /// Formats the camera can produce.
    pub supported_formats: Vec<CameraFormat>,
}

/// A resolution + frame rate + pixel format combination a camera supports.
#[derive(Debug, Clone)]
pub struct CameraFormat {
    /// Frame dimensions `[width, height]`.
    pub dimensions: [u32; 2],
    /// Frames per second.
    pub fps: f32,
    /// Pixel format produced by the camera at this setting.
    pub pixel_format: CapturePixelFormat,
}

/// Pixel formats that capture devices may produce.
///
/// Broader than [`rusty_codecs::format::PixelFormat`] because cameras output
/// compressed and YUV formats that the codec layer does not enumerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePixelFormat {
    /// RGBA 8-bit per channel, 32 bpp.
    Rgba,
    /// BGRA 8-bit per channel, 32 bpp (common on Windows, macOS CoreGraphics).
    Bgra,
    /// Semi-planar YUV 4:2:0 (Y plane + interleaved UV). Preferred for
    /// hardware encoders and DMA-BUF zero-copy paths.
    Nv12,
    /// Planar YUV 4:2:0 (Y + U + V planes). Common V4L2 output.
    I420,
    /// Packed YUV 4:2:2. Common USB camera output.
    Yuyv,
    /// Motion JPEG compressed frames.
    Mjpeg,
    /// RGB 24-bit (no alpha).
    Rgb,
    /// Grayscale 8-bit.
    Gray,
}

impl CapturePixelFormat {
    /// Converts a V4L2 FourCC byte string to a capture pixel format.
    ///
    /// Returns `None` for unsupported or raw formats (e.g. Bayer patterns).
    pub fn from_v4l2_fourcc(fourcc: &[u8; 4]) -> Option<Self> {
        match fourcc {
            b"YUYV" => Some(Self::Yuyv),
            b"MJPG" => Some(Self::Mjpeg),
            b"NV12" => Some(Self::Nv12),
            b"YU12" | b"I420" => Some(Self::I420),
            b"RGBP" | b"RGB3" => Some(Self::Rgb),
            b"GREY" => Some(Self::Gray),
            _ => None,
        }
    }

    /// Returns the V4L2 FourCC byte string for this format.
    ///
    /// For formats that don't have a direct V4L2 representation (Rgba, Bgra),
    /// falls back to RGB3 for capture and converts in software.
    pub fn to_v4l2_fourcc(&self) -> [u8; 4] {
        match self {
            Self::Yuyv => *b"YUYV",
            Self::Mjpeg => *b"MJPG",
            Self::Nv12 => *b"NV12",
            Self::I420 => *b"YU12",
            Self::Rgb => *b"RGB3",
            Self::Gray => *b"GREY",
            // No direct V4L2 equivalent — request RGB and convert.
            Self::Rgba | Self::Bgra => *b"RGB3",
        }
    }
}

/// Configuration for creating a camera capturer.
#[derive(Debug, Clone)]
pub struct CameraConfig {
    /// Preferred resolution `[width, height]`. The backend picks the closest
    /// supported resolution at or below these dimensions.
    pub preferred_resolution: Option<[u32; 2]>,
    /// Preferred frame rate. The backend picks the closest supported fps.
    pub preferred_fps: Option<f32>,
    /// Preferred pixel format. [`CapturePixelFormat::Nv12`] enables zero-copy
    /// on platforms with DMA-BUF or IOSurface support.
    pub preferred_format: Option<CapturePixelFormat>,
    /// Attempt DMA-BUF / IOSurface zero-copy if the platform supports it.
    /// Falls back to CPU buffers transparently on failure.
    pub zero_copy: bool,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            preferred_resolution: None,
            preferred_fps: None,
            preferred_format: None,
            zero_copy: true,
        }
    }
}

/// Configuration for creating a screen capturer.
#[derive(Debug, Clone)]
pub struct ScreenConfig {
    /// Target capture frame rate. The backend may not hit this exactly.
    pub target_fps: Option<f32>,
    /// Whether to include the cursor in captured frames.
    pub show_cursor: bool,
}

impl Default for ScreenConfig {
    fn default() -> Self {
        Self {
            target_fps: Some(30.0),
            show_cursor: true,
        }
    }
}
