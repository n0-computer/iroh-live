//! Ready-to-use test pattern video and tone audio sources.
//!
//! Implements [`VideoSource`] and [`AudioSource`] with animated SMPTE-style
//! color bars (bouncing ball, beep indicator) and a periodic 880 Hz tone.
//! Suitable for examples, demos, and integration tests that need a visual
//! or audible signal without capture hardware.

use std::time::{Duration, Instant};

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

const BALL_RADIUS: u32 = 15;
const BALL_BORDER: u32 = 3;

/// Animated SMPTE color-bar test pattern with a bouncing ball and beep indicator.
///
/// Renders:
/// - Top 70%: seven vertical SMPTE color bars
/// - 70–85%: horizontal grayscale ramp
/// - 85–100%: black
/// - A white ball bouncing left-right at the vertical center
/// - A yellow square pulsing in the bottom-right corner during the beep window
///
/// Does not sleep — the caller controls frame pacing. Timestamps are derived
/// from wall-clock time since [`start`](VideoSource::start).
#[derive(Debug)]
pub struct TestPatternSource {
    format: VideoFormat,
    frame_index: u64,
    started: bool,
    buffer: Vec<u8>,
    background: Vec<u8>,
    start_time: Instant,
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
            started: false,
            buffer: vec![0u8; size],
            background,
            start_time: Instant::now(),
        }
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

    fn stamp_ball(buf: &mut [u8], w: u32, h: u32, frame_index: u64) {
        let radius = BALL_RADIUS.min(w / 4).min(h / 4);
        if radius == 0 {
            return;
        }
        let outer = radius + BALL_BORDER;
        let range = w.saturating_sub(2 * outer).max(1);
        let period = 2 * range as u64;
        let pos_in_period = frame_index % period.max(1);
        let ball_x = if pos_in_period < range as u64 {
            outer + pos_in_period as u32
        } else {
            outer + (period - pos_in_period) as u32
        };
        let ball_y = h / 2;

        let outer_r2 = (outer * outer) as i64;
        let inner_r2 = (radius * radius) as i64;
        let y_min = ball_y.saturating_sub(outer);
        let y_max = (ball_y + outer).min(h);
        let x_min = ball_x.saturating_sub(outer);
        let x_max = (ball_x + outer).min(w);

        for y in y_min..y_max {
            let dy = y as i64 - ball_y as i64;
            for x in x_min..x_max {
                let dx = x as i64 - ball_x as i64;
                let d2 = dx * dx + dy * dy;
                if d2 <= outer_r2 {
                    let idx = ((y * w + x) * 4) as usize;
                    if d2 <= inner_r2 {
                        buf[idx] = 255;
                        buf[idx + 1] = 255;
                        buf[idx + 2] = 255;
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

    fn stamp_beep_indicator(buf: &mut [u8], w: u32, h: u32, beep_active: bool) {
        if !beep_active {
            return;
        }
        let size = 20u32.min(w / 8).min(h / 8);
        let x0 = w.saturating_sub(size + 10);
        let y0 = h.saturating_sub(size + 10);
        for y in y0..h.min(y0 + size) {
            for x in x0..w.min(x0 + size) {
                let idx = ((y * w + x) * 4) as usize;
                buf[idx] = 255;
                buf[idx + 1] = 255;
                buf[idx + 2] = 0;
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
        self.start_time = Instant::now();
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
        self.buffer.copy_from_slice(&self.background);
        Self::stamp_ball(&mut self.buffer, w, h, self.frame_index * 4);

        let elapsed = self.start_time.elapsed().as_secs_f64();
        let beep_active = (elapsed % 1.0) < 0.1;
        Self::stamp_beep_indicator(&mut self.buffer, w, h, beep_active);

        self.frame_index += 1;

        Ok(Some(VideoFrame::new_rgba(
            Bytes::copy_from_slice(&self.buffer),
            w,
            h,
            Duration::ZERO,
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
    fn cloned_boxed(&self) -> Box<dyn AudioSource> {
        Box::new(Self {
            format: self.format,
            phase: 0.0,
            sample_index: 0,
        })
    }

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
