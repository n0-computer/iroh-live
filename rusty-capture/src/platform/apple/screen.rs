//! macOS screen capture via ScreenCaptureKit (macOS 12.3+).
//!
//! Produces IOSurface-backed `CVPixelBuffer` frames wrapped in [`AppleGpuFrame`].
//! The pixel data stays in GPU memory — no CPU copy. VideoToolbox encoder can
//! pass the CVPixelBuffer directly to `VTCompressionSessionEncodeFrame`; wgpu
//! renderer can import via `CVMetalTextureCache`.

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

/// SCK provides physical pixels; scale factor is handled internally.
/// Returns 1.0 unconditionally for now.
fn display_scale_factor() -> f32 {
    1.0
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
            scale_factor: display_scale_factor(),
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

struct FrameHandler {
    tx: mpsc::SyncSender<VideoFrame>,
    capture_start: Instant,
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

        let width = pixel_buffer.width();
        let height = pixel_buffer.height();

        // Zero-copy: retain the CVPixelBuffer and wrap it as a GPU frame.
        let raw = pixel_buffer.as_ptr();
        let gpu_frame = unsafe {
            AppleGpuFrame::from_raw(raw, width as u32, height as u32, GpuPixelFormat::Bgra)
        };
        let frame = VideoFrame::new_gpu(
            GpuFrame::new(std::sync::Arc::new(gpu_frame)),
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
    width: u32,
    height: u32,
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

        let frame = window.frame();
        let width = frame.width as u32;
        let height = frame.height as u32;

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

        let (frame_tx, frame_rx) = mpsc::sync_channel(2);
        let handler = FrameHandler {
            tx: frame_tx,
            capture_start: Instant::now(),
        };

        let mut stream = SCStream::new(&filter, &stream_config);
        stream.add_output_handler(handler, SCStreamOutputType::Screen);
        stream
            .start_capture()
            .map_err(|e| anyhow::anyhow!("failed to start screen capture: {e:?}"))?;

        info!(width, height, "macOS screen capture started");

        Ok(Self {
            width: width as u32,
            height: height as u32,
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
        VideoFormat {
            pixel_format: RcPixelFormat::Bgra,
            dimensions: [self.width, self.height],
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
