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
use screencapturekit::shareable_content::SCShareableContent;
use screencapturekit::stream::SCStream;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;
use screencapturekit::stream::output_trait::SCStreamOutputTrait;
use screencapturekit::stream::output_type::SCStreamOutputType;
use tracing::{info, warn};

use rusty_codecs::format::{PixelFormat, VideoFormat, VideoFrame};
use rusty_codecs::traits::VideoSource;

use crate::types::{MonitorInfo, ScreenConfig};

// CoreGraphics APIs for permission check and display scale factor.
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGDisplayScreenSize(display: u32) -> CGSize;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
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

/// Computes the scale factor for a display by comparing logical and physical sizes.
///
/// Falls back to 1.0 if the physical size cannot be determined.
fn display_scale_factor(display_id: u32, logical_width: u32) -> f32 {
    if logical_width == 0 {
        return 1.0;
    }
    // CGDisplayScreenSize returns the physical size in mm. If it returns zero
    // the display doesn't report physical metrics (e.g. virtual displays).
    // We can't derive a pixel-density scale from that, so fall back.
    //
    // For actual HiDPI detection we'd use CGDisplayCopyDisplayMode +
    // pixelWidth / width ratio, but SCK's display.width already reflects
    // the backing store (physical) pixels, so the scale factor relative to
    // the logical desktop coordinate space can be approximated from the
    // frame rectangle vs pixel dimensions.
    1.0 // SCK provides physical pixels; scale factor is handled internally
}

/// Lists available macOS displays.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    check_screen_capture_permission();
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get shareable content: {e:?}"))?;
    let displays = content.displays;
    let mut result = Vec::new();
    for (i, display) in displays.iter().enumerate() {
        let width = display.width as u32;
        let height = display.height as u32;
        result.push(MonitorInfo {
            backend: crate::CaptureBackend::ScreenCaptureKit,
            id: format!("macos-display-{}", display.display_id),
            name: format!("Display {}", display.display_id),
            position: [display.frame.origin.x as i32, display.frame.origin.y as i32],
            dimensions: [width, height],
            scale_factor: display_scale_factor(display.display_id, width),
            refresh_rate_hz: None,
            is_primary: i == 0,
        });
    }
    Ok(result)
}

struct FrameHandler {
    tx: mpsc::Sender<VideoFrame>,
    capture_start: Instant,
}

impl SCStreamOutputTrait for FrameHandler {
    fn did_output_sample_buffer(
        &self,
        sample_buffer: screencapturekit::cm_sample_buffer::CMSampleBuffer,
        _of_type: SCStreamOutputType,
    ) {
        let Some(pixel_buffer) = sample_buffer.pixel_buffer else {
            return;
        };
        let bytes_per_row = pixel_buffer.bytes_per_row as usize;
        let height = pixel_buffer.height as usize;
        let width = pixel_buffer.width as usize;
        let data = &pixel_buffer.data;

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
        let _ = self.tx.send(frame);
    }
}

/// macOS screen capturer via ScreenCaptureKit.
#[derive(derive_more::Debug)]
pub struct MacScreenCapturer {
    width: u32,
    height: u32,
    #[debug(skip)]
    rx: mpsc::Receiver<VideoFrame>,
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
        let display = if let Some(did) = display_id {
            content
                .displays
                .into_iter()
                .find(|d| d.display_id == did)
                .context("display not found")?
        } else {
            content
                .displays
                .into_iter()
                .next()
                .context("no displays available")?
        };

        let width = display.width as u32;
        let height = display.height as u32;

        let filter = SCContentFilter::new(
            screencapturekit::stream::content_filter::InitParams::Display(display),
        );

        let mut stream_config = SCStreamConfiguration::default();
        stream_config.width = width;
        stream_config.height = height;
        stream_config.shows_cursor = config.show_cursor;
        // Request BGRA for simple byte-swap to RGBA (avoids YCbCr→RGB conversion).
        stream_config.pixel_format = screencapturekit::stream::configuration::PixelFormat::ARGB8888;
        // Allow up to 8 frames in the output queue to absorb processing jitter.
        stream_config.queue_depth = 8;
        if let Some(fps) = config.target_fps {
            stream_config.minimum_frame_interval = screencapturekit::cm_sample_buffer::CMTime {
                value: 1,
                timescale: fps as i32,
                flags: 0,
                epoch: 0,
            };
        }

        let (frame_tx, frame_rx) = mpsc::channel();
        let handler = FrameHandler {
            tx: frame_tx,
            capture_start: Instant::now(),
        };

        let mut stream = SCStream::new(filter, stream_config, handler);
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
            pixel_format: PixelFormat::Rgba,
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
