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
use std::time::Duration;

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

// CoreGraphics permission check (macOS 10.15+).
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

/// Lists available macOS displays.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    check_screen_capture_permission();
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get shareable content: {e:?}"))?;
    let displays = content.displays;
    let mut result = Vec::new();
    for (i, display) in displays.iter().enumerate() {
        result.push(MonitorInfo {
            id: format!("macos-display-{}", display.display_id),
            name: format!("Display {}", display.display_id),
            position: [display.frame.origin.x as i32, display.frame.origin.y as i32],
            dimensions: [display.width as u32, display.height as u32],
            scale_factor: 1.0, // SCK handles scaling internally
            refresh_rate_hz: None,
            is_primary: i == 0,
        });
    }
    Ok(result)
}

struct FrameHandler {
    tx: mpsc::Sender<VideoFrame>,
    width: u32,
    height: u32,
}

impl SCStreamOutputTrait for FrameHandler {
    fn did_output_sample_buffer(
        &self,
        sample_buffer: screencapturekit::cm_sample_buffer::CMSampleBuffer,
        _of_type: SCStreamOutputType,
    ) {
        // Extract pixel data from the sample buffer.
        // Currently copies to CPU RGBA. Future: wrap CVPixelBuffer as GpuFrame.
        let Some(pixel_buffer) = sample_buffer.pixel_buffer else {
            return;
        };
        let bytes_per_row = pixel_buffer.bytes_per_row;
        let height = pixel_buffer.height;
        let width = pixel_buffer.width;
        let data = &pixel_buffer.data;

        // ScreenCaptureKit delivers BGRA. Convert to RGBA.
        let pixel_count = (width * height) as usize;
        let mut rgba = vec![0u8; pixel_count * 4];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let src_offset = y * bytes_per_row as usize + x * 4;
                let dst_offset = (y * width as usize + x) * 4;
                if src_offset + 3 < data.len() && dst_offset + 3 < rgba.len() {
                    rgba[dst_offset] = data[src_offset + 2]; // R ← B
                    rgba[dst_offset + 1] = data[src_offset + 1]; // G
                    rgba[dst_offset + 2] = data[src_offset]; // B ← R
                    rgba[dst_offset + 3] = data[src_offset + 3]; // A
                }
            }
        }

        let frame = VideoFrame::new_rgba(rgba.into(), width as u32, height as u32);
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
    _stream: SCStream,
}

impl MacScreenCapturer {
    /// Creates a screen capturer for the primary display.
    pub fn new(config: &ScreenConfig) -> Result<Self> {
        check_screen_capture_permission();
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

        let display = content
            .displays
            .into_iter()
            .next()
            .context("no displays available")?;

        let width = display.width as u32;
        let height = display.height as u32;

        let filter = SCContentFilter::new(
            screencapturekit::stream::content_filter::InitParams::Display(display),
        );

        let mut stream_config = SCStreamConfiguration::default();
        stream_config.width = width;
        stream_config.height = height;
        stream_config.shows_cursor = config.show_cursor;
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
            width,
            height,
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
            _stream: stream,
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
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let mut latest = None;
        while let Ok(frame) = self.rx.try_recv() {
            latest = Some(frame);
        }
        Ok(latest)
    }
}
