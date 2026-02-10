use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use image::{Rgba, RgbaImage};
use n0_watcher::{Watchable, Watcher};
use rustfft::{FftPlanner, num_complex::Complex};
use std::f32::consts::PI;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{av::DecodedFrame, subscribe::WatchTrack, util::spawn_thread};

/// Max samples in the ring buffer (~100ms of stereo audio at 48kHz).
const MAX_SAMPLES: usize = 9600;

/// Shared ring buffer for recent decoded audio samples.
#[derive(Debug, Clone)]
pub(crate) struct SampleRing {
    inner: Arc<Mutex<VecDeque<f32>>>,
}

impl SampleRing {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_SAMPLES))),
        }
    }

    /// Append interleaved samples, dropping oldest if over capacity.
    pub(crate) fn push(&self, samples: &[f32]) {
        let mut buf = self.inner.lock().unwrap();
        buf.extend(samples);
        let overflow = buf.len().saturating_sub(MAX_SAMPLES);
        if overflow > 0 {
            buf.drain(..overflow);
        }
    }

    /// Clone current contents without draining.
    pub(crate) fn snapshot(&self) -> Vec<f32> {
        self.inner.lock().unwrap().iter().copied().collect()
    }
}

/// Audio visualization mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Visualization {
    /// Scrolling oscilloscope waveform showing recent audio amplitude.
    #[default]
    Waveform,
    /// Scrolling peak envelope with green-yellow-red level gradient.
    Peaks,
    /// FFT spectrum analyzer with log-frequency bars.
    Spectrum,
    /// Stereo waveform with L/R channels in top/bottom halves.
    WaveformStereo,
}

/// All visualization modes in cycle order.
const ALL_MODES: [Visualization; 4] = [
    Visualization::Waveform,
    Visualization::Peaks,
    Visualization::Spectrum,
    Visualization::WaveformStereo,
];

impl Visualization {
    /// Cycle to the next mode. Returns `None` after the last mode (to turn off).
    pub fn cycle(current: Option<Self>) -> Option<Self> {
        match current {
            None => Some(ALL_MODES[0]),
            Some(mode) => {
                let idx = ALL_MODES.iter().position(|&m| m == mode).unwrap_or(0);
                ALL_MODES.get(idx + 1).copied()
            }
        }
    }

    /// Short label for UI display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Waveform => "Wav",
            Self::Peaks => "Peak",
            Self::Spectrum => "FFT",
            Self::WaveformStereo => "L/R",
        }
    }
}

/// Configurable colors for audio visualization rendering.
#[derive(Debug, Clone, Copy)]
pub struct VisualizationStyle {
    /// Background fill color (RGBA).
    pub background: [u8; 4],
    /// Foreground waveform color (RGBA).
    pub foreground: [u8; 4],
    /// Guide line color (RGBA).
    pub guide: [u8; 4],
    /// Peak hold marker color (RGBA).
    pub peak_hold: [u8; 4],
}

impl Default for VisualizationStyle {
    fn default() -> Self {
        Self {
            background: [0x1a, 0x1a, 0x2e, 0xff],
            foreground: [0x00, 0xff, 0x88, 0xff],
            guide: [0x33, 0x33, 0x55, 0xff],
            peak_hold: [0xff, 0xff, 0xff, 0xff],
        }
    }
}

impl VisualizationStyle {
    fn bg(&self) -> Rgba<u8> {
        Rgba(self.background)
    }
    fn fg(&self) -> Rgba<u8> {
        Rgba(self.foreground)
    }
    fn guide(&self) -> Rgba<u8> {
        Rgba(self.guide)
    }
}

/// Spawn a visualizer thread that renders audio samples into RGBA frames at ~30fps.
///
/// Returns a [`WatchTrack`] that can be displayed with existing video view components.
pub(crate) fn spawn_visualizer(
    ring: SampleRing,
    channels: u32,
    mode: Visualization,
    shutdown: CancellationToken,
    style: VisualizationStyle,
) -> WatchTrack {
    let viewport = Watchable::new((320u32, 180u32));
    let (frame_tx, frame_rx) = mpsc::channel::<DecodedFrame>(2);
    let thread = spawn_thread("avis", {
        let mut viewport_w = viewport.watch();
        let shutdown = shutdown.clone();
        move || {
            let frame_duration = Duration::from_secs_f32(1.0 / 30.0);
            let start = Instant::now();
            for i in 1u32.. {
                if shutdown.is_cancelled() {
                    break;
                }
                viewport_w.update();
                let &(w, h) = viewport_w.peek();
                let (w, h) = (w.max(1), h.max(1));
                let samples = ring.snapshot();
                let img = match mode {
                    Visualization::Waveform => render_waveform(&samples, channels, w, h, &style),
                    Visualization::Peaks => render_peaks(&samples, channels, w, h, &style),
                    Visualization::Spectrum => render_spectrum(&samples, channels, w, h, &style),
                    Visualization::WaveformStereo => {
                        render_waveform_stereo(&samples, channels, w, h, &style)
                    }
                };
                let delay = image::Delay::from_saturating_duration(frame_duration);
                let decoded = DecodedFrame {
                    frame: image::Frame::from_parts(img, 0, 0, delay),
                    timestamp: start.elapsed(),
                };
                if frame_tx.blocking_send(decoded).is_err() {
                    break;
                }
                let expected = i * frame_duration;
                let actual = start.elapsed();
                if expected > actual {
                    thread::sleep(expected - actual);
                }
            }
        }
    });
    WatchTrack::from_thread(
        "audio-vis".to_string(),
        frame_rx,
        viewport,
        shutdown,
        thread,
    )
}

// -- Renderers --

fn render_waveform(
    samples: &[f32],
    channels: u32,
    width: u32,
    height: u32,
    style: &VisualizationStyle,
) -> RgbaImage {
    let w = width as usize;
    let h = height as usize;
    let mut img = RgbaImage::from_pixel(width, height, style.bg());

    // Center guide line.
    let center = (h / 2) as u32;
    for x in 0..width {
        img.put_pixel(x, center, style.guide());
    }

    if samples.is_empty() || w == 0 || h == 0 {
        return img;
    }

    // Mix to mono.
    let ch = channels.max(1) as usize;
    let mono: Vec<f32> = samples
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect();

    if mono.is_empty() {
        return img;
    }

    let fg = style.fg();

    // Compute per-column min/max, then smooth with neighbors for less choppy rendering.
    let mut mins = vec![0.0f32; w];
    let mut maxs = vec![0.0f32; w];
    for x in 0..w {
        let start = x * mono.len() / w;
        let end = ((x + 1) * mono.len() / w).max(start + 1).min(mono.len());
        for &s in &mono[start..end] {
            mins[x] = mins[x].min(s);
            maxs[x] = maxs[x].max(s);
        }
    }

    // Simple 3-tap smoothing pass on the envelope.
    let smooth_mins = smooth_envelope(&mins);
    let smooth_maxs = smooth_envelope(&maxs);

    for x in 0..w {
        let y_top = ((1.0 - smooth_maxs[x].clamp(-1.0, 1.0)) * 0.5 * (h - 1) as f32) as u32;
        let y_bot = ((1.0 - smooth_mins[x].clamp(-1.0, 1.0)) * 0.5 * (h - 1) as f32) as u32;
        let y_top = y_top.min(height - 1);
        let y_bot = y_bot.min(height - 1);

        for y in y_top..=y_bot {
            img.put_pixel(x as u32, y, fg);
        }
    }

    img
}

/// Scrolling peak envelope: each column shows the peak level of its time window,
/// drawn from bottom with green-yellow-red gradient. Like a DJ waveform overview.
fn render_peaks(
    samples: &[f32],
    channels: u32,
    width: u32,
    height: u32,
    style: &VisualizationStyle,
) -> RgbaImage {
    let w = width as usize;
    let h = height as usize;
    let mut img = RgbaImage::from_pixel(width, height, style.bg());

    if samples.is_empty() || w == 0 || h == 0 {
        return img;
    }

    // Guide lines at -6dB (50%) and -12dB (25%).
    let guide = style.guide();
    let y_6db = h - (h / 2);
    let y_12db = h - (h / 4);
    for x in 0..width {
        img.put_pixel(x, y_6db as u32, guide);
        img.put_pixel(x, y_12db as u32, guide);
    }

    // Mix to mono.
    let ch = channels.max(1) as usize;
    let mono: Vec<f32> = samples
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect();

    if mono.is_empty() {
        return img;
    }

    // For each pixel column, compute peak (abs max) and draw a filled bar from bottom.
    for x in 0..w {
        let start = x * mono.len() / w;
        let end = ((x + 1) * mono.len() / w).max(start + 1).min(mono.len());

        let peak = mono[start..end]
            .iter()
            .fold(0.0f32, |acc, &s| acc.max(s.abs()));
        let bar_h = (peak.clamp(0.0, 1.0) * h as f32) as usize;

        for y_off in 0..bar_h {
            let y = h - 1 - y_off;
            let t = y_off as f32 / h as f32;
            img.put_pixel(x as u32, y as u32, level_color(t));
        }
    }

    img
}

/// FFT spectrum analyzer renderer.
fn render_spectrum(
    samples: &[f32],
    channels: u32,
    width: u32,
    height: u32,
    style: &VisualizationStyle,
) -> RgbaImage {
    const FFT_SIZE: usize = 1024;

    let w = width as usize;
    let h = height as usize;
    let mut img = RgbaImage::from_pixel(width, height, style.bg());

    if w == 0 || h == 0 {
        return img;
    }

    // Mix to mono.
    let ch = channels.max(1) as usize;
    let mono: Vec<f32> = samples
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect();

    // Take last FFT_SIZE samples, zero-pad if fewer.
    let mut buf: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let offset = mono.len().saturating_sub(FFT_SIZE);
    let slice = &mono[offset..];
    for (i, &s) in slice.iter().enumerate() {
        let dest_i = FFT_SIZE - slice.len() + i;
        // Apply Hann window.
        let angle = 2.0 * PI * dest_i as f32 / (FFT_SIZE - 1) as f32;
        let window = 0.5 * (1.0 - angle.cos());
        buf[dest_i] = Complex::new(s * window, 0.0);
    }

    // Run FFT.
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    fft.process(&mut buf);

    // Magnitudes of first half (Nyquist).
    let half = FFT_SIZE / 2;
    let magnitudes: Vec<f32> = buf[..half]
        .iter()
        .map(|c| (c.re * c.re + c.im * c.im).sqrt())
        .collect();
    let max_mag = magnitudes.iter().copied().fold(0.0f32, f32::max).max(1e-6);

    // Log-frequency grouping into w columns.
    // Map column x to frequency bin range using logarithmic scale.
    for x in 0..w {
        let f_lo = (half as f32).powf(x as f32 / w as f32);
        let f_hi = (half as f32).powf((x + 1) as f32 / w as f32);
        let bin_lo = (f_lo as usize).max(1).min(half - 1);
        let bin_hi = (f_hi as usize).max(bin_lo + 1).min(half);

        let count = (bin_hi - bin_lo).max(1);
        let sum: f32 = magnitudes[bin_lo..bin_hi].iter().sum();
        let level = (sum / count as f32) / max_mag;

        // Draw vertical bar from bottom.
        let bar_h = (level * h as f32) as usize;
        for y_off in 0..bar_h {
            let y = h - 1 - y_off;
            let t = y_off as f32 / h as f32;
            img.put_pixel(x as u32, y as u32, level_color(t));
        }
    }

    img
}

/// Stereo waveform renderer: L channel in top half, R channel in bottom half.
fn render_waveform_stereo(
    samples: &[f32],
    channels: u32,
    width: u32,
    height: u32,
    style: &VisualizationStyle,
) -> RgbaImage {
    let ch = channels.max(1) as usize;

    // Fall back to mono waveform if not stereo.
    if ch < 2 {
        return render_waveform(samples, channels, width, height, style);
    }

    let w = width as usize;
    let h = height as usize;
    let mut img = RgbaImage::from_pixel(width, height, style.bg());

    if samples.is_empty() || w == 0 || h == 0 {
        return img;
    }

    // Split into L and R channels.
    let mut left = Vec::with_capacity(samples.len() / ch);
    let mut right = Vec::with_capacity(samples.len() / ch);
    for frame in samples.chunks(ch) {
        left.push(frame[0]);
        right.push(frame[1]);
    }

    let half_h = h / 2;
    let fg = style.fg();

    // Guide line at border between halves.
    let border_y = half_h as u32;
    for x in 0..width {
        img.put_pixel(x, border_y, style.guide());
    }

    // Center guides for each half.
    let top_center = (half_h / 2) as u32;
    let bot_center = (half_h + half_h / 2) as u32;
    for x in 0..width {
        img.put_pixel(x, top_center, style.guide());
        img.put_pixel(x, bot_center, style.guide());
    }

    // Render L channel in top half.
    render_waveform_half(&mut img, &left, w, 0, half_h, fg);
    // Render R channel in bottom half.
    render_waveform_half(&mut img, &right, w, half_h, half_h, fg);

    img
}

/// Render a mono waveform into a region of the image.
fn render_waveform_half(
    img: &mut RgbaImage,
    mono: &[f32],
    w: usize,
    y_offset: usize,
    region_h: usize,
    fg: Rgba<u8>,
) {
    if mono.is_empty() || w == 0 || region_h == 0 {
        return;
    }

    let mut mins = vec![0.0f32; w];
    let mut maxs = vec![0.0f32; w];
    for x in 0..w {
        let start = x * mono.len() / w;
        let end = ((x + 1) * mono.len() / w).max(start + 1).min(mono.len());
        for &s in &mono[start..end] {
            mins[x] = mins[x].min(s);
            maxs[x] = maxs[x].max(s);
        }
    }

    let smooth_mins = smooth_envelope(&mins);
    let smooth_maxs = smooth_envelope(&maxs);

    for x in 0..w {
        let y_top = ((1.0 - smooth_maxs[x].clamp(-1.0, 1.0)) * 0.5 * (region_h - 1) as f32)
            as usize
            + y_offset;
        let y_bot = ((1.0 - smooth_mins[x].clamp(-1.0, 1.0)) * 0.5 * (region_h - 1) as f32)
            as usize
            + y_offset;
        let y_top = y_top.min(y_offset + region_h - 1);
        let y_bot = y_bot.min(y_offset + region_h - 1);

        for y in y_top..=y_bot {
            img.put_pixel(x as u32, y as u32, fg);
        }
    }
}

/// 3-tap weighted average: 0.25 * prev + 0.5 * current + 0.25 * next.
fn smooth_envelope(values: &[f32]) -> Vec<f32> {
    let n = values.len();
    if n <= 2 {
        return values.to_vec();
    }
    let mut out = vec![0.0f32; n];
    out[0] = values[0] * 0.75 + values[1] * 0.25;
    for i in 1..n - 1 {
        out[i] = values[i - 1] * 0.25 + values[i] * 0.5 + values[i + 1] * 0.25;
    }
    out[n - 1] = values[n - 2] * 0.25 + values[n - 1] * 0.75;
    out
}

/// Green → Yellow → Red gradient based on level (0.0 .. 1.0).
fn level_color(t: f32) -> Rgba<u8> {
    if t < 0.6 {
        let r = (t / 0.6 * 255.0) as u8;
        Rgba([r, 0xff, 0x00, 0xff])
    } else {
        let g = ((1.0 - (t - 0.6) / 0.4) * 255.0) as u8;
        Rgba([0xff, g, 0x00, 0xff])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_style() -> VisualizationStyle {
        VisualizationStyle::default()
    }

    #[test]
    fn sample_ring_push_and_snapshot() {
        let ring = SampleRing::new();
        ring.push(&[1.0, 2.0, 3.0]);
        assert_eq!(ring.snapshot(), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn sample_ring_capacity() {
        let ring = SampleRing::new();
        let big = vec![0.5; MAX_SAMPLES + 100];
        ring.push(&big);
        let snap = ring.snapshot();
        assert_eq!(snap.len(), MAX_SAMPLES);
    }

    #[test]
    fn sample_ring_non_destructive_read() {
        let ring = SampleRing::new();
        ring.push(&[1.0, 2.0]);
        let s1 = ring.snapshot();
        let s2 = ring.snapshot();
        assert_eq!(s1, s2);
    }

    #[test]
    fn waveform_correct_dimensions() {
        let samples = vec![0.0; 480];
        let img = render_waveform(&samples, 1, 64, 32, &default_style());
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn waveform_has_visible_content() {
        let style = default_style();
        let samples: Vec<f32> = (0..480).map(|i| (i as f32 * 0.1).sin()).collect();
        let img = render_waveform(&samples, 1, 64, 32, &style);
        let bg = Rgba(style.background);
        let non_bg = img.pixels().filter(|&&p| p != bg).count();
        assert!(non_bg > 0);
    }

    #[test]
    fn peaks_correct_dimensions() {
        let samples: Vec<f32> = (0..960).map(|i| (i as f32 * 0.1).sin()).collect();
        let img = render_peaks(&samples, 2, 100, 50, &default_style());
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 50);
    }

    #[test]
    fn peaks_has_visible_content() {
        let style = default_style();
        let samples: Vec<f32> = (0..960).map(|i| (i as f32 * 0.1).sin()).collect();
        let img = render_peaks(&samples, 2, 100, 50, &style);
        let bg = Rgba(style.background);
        let non_bg = img.pixels().filter(|&&p| p != bg).count();
        assert!(non_bg > 0);
    }

    #[test]
    fn render_empty_samples() {
        let style = default_style();
        let img = render_waveform(&[], 1, 64, 32, &style);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
        let img = render_peaks(&[], 2, 64, 32, &style);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn spectrum_correct_dimensions() {
        let samples: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.05).sin()).collect();
        let img = render_spectrum(&samples, 1, 128, 64, &default_style());
        assert_eq!(img.width(), 128);
        assert_eq!(img.height(), 64);
    }

    #[test]
    fn spectrum_has_visible_content() {
        let style = default_style();
        let samples: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.1).sin()).collect();
        let img = render_spectrum(&samples, 1, 128, 64, &style);
        let bg = Rgba(style.background);
        let non_bg = img.pixels().filter(|&&p| p != bg).count();
        assert!(non_bg > 0);
    }

    #[test]
    fn spectrum_empty_samples() {
        let style = default_style();
        let img = render_spectrum(&[], 1, 64, 32, &style);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn waveform_stereo_correct_dimensions() {
        let samples: Vec<f32> = (0..960)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.3 })
            .collect();
        let img = render_waveform_stereo(&samples, 2, 100, 50, &default_style());
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 50);
    }

    #[test]
    fn waveform_stereo_has_visible_content() {
        let style = default_style();
        let samples: Vec<f32> = (0..960)
            .map(|i| (i as f32 * 0.1).sin() * if i % 2 == 0 { 1.0 } else { 0.7 })
            .collect();
        let img = render_waveform_stereo(&samples, 2, 100, 50, &style);
        let bg = Rgba(style.background);
        let non_bg = img.pixels().filter(|&&p| p != bg).count();
        assert!(non_bg > 0);
    }

    #[test]
    fn waveform_stereo_falls_back_to_mono() {
        let style = default_style();
        let samples: Vec<f32> = (0..480).map(|i| (i as f32 * 0.1).sin()).collect();
        let img = render_waveform_stereo(&samples, 1, 64, 32, &style);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
        let bg = Rgba(style.background);
        let non_bg = img.pixels().filter(|&&p| p != bg).count();
        assert!(non_bg > 0);
    }

    #[test]
    fn waveform_stereo_empty_samples() {
        let style = default_style();
        let img = render_waveform_stereo(&[], 2, 64, 32, &style);
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }

    #[test]
    fn visualization_cycle_order() {
        let mut mode = None;
        mode = Visualization::cycle(mode);
        assert_eq!(mode, Some(Visualization::Waveform));
        mode = Visualization::cycle(mode);
        assert_eq!(mode, Some(Visualization::Peaks));
        mode = Visualization::cycle(mode);
        assert_eq!(mode, Some(Visualization::Spectrum));
        mode = Visualization::cycle(mode);
        assert_eq!(mode, Some(Visualization::WaveformStereo));
        mode = Visualization::cycle(mode);
        assert_eq!(mode, None);
    }
}
