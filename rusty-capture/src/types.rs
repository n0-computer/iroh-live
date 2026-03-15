//! Capture device metadata and configuration types.

/// Identifies a capture backend.
///
/// Each variant corresponds to a platform-specific implementation. Use
/// [`CameraCapturer::list_backends`](crate::CameraCapturer::list_backends) or
/// [`ScreenCapturer::list_backends`](crate::ScreenCapturer::list_backends) to
/// discover which backends are compiled in and available at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureBackend {
    /// PipeWire portal (Linux Wayland). Supports screen + camera with DMA-BUF zero-copy.
    PipeWire,
    /// Video4Linux2 (Linux). Camera only, MMAP + optional DMA-BUF export.
    V4l2,
    /// X11 MIT-SHM (Linux). Screen only, CPU-mapped shared memory.
    X11,
    /// ScreenCaptureKit (macOS 12.3+). Screen only.
    ScreenCaptureKit,
    /// AVFoundation (macOS/iOS). Camera only.
    AVFoundation,
}

impl std::fmt::Display for CaptureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PipeWire => write!(f, "PipeWire"),
            Self::V4l2 => write!(f, "V4L2"),
            Self::X11 => write!(f, "X11"),
            Self::ScreenCaptureKit => write!(f, "ScreenCaptureKit"),
            Self::AVFoundation => write!(f, "AVFoundation"),
        }
    }
}

/// Describes a display monitor available for screen capture.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    /// Which capture backend provides this monitor.
    pub backend: CaptureBackend,
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
    /// Which capture backend provides this camera.
    pub backend: CaptureBackend,
    /// Platform-specific identifier (e.g. `/dev/video0`, AVFoundation uniqueID).
    pub id: String,
    /// Human-readable device name.
    pub name: String,
    /// Formats the camera can produce.
    pub supported_formats: Vec<CameraFormat>,
}

/// A resolution + frame rate + pixel format combination a camera supports.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraFormat {
    /// Frame dimensions `[width, height]`.
    pub dimensions: [u32; 2],
    /// Frames per second.
    pub fps: f32,
    /// Pixel format produced by the camera at this setting.
    pub pixel_format: CapturePixelFormat,
}

impl CameraFormat {
    /// Returns the total pixel count (width × height).
    pub fn pixel_count(&self) -> u64 {
        self.dimensions[0] as u64 * self.dimensions[1] as u64
    }
}

/// Strategy for selecting a camera format from the list of supported formats.
#[derive(Debug, Clone, Default)]
pub enum CameraSelector {
    /// Selects the format with the highest frame rate.
    /// Ties broken by highest resolution.
    HighestFramerate,
    /// Selects the format with the highest resolution.
    /// Ties broken by highest frame rate.
    #[default]
    HighestResolution,
    /// Selects the format closest to the target resolution.
    /// Prefers exact match, then next larger, then next smaller.
    /// Ties broken by highest frame rate.
    TargetResolution(u32, u32),
}

impl CameraSelector {
    /// Picks the best format from a slice of references.
    fn select_refs<'a>(&self, formats: &[&'a CameraFormat]) -> Option<&'a CameraFormat> {
        if formats.is_empty() {
            return None;
        }
        // Delegate to select() on a view. We re-implement to avoid cloning.
        let idx = self.select_index(formats.iter().copied())?;
        Some(formats[idx])
    }

    /// Returns the index of the best format from an iterator.
    fn select_index<'a>(
        &self,
        formats: impl Iterator<Item = &'a CameraFormat> + Clone,
    ) -> Option<usize> {
        match self {
            Self::HighestFramerate => formats
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    a.fps
                        .partial_cmp(&b.fps)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.pixel_count().cmp(&b.pixel_count()))
                })
                .map(|(i, _)| i),
            Self::HighestResolution => formats
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    a.pixel_count().cmp(&b.pixel_count()).then_with(|| {
                        a.fps
                            .partial_cmp(&b.fps)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                })
                .map(|(i, _)| i),
            Self::TargetResolution(tw, th) => {
                let target = *tw as u64 * *th as u64;
                let all: Vec<_> = formats.collect();
                // Prefer smallest resolution >= target, break ties by fps.
                let above = all
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.pixel_count() >= target)
                    .min_by(|(_, a), (_, b)| {
                        a.pixel_count().cmp(&b.pixel_count()).then_with(|| {
                            b.fps
                                .partial_cmp(&a.fps)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                    });
                // Fallback: largest resolution < target, break ties by fps.
                let below = || {
                    all.iter()
                        .enumerate()
                        .filter(|(_, f)| f.pixel_count() < target)
                        .max_by(|(_, a), (_, b)| {
                            a.pixel_count().cmp(&b.pixel_count()).then_with(|| {
                                a.fps
                                    .partial_cmp(&b.fps)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                        })
                };
                above.or_else(below).map(|(i, _)| i)
            }
        }
    }

    /// Picks the best format from a list according to this strategy.
    ///
    /// Returns `None` if `formats` is empty.
    pub fn select<'a>(&self, formats: &'a [CameraFormat]) -> Option<&'a CameraFormat> {
        let idx = self.select_index(formats.iter())?;
        Some(&formats[idx])
    }
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
    /// Strategy for picking resolution and frame rate from the camera's
    /// supported format list. Defaults to [`CameraSelector::HighestResolution`].
    pub selector: CameraSelector,
    /// Preferred pixel format. [`CapturePixelFormat::Nv12`] enables zero-copy
    /// on platforms with DMA-BUF or IOSurface support. When set, only formats
    /// matching this pixel format are considered.
    pub preferred_format: Option<CapturePixelFormat>,
    /// Attempt DMA-BUF / IOSurface zero-copy if the platform supports it.
    /// Falls back to CPU buffers transparently on failure.
    pub zero_copy: bool,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            selector: CameraSelector::default(),
            preferred_format: None,
            zero_copy: true,
        }
    }
}

impl CameraConfig {
    /// Selects the best format from a camera's supported formats.
    ///
    /// Filters by [`preferred_format`](Self::preferred_format) when set,
    /// then applies the [`selector`](Self::selector) strategy.
    pub fn select_format<'a>(&self, formats: &'a [CameraFormat]) -> Option<&'a CameraFormat> {
        if let Some(pf) = self.preferred_format {
            let filtered: Vec<&CameraFormat> =
                formats.iter().filter(|f| f.pixel_format == pf).collect();
            if let Some(f) = self.selector.select_refs(&filtered) {
                return Some(f);
            }
        }
        self.selector.select(formats)
    }
}

/// Configuration for creating a screen capturer.
#[derive(Debug, Clone)]
pub struct ScreenConfig {
    /// Target capture frame rate. The backend may not hit this exactly.
    pub target_fps: Option<f32>,
    /// Whether to include the cursor in captured frames.
    pub show_cursor: bool,
    /// Optional restore token from a previous PipeWire ScreenCast portal session.
    ///
    /// When provided, the portal may skip the user dialog and reuse the
    /// previous source selection. Obtain the token from
    /// [`PipeWireScreenCapturer::pipewire_restore_token`] after a successful
    /// session.
    pub pipewire_restore_token: Option<String>,
}

impl Default for ScreenConfig {
    fn default() -> Self {
        Self {
            target_fps: Some(30.0),
            show_cursor: true,
            pipewire_restore_token: None,
        }
    }
}
