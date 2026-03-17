//! macOS screen capture via ScreenCaptureKit (macOS 12.3+).
//!
//! Produces IOSurface-backed `CVPixelBuffer` frames. Currently copies to CPU
//! RGBA; the zero-copy IOSurface path (direct to VideoToolbox encoder) is
//! planned via `NativeFrameHandle::IoSurface`.
//!
//! # Zero-Copy Plan
//!
//! ScreenCaptureKit delivers `CMSampleBuffer` containing IOSurface-backed
//! `CVPixelBuffer`. For zero-copy:
//!
//! 1. Add `NativeFrameHandle::IoSurface(IoSurfaceRef)` behind `cfg(target_os = "macos")`.
//! 2. Implement `GpuFrameInner` for an `AppleGpuFrame` that holds the
//!    `CVPixelBuffer` (preventing the IOSurface from being recycled).
//! 3. `download_rgba()` calls `CVPixelBufferLockBaseAddress` + memcpy.
//! 4. `native_handle()` returns the IOSurface ref.
//! 5. VideoToolbox encoder's `VTCompressionSessionEncodeFrame` accepts the
//!    `CVPixelBuffer` directly — true zero-copy GPU encode.

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

use rusty_codecs::format::{PixelFormat as RcPixelFormat, VideoFormat, VideoFrame};
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

        let guard = match pixel_buffer.lock_read_only() {
            Ok(g) => g,
            Err(_) => return,
        };

        let bytes_per_row = guard.bytes_per_row();
        let height = guard.height();
        let width = guard.width();
        let data = guard.as_slice();

        // Validate the buffer is large enough for the reported dimensions.
        let expected = bytes_per_row * height;
        if data.len() < expected {
            return;
        }

        // ScreenCaptureKit delivers BGRA. Convert to RGBA, stripping row
        // padding (bytes_per_row may exceed width * 4 due to alignment).
        let row_bytes = width * 4;
        let mut rgba = vec![0u8; row_bytes * height];
        for y in 0..height {
            let src_row = &data[y * bytes_per_row..y * bytes_per_row + row_bytes];
            let dst_row = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
            // Swap B and R channels in-place: BGRA → RGBA.
            for (src, dst) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                dst[0] = src[2]; // R ← B
                dst[1] = src[1]; // G
                dst[2] = src[0]; // B ← R
                dst[3] = src[3]; // A
            }
        }

        let frame = VideoFrame::new_rgba(
            rgba.into(),
            width as u32,
            height as u32,
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
    /// Creates a screen capturer for the given monitor.
    ///
    /// Captures from the display matching the monitor's ID. Falls back to the
    /// first available display if the ID is not recognized.
    pub fn new(monitor: &MonitorInfo, config: &ScreenConfig) -> Result<Self> {
        check_screen_capture_permission();
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

        // Try to find the display matching the monitor ID, fall back to first.
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

        let width = display.width();
        let height = display.height();

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();

        let mut stream_config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height)
            .with_shows_cursor(config.show_cursor)
            // Request BGRA for simple byte-swap to RGBA (avoids YCbCr→RGB conversion).
            .with_pixel_format(PixelFormat::BGRA)
            .with_queue_depth(8);

        if let Some(fps) = config.target_fps {
            stream_config = stream_config.with_minimum_frame_interval(&CMTime::new(1, fps as i32));
        }

        // Bounded: 2-frame buffer. The callback drops frames via try_send
        // when the consumer falls behind.
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
            width,
            height,
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
            pixel_format: RcPixelFormat::Rgba,
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
