//! Ready-to-use test pattern video and tone audio sources.
//!
//! Implements [`VideoSource`] and [`AudioSource`] with animated SMPTE-style
//! color bars (bouncing ball, beep indicator) and a periodic 880 Hz tone.
//! Suitable for examples, demos, and integration tests that need a visual
//! or audible signal without capture hardware.

use std::time::Duration;

use bytes::Bytes;

use crate::{
    format::{AudioFormat, PixelFormat, VideoFormat, VideoFrame},
    traits::{AudioSource, VideoSource},
};

// ── SMPTE test pattern video source ──────────────────────────────

const SMPTE_BARS: [[u8; 3]; 7] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
];

/// Width of the vertical scanning line (pixels).
const LINE_WIDTH: u32 = 10;
/// Border on each side of the scanning line (pixels).
const LINE_BORDER: u32 = 3;

/// Animated SMPTE color-bar test pattern with a scanning line and beep indicator.
///
/// Renders:
/// - Top 70%: seven vertical SMPTE color bars
/// - 70–85%: horizontal grayscale ramp
/// - 85–100%: black
/// - A white vertical line (top to bottom) bouncing left-right
/// - A large yellow square pulsing in the center during the beep window
///
/// Does not sleep — the caller controls frame pacing. Timestamps are
/// frame-index based (`index / fps`) for deterministic tests by default.
#[derive(Debug)]
pub struct TestPatternSource {
    format: VideoFormat,
    frame_index: u64,
    fps: f64,
    started: bool,
    buffer: Vec<u8>,
    background: Vec<u8>,
}

impl TestPatternSource {
    /// Creates a test pattern source with the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width * height * 4) as usize;
        let background = Self::render_background(width, height);
        Self {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [width, height],
            },
            frame_index: 0,
            fps: 30.0,
            started: false,
            buffer: vec![0u8; size],
            background,
        }
    }

    /// Sets the frame rate used for timestamp generation.
    pub fn with_fps(mut self, fps: f64) -> Self {
        self.fps = fps;
        self
    }

    fn render_background(w: u32, h: u32) -> Vec<u8> {
        let mut data = vec![0u8; (w * h * 4) as usize];
        let bar_end = h * 70 / 100;
        let ramp_end = h * 85 / 100;

        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let (r, g, b) = if y < bar_end {
                    let bar_idx = (x * 7 / w) as usize;
                    let bar_idx = bar_idx.min(6);
                    let c = SMPTE_BARS[bar_idx];
                    (c[0], c[1], c[2])
                } else if y < ramp_end {
                    let v = (x * 255 / w.max(1)) as u8;
                    (v, v, v)
                } else {
                    (0, 0, 0)
                };
                data[idx] = r;
                data[idx + 1] = g;
                data[idx + 2] = b;
                data[idx + 3] = 255;
            }
        }
        data
    }

    /// Renders a vertical scanning line from top to bottom, bouncing left-right.
    ///
    /// Position is derived from `time_secs` so speed is constant regardless
    /// of frame rate (~120 px/s). The line has a black border for visibility
    /// against both light and dark backgrounds.
    fn stamp_line(buf: &mut [u8], w: u32, h: u32, time_secs: f64) {
        let half = LINE_WIDTH / 2 + LINE_BORDER;
        let range = w.saturating_sub(2 * half).max(1);
        // ~120 pixels per second, bouncing back and forth.
        let pixels_per_sec = 120.0_f64;
        let period = 2.0 * range as f64 / pixels_per_sec;
        let t_in_period = time_secs % period.max(0.001);
        let frac = t_in_period / period;
        let pos_frac = if frac < 0.5 {
            frac * 2.0
        } else {
            2.0 - frac * 2.0
        };
        let center_x = half + (pos_frac * range as f64) as u32;

        let x_min = center_x.saturating_sub(half);
        let x_max = (center_x + half).min(w);
        let inner_min = center_x.saturating_sub(LINE_WIDTH / 2);
        let inner_max = (center_x + LINE_WIDTH / 2).min(w);

        for y in 0..h {
            for x in x_min..x_max {
                let idx = ((y * w + x) * 4) as usize;
                if x >= inner_min && x < inner_max {
                    // White core
                    buf[idx] = 255;
                    buf[idx + 1] = 255;
                    buf[idx + 2] = 255;
                } else {
                    // Black border
                    buf[idx] = 0;
                    buf[idx + 1] = 0;
                    buf[idx + 2] = 0;
                }
                buf[idx + 3] = 255;
            }
        }
    }

    /// Renders a centered yellow square with black border during the beep window.
    ///
    /// The square side is 1/3 of the shorter dimension. Large enough to
    /// survive codec compression, used by E2E tests to verify live video.
    fn stamp_beep_indicator(buf: &mut [u8], w: u32, h: u32, beep_active: bool) {
        if !beep_active {
            return;
        }
        let border = 3u32;
        let side = w.min(h) / 3;
        let outer = side + 2 * border;
        let ox = (w.saturating_sub(outer)) / 2;
        let oy = (h.saturating_sub(outer)) / 2;
        for y in oy..(oy + outer).min(h) {
            for x in ox..(ox + outer).min(w) {
                let idx = ((y * w + x) * 4) as usize;
                let in_inner = x >= ox + border
                    && x < ox + border + side
                    && y >= oy + border
                    && y < oy + border + side;
                if in_inner {
                    buf[idx] = 255;
                    buf[idx + 1] = 255;
                    buf[idx + 2] = 0;
                } else {
                    buf[idx] = 0;
                    buf[idx + 1] = 0;
                    buf[idx + 2] = 0;
                }
                buf[idx + 3] = 255;
            }
        }
    }
}

impl VideoSource for TestPatternSource {
    fn name(&self) -> &str {
        "test-pattern"
    }
    fn format(&self) -> VideoFormat {
        self.format.clone()
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.started = true;
        self.frame_index = 0;
        Ok(())
    }
    fn stop(&mut self) -> anyhow::Result<()> {
        self.started = false;
        Ok(())
    }
    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        if !self.started {
            return Ok(None);
        }
        let [w, h] = self.format.dimensions;
        let t = self.frame_index as f64 / self.fps;
        self.buffer.copy_from_slice(&self.background);
        Self::stamp_line(&mut self.buffer, w, h, t);
        let beep_active = (t % 1.0) < 0.1;
        Self::stamp_beep_indicator(&mut self.buffer, w, h, beep_active);

        let timestamp = Duration::from_secs_f64(t);
        self.frame_index += 1;

        Ok(Some(VideoFrame::new_rgba(
            Bytes::copy_from_slice(&self.buffer),
            w,
            h,
            timestamp,
        )))
    }
}

// ── Test tone audio source ───────────────────────────────────────

/// Periodic 880 Hz tone: beeps for 100 ms every second, silent otherwise.
///
/// Produces mono 48 kHz audio. The beep-silence pattern is synchronized
/// with [`TestPatternSource`]'s yellow indicator square.
#[derive(Debug)]
pub struct TestToneSource {
    format: AudioFormat,
    phase: f64,
    sample_index: u64,
}

impl TestToneSource {
    /// Creates a new test tone source (mono 48 kHz, 880 Hz beep).
    pub fn new() -> Self {
        Self {
            format: AudioFormat::mono_48k(),
            phase: 0.0,
            sample_index: 0,
        }
    }
}

impl Default for TestToneSource {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSource for TestToneSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> anyhow::Result<Option<usize>> {
        let sample_rate = self.format.sample_rate as f64;
        let channels = self.format.channel_count as usize;
        let frames = buf.len() / channels;

        for i in 0..frames {
            let t = self.sample_index as f64 / sample_rate;
            let in_beep = (t % 1.0) < 0.1;
            let sample = if in_beep {
                (self.phase * std::f64::consts::TAU).sin() as f32 * 0.3
            } else {
                0.0
            };
            if in_beep {
                self.phase += 880.0 / sample_rate;
            } else {
                self.phase = 0.0;
            }
            for ch in 0..channels {
                buf[i * channels + ch] = sample;
            }
            self.sample_index += 1;
        }
        Ok(Some(frames))
    }
}
