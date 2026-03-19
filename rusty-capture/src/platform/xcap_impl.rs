//! Cross-platform screen capture via the [`xcap`] crate.
//!
//! Works on Linux (X11 and Wayland via XDG Desktop Portal), macOS, and
//! Windows. Produces CPU-backed RGBA frames with software-driven frame
//! rate limiting — no zero-copy path.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusty_codecs::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};
use tracing::debug;
use xcap::Monitor;

use crate::types::{CaptureBackend, MonitorInfo, ScreenConfig};

/// Enumerates monitors visible to xcap.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    let xcap_monitors = Monitor::all().context("xcap: failed to enumerate monitors")?;
    let mut result = Vec::with_capacity(xcap_monitors.len());
    for (i, m) in xcap_monitors.iter().enumerate() {
        let name = m.name().unwrap_or_else(|_| format!("Monitor {i}"));
        let width = m.width().unwrap_or(0);
        let height = m.height().unwrap_or(0);
        let is_primary = m.is_primary().unwrap_or(false);
        let x = m.x().unwrap_or(0);
        let y = m.y().unwrap_or(0);
        let scale_factor = m.scale_factor().unwrap_or(1.0);
        let refresh_rate_hz = m.frequency().ok().filter(|&f| f > 0.0);
        result.push(MonitorInfo {
            backend: CaptureBackend::Xcap,
            id: format!("xcap-{i}"),
            name,
            position: [x, y],
            dimensions: [width, height],
            scale_factor,
            refresh_rate_hz,
            is_primary,
        });
    }
    Ok(result)
}

/// Screen capturer backed by [`xcap`].
///
/// Captures screenshots at a target frame rate using
/// [`Monitor::capture_image`] and converts them to RGBA [`VideoFrame`]
/// values. Frame rate is enforced with a simple sleep-based limiter.
pub struct XcapScreenCapturer {
    monitor: Monitor,
    monitor_name: String,
    target_interval: Duration,
    dimensions: [u32; 2],
    started: bool,
    start_time: Instant,
    last_capture: Instant,
}

impl std::fmt::Debug for XcapScreenCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XcapScreenCapturer")
            .field("monitor_name", &self.monitor_name)
            .field("dimensions", &self.dimensions)
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

impl XcapScreenCapturer {
    /// Creates a new xcap screen capturer for the given monitor.
    pub fn new(monitor_info: &MonitorInfo, config: &ScreenConfig) -> Result<Self> {
        let all = Monitor::all().context("xcap: failed to enumerate monitors")?;
        // Match by index embedded in the id ("xcap-N").
        let idx: usize = monitor_info
            .id
            .strip_prefix("xcap-")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let monitor = all
            .into_iter()
            .nth(idx)
            .context("xcap: monitor index out of range")?;

        let width = monitor.width().unwrap_or(monitor_info.dimensions[0]);
        let height = monitor.height().unwrap_or(monitor_info.dimensions[1]);
        let fps = config.target_fps.unwrap_or(30.0);
        let target_interval = Duration::from_secs_f64(1.0 / f64::from(fps));

        let name = monitor.name().unwrap_or_else(|_| monitor_info.name.clone());

        debug!(
            name = %name,
            width,
            height,
            fps,
            "xcap screen capturer created"
        );

        let now = Instant::now();
        Ok(Self {
            monitor,
            monitor_name: name,
            target_interval,
            dimensions: [width, height],
            started: false,
            start_time: now,
            last_capture: now,
        })
    }
}

impl VideoSource for XcapScreenCapturer {
    fn name(&self) -> &str {
        &self.monitor_name
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: self.dimensions,
        }
    }

    fn start(&mut self) -> Result<()> {
        self.started = true;
        self.start_time = Instant::now();
        self.last_capture = self.start_time;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.started = false;
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.started {
            return Ok(None);
        }

        // Frame rate limiting: return None if the interval hasn't elapsed yet.
        // The caller is responsible for polling at the appropriate rate.
        if self.last_capture.elapsed() < self.target_interval {
            return Ok(None);
        }

        let img = self
            .monitor
            .capture_image()
            .context("xcap: capture_image failed")?;

        let now = Instant::now();
        self.last_capture = now;
        let timestamp = now.duration_since(self.start_time);

        let (w, h) = (img.width(), img.height());

        // Update dimensions if the monitor resolution changed.
        if w != self.dimensions[0] || h != self.dimensions[1] {
            debug!(
                old_w = self.dimensions[0],
                old_h = self.dimensions[1],
                new_w = w,
                new_h = h,
                "xcap: monitor resolution changed"
            );
            self.dimensions = [w, h];
        }

        let data: bytes::Bytes = img.into_raw().into();
        Ok(Some(VideoFrame::new_rgba(data, w, h, timestamp)))
    }
}
