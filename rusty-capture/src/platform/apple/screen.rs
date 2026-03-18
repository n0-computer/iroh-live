//! macOS screen capture via ScreenCaptureKit (macOS 12.3+).
//!
//! Produces IOSurface-backed `CVPixelBuffer` frames wrapped in [`AppleGpuFrame`].
//! The pixel data stays in GPU memory — no CPU copy. VideoToolbox encoder can
//! pass the CVPixelBuffer directly to `VTCompressionSessionEncodeFrame`; wgpu
//! renderer can import via `CVMetalTextureCache`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use anyhow::{Context, Result};
use screencapturekit::cm::CMSampleBuffer;
use screencapturekit::cm::CMTime;
use screencapturekit::shareable_content::SCShareableContent;
use screencapturekit::stream::SCStream;
use screencapturekit::stream::configuration::PixelFormat;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;
use screencapturekit::stream::output_trait::SCStreamOutputTrait;
use screencapturekit::stream::output_type::SCStreamOutputType;
use tracing::{info, warn};

use rusty_codecs::format::{
    AppleGpuFrame, GpuFrame, GpuPixelFormat, PixelFormat as RcPixelFormat, VideoFormat, VideoFrame,
};
use rusty_codecs::traits::VideoSource;

use crate::types::{MonitorInfo, ScreenConfig};

// CoreGraphics APIs for permission check.
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

/// Warns if Screen Recording permission has not been granted.
fn check_screen_capture_permission() {
    let granted = unsafe { CGPreflightScreenCaptureAccess() };
    if !granted {
        warn!(
            "Screen Recording permission not granted. \
             Grant access in System Settings > Privacy & Security > Screen Recording"
        );
    }
}

/// Derive scale factor from SCK display: pixel width / frame width.
/// Returns 2.0 on Retina, 1.0 on non-Retina.
fn display_scale_factor(display: &screencapturekit::shareable_content::SCDisplay) -> f64 {
    // SCDisplay.width() returns logical points — same as frame().width —
    // so we can't derive Retina scale from it. Query NSScreen instead.
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else {
        // Not on the main thread — fall back to safe Retina default.
        return 2.0;
    };

    let display_id = display.display_id();

    // Find the NSScreen matching this display via NSScreenNumber in deviceDescription.
    let scale = NSScreen::screens(mtm)
        .iter()
        .find(|screen| {
            let desc = screen.deviceDescription();
            let key = NSString::from_str("NSScreenNumber");
            desc.objectForKey(&key)
                .and_then(|val| {
                    // NSScreenNumber is an NSNumber containing the CGDirectDisplayID.
                    let num: *const objc2::runtime::AnyObject = objc2::rc::Retained::as_ptr(&val);
                    let id: u32 = unsafe { objc2::msg_send![num, unsignedIntValue] };
                    Some(id)
                })
                .is_some_and(|id| id == display_id)
        })
        .map(|screen| screen.backingScaleFactor())
        .unwrap_or(2.0);

    tracing::debug!(display_id, scale, "display scale factor from NSScreen");
    scale
}

/// Lists available macOS displays.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    check_screen_capture_permission();
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get shareable content: {e:?}"))?;
    let displays = content.displays();
    let mut result = Vec::new();
    for (i, display) in displays.iter().enumerate() {
        let width = display.width();
        let height = display.height();
        let frame = display.frame();
        result.push(MonitorInfo {
            backend: crate::CaptureBackend::ScreenCaptureKit,
            id: format!("macos-display-{}", display.display_id()),
            name: format!("Display {}", display.display_id()),
            position: [frame.x as i32, frame.y as i32],
            dimensions: [width, height],
            scale_factor: display_scale_factor(display) as f32,
            refresh_rate_hz: None,
            is_primary: i == 0,
        });
    }
    Ok(result)
}

/// Lists on-screen windows available for capture.
pub fn windows() -> Result<Vec<crate::types::WindowInfo>> {
    check_screen_capture_permission();
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get shareable content: {e:?}"))?;
    // Build display bounds for window→display mapping.
    let displays = content.displays();
    let display_bounds: Vec<_> = displays
        .iter()
        .map(|d| {
            let f = d.frame();
            (d.display_id(), f.x, f.y, f.x + f.width, f.y + f.height)
        })
        .collect();

    let mut result = Vec::new();
    for window in content.windows() {
        let frame = window.frame();
        // Skip tiny windows (status bar items, invisible helpers, menu extras)
        if frame.width < 100.0 || frame.height < 100.0 {
            continue;
        }
        let app_name = window
            .owning_application()
            .map(|app| app.application_name())
            .unwrap_or_default();
        let title = window.title().unwrap_or_default();
        let display_title = if title.is_empty() {
            app_name.clone()
        } else {
            title
        };
        if display_title.is_empty() {
            continue;
        }
        // Find which display contains the window center.
        let cx = frame.x + frame.width / 2.0;
        let cy = frame.y + frame.height / 2.0;
        let display_id = display_bounds
            .iter()
            .find(|(_, x0, y0, x1, y1)| cx >= *x0 && cx < *x1 && cy >= *y0 && cy < *y1)
            .map(|(id, ..)| *id);
        result.push(crate::types::WindowInfo {
            backend: crate::CaptureBackend::ScreenCaptureKit,
            id: window.window_id(),
            title: display_title,
            app_name,
            dimensions: [frame.width as u32, frame.height as u32],
            display_id,
            is_on_screen: window.is_on_screen(),
        });
    }
    Ok(result)
}

/// Actual pixel dimensions observed from the CVPixelBuffer callback.
/// SCK may deliver at a different resolution than requested (e.g. Retina scaling).
struct ActualDimensions {
    width: AtomicU32,
    height: AtomicU32,
}

struct FrameHandler {
    tx: mpsc::SyncSender<VideoFrame>,
    capture_start: Instant,
    actual_dims: Arc<ActualDimensions>,
}

impl SCStreamOutputTrait for FrameHandler {
    fn did_output_sample_buffer(
        &self,
        sample_buffer: CMSampleBuffer,
        _of_type: SCStreamOutputType,
    ) {
        let Some(pixel_buffer) = sample_buffer.image_buffer() else {
            return;
        };

        let width = pixel_buffer.width() as u32;
        let height = pixel_buffer.height() as u32;

        // Update actual dimensions on first frame (or if source resizes).
        self.actual_dims.width.store(width, Ordering::Relaxed);
        self.actual_dims.height.store(height, Ordering::Relaxed);

        // Zero-copy: retain the CVPixelBuffer and wrap it as a GPU frame.
        let raw = pixel_buffer.as_ptr();
        let gpu_frame =
            unsafe { AppleGpuFrame::from_raw(raw, width, height, GpuPixelFormat::Bgra) };
        let frame = VideoFrame::new_gpu(
            GpuFrame::new(Arc::new(gpu_frame)),
            self.capture_start.elapsed(),
        );

        // Drop frame if channel is full — backpressure from the callback
        // thread. The consumer drains to latest anyway.
        let _ = self.tx.try_send(frame);
    }
}

/// macOS screen capturer via ScreenCaptureKit.
#[derive(derive_more::Debug)]
pub struct MacScreenCapturer {
    /// Requested dimensions (logical points for windows, display pixels for screens).
    /// Used as fallback before the first frame arrives.
    requested_width: u32,
    requested_height: u32,
    /// Actual pixel dimensions from the CVPixelBuffer callback.
    /// Updated on every frame — authoritative source of truth.
    #[debug(skip)]
    actual_dims: Arc<ActualDimensions>,
    #[debug(skip)]
    rx: mpsc::Receiver<VideoFrame>, // bounded via SyncSender
    #[debug(skip)]
    stream: SCStream,
}

impl MacScreenCapturer {
    /// Creates a window capturer for a specific window by ID.
    pub fn new_window(window_id: u32, config: &ScreenConfig) -> Result<Self> {
        check_screen_capture_permission();
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

        let window = content
            .windows()
            .into_iter()
            .find(|w| w.window_id() == window_id)
            .context("window not found")?;

        // Window frame is in logical points. On Retina (2x), request double
        // to capture at native pixel resolution. The actual CVPixelBuffer
        // dimensions are tracked via actual_dims from the callback.
        let frame = window.frame();
        let displays = content.displays();
        let cx = frame.x + frame.width / 2.0;
        let cy = frame.y + frame.height / 2.0;
        let scale = displays
            .iter()
            .find(|d| {
                let f = d.frame();
                cx >= f.x && cx < f.x + f.width && cy >= f.y && cy < f.y + f.height
            })
            .map(|d| display_scale_factor(d))
            .unwrap_or(2.0);
        let width = (frame.width * scale) as u32;
        let height = (frame.height * scale) as u32;

        let filter = SCContentFilter::create().with_window(&window).build();

        Self::start_stream(width, height, filter, config)
    }

    /// Creates a screen capturer for the given monitor.
    ///
    /// Captures from the display matching the monitor's ID. Falls back to the
    /// first available display if the ID is not recognized.
    pub fn new(monitor: &MonitorInfo, config: &ScreenConfig) -> Result<Self> {
        check_screen_capture_permission();
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

        let display_id: Option<u32> = monitor
            .id
            .strip_prefix("macos-display-")
            .and_then(|s| s.parse().ok());
        let displays = content.displays();
        let display = if let Some(did) = display_id {
            displays
                .into_iter()
                .find(|d| d.display_id() == did)
                .context("display not found")?
        } else {
            displays
                .into_iter()
                .next()
                .context("no displays available")?
        };

        let width = display.width() as u32;
        let height = display.height() as u32;

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();

        Self::start_stream(width, height, filter, config)
    }

    fn start_stream(
        width: u32,
        height: u32,
        filter: SCContentFilter,
        config: &ScreenConfig,
    ) -> Result<Self> {
        let mut stream_config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height)
            .with_shows_cursor(config.show_cursor)
            .with_pixel_format(PixelFormat::BGRA)
            .with_queue_depth(8);

        if let Some(fps) = config.target_fps {
            stream_config = stream_config.with_minimum_frame_interval(&CMTime::new(1, fps as i32));
        }

        let actual_dims = Arc::new(ActualDimensions {
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
        });

        let (frame_tx, frame_rx) = mpsc::sync_channel(2);
        let handler = FrameHandler {
            tx: frame_tx,
            capture_start: Instant::now(),
            actual_dims: Arc::clone(&actual_dims),
        };

        let mut stream = SCStream::new(&filter, &stream_config);
        stream.add_output_handler(handler, SCStreamOutputType::Screen);
        stream
            .start_capture()
            .map_err(|e| anyhow::anyhow!("failed to start screen capture: {e:?}"))?;

        info!(width, height, "macOS screen capture started");

        Ok(Self {
            requested_width: width,
            requested_height: height,
            actual_dims,
            rx: frame_rx,
            stream,
        })
    }
}

impl VideoSource for MacScreenCapturer {
    fn name(&self) -> &str {
        "macos-screen"
    }

    fn format(&self) -> VideoFormat {
        // Use actual pixel buffer dimensions if we've received a frame,
        // otherwise fall back to the requested dimensions.
        let w = self.actual_dims.width.load(Ordering::Relaxed);
        let h = self.actual_dims.height.load(Ordering::Relaxed);
        let (w, h) = if w > 0 && h > 0 {
            (w, h)
        } else {
            (self.requested_width, self.requested_height)
        };
        VideoFormat {
            pixel_format: RcPixelFormat::Bgra,
            dimensions: [w, h],
        }
    }

    fn start(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.stream
            .stop_capture()
            .map_err(|e| anyhow::anyhow!("failed to stop screen capture: {e:?}"))
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let mut latest = None;
        while let Ok(frame) = self.rx.try_recv() {
            latest = Some(frame);
        }
        Ok(latest)
    }
}
