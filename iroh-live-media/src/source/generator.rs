//! Generated sources: a test picture and a tone for diagnosing playback.
//!
//! Each element of the pattern makes one fault visible:
//!
//! - A white bar sweeps left to right once every [`SWEEP`]. Judder and dropped
//!   frames show on its moving edge. The edge spans most of the frame height,
//!   so tearing breaks it into offset segments.
//! - A ruler above the sweep marks tenths of the width, one tick per 200 ms of
//!   the bar's travel, so the rate can be read off.
//! - Stripes at four pitches sit below the sweep. The three grey columns lose
//!   their stripes when the picture is scaled or the encoder runs out of bits.
//!   The magenta and green column has flat luma, so it turns solid olive only
//!   when chroma detail is lost.
//! - A frame counter makes dropped frames countable. A clock makes latency
//!   measurable: photograph the publisher's and the player's screens together
//!   and subtract the two stamps.
//! - A marker lights for [`BEEP_LENGTH`] every [`BEEP_PERIOD`], at the same
//!   media time as the beep, so a viewer sees and hears whether they agree.
//!
//! Both generators take their phase from one process-wide clock, so a picture
//! and a tone started at different moments still agree. Each paces against an
//! absolute schedule, because a sleep always overshoots and relative waits
//! would fall behind the clock.

use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use moq_mux::Clock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{AudioFormat, sender::PcmFanout};
use crate::{
    audio,
    frames::FrameSlot,
    video::{self, Size, Surface},
};

/// How long the bar takes to sweep the frame once.
///
/// Two seconds is slow enough to follow by eye, and a stall of a few frames
/// still shows as a hesitation.
const SWEEP: Duration = Duration::from_secs(2);

/// How often the marker flashes and the tone beeps.
const BEEP_PERIOD: Duration = Duration::from_secs(1);

/// How long each flash and beep lasts.
///
/// Three frames at 30 fps, so no single dropped frame hides the event.
const BEEP_LENGTH: Duration = Duration::from_millis(100);

/// The beep frequency, in hertz.
///
/// An octave above concert A carries through a laptop speaker and a phone
/// microphone.
pub(crate) const BEEP_HZ: f64 = 880.0;

/// When the marker is lit and the beep sounds, in media time.
pub(crate) const BEEP: Gate = Gate::Pulse {
    period: BEEP_PERIOD,
    length: BEEP_LENGTH,
};

/// The sample rate of every generated tone.
///
/// It is the Opus rate, so the encoder does not resample.
pub(crate) const TONE_RATE: u32 = 48_000;

/// The duration of one tone buffer.
///
/// It matches the Opus frame, so the encoder takes each buffer whole.
const TONE_FRAME: Duration = Duration::from_millis(20);

/// The peak amplitude of the tone.
///
/// Opus overshoots a little on decode, so a tone at full scale clips against
/// the mixer's clamp.
const AMPLITUDE: f32 = 0.5;

/// How long a pulse takes to reach full amplitude, and to fall from it.
///
/// A hard edge on a sine clicks. Two milliseconds is a fifteenth of a frame at
/// 30 fps, too short to shift when the beep appears to start.
const RAMP: Duration = Duration::from_millis(2);

/// The timeline both generators draw their phase from.
///
/// One per process, so the flash and the beep agree whenever each was started.
fn test_clock() -> Clock {
    static CLOCK: OnceLock<Clock> = OnceLock::new();
    *CLOCK.get_or_init(Clock::new)
}

/// When a generated tone sounds.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Gate {
    /// It never stops.
    Continuous,
    /// It sounds for `length` at the start of every `period` of media time.
    Pulse {
        /// How often a pulse begins.
        period: Duration,
        /// How long one lasts.
        length: Duration,
    },
}

impl Gate {
    /// Returns whether the tone sounds at `media`.
    fn open(self, media: Duration) -> bool {
        match self {
            Self::Continuous => true,
            Self::Pulse { period, length } => {
                media.as_micros() % period.as_micros() < length.as_micros()
            }
        }
    }

    /// Returns the amplitude at `media`, tapered over [`RAMP`] at each edge.
    fn envelope(self, media: Duration) -> f32 {
        let Self::Pulse { period, length } = self else {
            return 1.0;
        };
        let phase = media.as_micros() % period.as_micros();
        let length = length.as_micros();
        if phase >= length {
            return 0.0;
        }
        let edge = phase.min(length - phase) as f32;
        (edge / RAMP.as_micros() as f32).min(1.0)
    }
}

/// Sleeps until `due`, or returns `false` once `stop` is cancelled.
///
/// Wakes at least every 100 ms to check `stop`, so a dropped source does not
/// hold its thread for a whole slow frame interval.
fn sleep_until(due: Instant, stop: &CancellationToken) -> bool {
    loop {
        if stop.is_cancelled() {
            return false;
        }
        let now = Instant::now();
        let Some(left) = due
            .checked_duration_since(now)
            .filter(|left| !left.is_zero())
        else {
            return true;
        };
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

/// Paints the pattern into `slot` until `stop` is cancelled.
pub(crate) fn run_pattern(size: Size, rate: video::Rate, slot: FrameSlot, stop: CancellationToken) {
    let clock = test_clock();
    let interval = Duration::from_secs_f64(1.0 / rate.as_f64());
    let mut canvas = Canvas::new(size);
    let started = Instant::now();
    for count in 0u64.. {
        let due = started + interval.mul_f64(count as f64);
        if !sleep_until(due, &stop) {
            break;
        }
        // Read the clock after the wait, so the frame shows its own time.
        let timestamp = clock.now();
        let media = Duration::from(timestamp);
        let rgba = canvas.paint(count, media, SystemTime::now());
        match Surface::rgba(rgba, size) {
            Ok(surface) => slot.send(Arc::new(video::Frame::new(surface, timestamp))),
            Err(err) => {
                warn!(error = %err, "the test pattern could not build a picture");
                break;
            }
        }
    }
    debug!("test pattern stopped");
}

/// Generates a gated sine at `hz` into `fanout` until `stop` is cancelled.
pub(crate) fn run_tone(
    hz: f64,
    format: AudioFormat,
    gate: Gate,
    fanout: PcmFanout,
    stop: CancellationToken,
) {
    let clock = test_clock();
    let sample_rate = format.sample_rate.max(1);
    let channels = format.layout.channels();
    let per_frame = (f64::from(sample_rate) * TONE_FRAME.as_secs_f64()) as usize;
    let sample_ns = 1_000_000_000 / u64::from(sample_rate);
    let step = hz * std::f64::consts::TAU / f64::from(sample_rate);
    // Start on the shared test clock, so the beep lines up with the flash.
    let origin = Duration::from(clock.now());
    let started = Instant::now();
    let mut sample = 0usize;

    for index in 0u64.. {
        if !sleep_until(started + TONE_FRAME * index as u32, &stop) {
            break;
        }
        let start = origin + TONE_FRAME * index as u32;
        let mut data = Vec::with_capacity(per_frame * channels as usize * 4);
        for offset in 0..per_frame {
            // Each sample gets its own media time, so a pulse edge can fall
            // inside a buffer.
            let media = start + Duration::from_nanos(offset as u64 * sample_ns);
            let value =
                ((sample + offset) as f64 * step).sin() as f32 * AMPLITUDE * gate.envelope(media);
            for _ in 0..channels {
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
        sample += per_frame;
        let Ok(timestamp) = moq_net::Timestamp::from_micros(start.as_micros() as u64) else {
            break;
        };
        // An error only means no broadcast is attached.
        let _ = fanout.send(audio::Frame::new(data.into(), timestamp));
    }
    debug!("test tone stopped");
}

/// The rows one element of the pattern occupies.
#[derive(Debug, Clone, Copy)]
struct Band {
    /// First row, inclusive.
    top: u32,
    /// Last row, exclusive.
    bottom: u32,
}

impl Band {
    fn height(self) -> u32 {
        self.bottom.saturating_sub(self.top)
    }

    fn rows(self) -> std::ops::Range<u32> {
        self.top..self.bottom
    }
}

/// Where each element lands, for one frame size.
///
/// Bands are cut in sixteenths of the height, so the pattern reads the same at
/// every frame size.
#[derive(Debug, Clone, Copy)]
struct Layout {
    size: Size,
    /// The frame counter.
    counter: Band,
    /// The time of day.
    clock: Band,
    /// Where the sweeping bar runs, above the stripes.
    sweep: Band,
    /// The four stripe columns.
    stripes: Band,
    /// The marker that flashes with the beep.
    marker: Band,
}

impl Layout {
    fn new(size: Size) -> Self {
        let band = |from: u32, to: u32| Band {
            top: size.height * from / 16,
            bottom: size.height * to / 16,
        };
        Self {
            size,
            counter: band(0, 3),
            clock: band(3, 6),
            sweep: band(6, 10),
            stripes: band(10, 13),
            marker: band(13, 16),
        }
    }

    /// Returns the rows the sweeping bar spans.
    ///
    /// The bar runs from the top of its band to the bottom of the frame, across
    /// the stripes and the marker. Tearing shows only on a long edge.
    fn bar(self) -> Band {
        Band {
            top: self.sweep.top,
            bottom: self.size.height,
        }
    }

    /// Returns the width of the sweeping bar.
    ///
    /// Wide enough to survive a low bitrate, narrow enough to place against a
    /// ruler tick.
    fn bar_width(self) -> u32 {
        (self.size.width / 64).max(4)
    }
}

/// Ink for the digits and the sweeping bar.
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

/// The background.
const BLACK: [u8; 3] = [0x00, 0x00, 0x00];

/// The lit marker.
///
/// Nothing else in the pattern is yellow, so a flash stands out in a photo of
/// two screens.
const YELLOW: [u8; 3] = [0xff, 0xff, 0x00];

/// The ruler above the sweep.
const GREY: [u8; 3] = [0x60, 0x60, 0x60];

/// The marker between flashes.
///
/// Not black, so the marker band can be found in a still frame.
const DIM: [u8; 3] = [0x28, 0x28, 0x28];

/// The four stripe columns: pitch in pixels, then the two colours.
///
/// The greys go from one pixel to four, so the column where the stripes blur
/// shows how much detail is lost. Magenta and this green have the same BT.601
/// luma (about 105), so only chroma carries the last column's stripes. They
/// survive 4:2:0 subsampling, but not scaling or a poor chroma resampler.
const STRIPES: [(u32, [u8; 3], [u8; 3]); 4] = [
    (1, WHITE, BLACK),
    (2, WHITE, BLACK),
    (4, WHITE, BLACK),
    (4, [0xff, 0x00, 0xff], [0x00, 0xb3, 0x00]),
];

/// The buffers one publication paints from.
///
/// The static parts are painted once and copied into each frame. That keeps a
/// 720p pattern well inside its frame interval.
#[derive(Debug)]
struct Canvas {
    layout: Layout,
    /// Everything that does not change from frame to frame.
    background: Vec<u8>,
    /// The frame being painted.
    rgba: Vec<u8>,
}

impl Canvas {
    /// Creates a canvas for `size` with the static parts drawn.
    fn new(size: Size) -> Self {
        let layout = Layout::new(size);
        let mut background = vec![0u8; size.pixels() as usize * 4];
        fill(&mut background, size, layout.counter, BLACK);
        fill(&mut background, size, layout.clock, BLACK);
        fill(&mut background, size, layout.sweep, BLACK);
        stripes(&mut background, size, layout.stripes);
        fill(&mut background, size, layout.marker, DIM);
        ruler(&mut background, size, layout.sweep);
        Self {
            layout,
            rgba: background.clone(),
            background,
        }
    }

    /// Paints one frame and returns its pixels.
    ///
    /// `media` is the frame's presentation time and drives the sweep and the
    /// marker. Both move at a constant rate on the player's timeline, so a stall
    /// shows as a jump. `count` and `wall` are only drawn as digits.
    fn paint(&mut self, count: u64, media: Duration, wall: SystemTime) -> &[u8] {
        let layout = self.layout;
        let size = layout.size;
        self.rgba.copy_from_slice(&self.background);

        if BEEP.open(media) {
            fill(&mut self.rgba, size, layout.marker, YELLOW);
        }

        let width = layout.bar_width();
        let left = sweep_x(size.width, width, media);
        for y in layout.bar().rows() {
            for x in left..(left + width).min(size.width) {
                put(&mut self.rgba, size, x, y, WHITE);
            }
        }

        draw_line(
            &mut self.rgba,
            size,
            layout.counter,
            &counter_text(count),
            WHITE,
        );
        draw_line(&mut self.rgba, size, layout.clock, &clock_text(wall), WHITE);
        &self.rgba
    }
}

/// Returns the frame counter line, six digits that wrap after nine hours at 30 fps.
fn counter_text(count: u64) -> String {
    format!("F {:06}", count % 1_000_000)
}

/// Returns the clock line: the time of day in UTC, to the millisecond.
///
/// UTC, because the publisher and the player may not share a timezone.
fn clock_text(wall: SystemTime) -> String {
    let since = wall
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = since.as_secs() % 86_400;
    format!(
        "T {:02}:{:02}:{:02}.{:03}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60,
        since.subsec_millis(),
    )
}

/// Returns the left edge of the sweeping bar at `media`.
fn sweep_x(width: u32, bar: u32, media: Duration) -> u32 {
    let period = SWEEP.as_micros();
    let phase = media.as_micros() % period;
    let travel = u128::from(width.saturating_sub(bar));
    // The division cannot exceed `travel`, which came from a u32.
    (travel * phase / period) as u32
}

/// Fills `band` with one colour.
fn fill(rgba: &mut [u8], size: Size, band: Band, colour: [u8; 3]) {
    let Some(first) = band.rows().next() else {
        return;
    };
    for x in 0..size.width {
        put(rgba, size, x, first, colour);
    }
    replicate(rgba, size, band);
}

/// Draws the four stripe columns across `band`.
fn stripes(rgba: &mut [u8], size: Size, band: Band) {
    let Some(first) = band.rows().next() else {
        return;
    };
    for x in 0..size.width {
        let column = (x * STRIPES.len() as u32 / size.width).min(STRIPES.len() as u32 - 1);
        let (pitch, first_colour, second) = STRIPES[column as usize];
        let colour = match (x / pitch) % 2 {
            0 => first_colour,
            _ => second,
        };
        put(rgba, size, x, first, colour);
    }
    replicate(rgba, size, band);
}

/// Copies the first row of `band` over the rest of it.
///
/// Every band is the same on every row, so only one row is painted pixel by
/// pixel.
fn replicate(rgba: &mut [u8], size: Size, band: Band) {
    let stride = size.width as usize * 4;
    let Some(first) = band.rows().next() else {
        return;
    };
    let (head, rest) = rgba.split_at_mut((first as usize + 1) * stride);
    let row = &head[first as usize * stride..];
    for target in rest
        .chunks_exact_mut(stride)
        .take(band.height() as usize - 1)
    {
        target.copy_from_slice(row);
    }
}

/// Draws a tick at every tenth of the width along the top of `band`.
fn ruler(rgba: &mut [u8], size: Size, band: Band) {
    let height = (band.height() / 6).max(2);
    let width = (size.width / 200).max(1);
    for tick in 0..=10 {
        let centre = size.width * tick / 10;
        let from = centre.saturating_sub(width);
        for y in band.top..(band.top + height).min(band.bottom) {
            for x in from..(from + 2 * width).min(size.width) {
                put(rgba, size, x, y, GREY);
            }
        }
    }
}

/// Rows in one glyph of the built-in font.
const GLYPH_ROWS: usize = 7;

/// Columns in one glyph. A blank column separates two of them.
const GLYPH_COLS: u32 = 5;

/// Draws `text` centred in `band`, as large as the band and the frame allow.
///
/// Large digits stay readable in a photo of a small screen.
fn draw_line(rgba: &mut [u8], size: Size, band: Band, text: &str, ink: [u8; 3]) {
    let columns = text.chars().count() as u32 * (GLYPH_COLS + 1);
    // Leave a sixteenth of the width and two rows of the band as margin, so no
    // glyph touches an edge the encoder blurs.
    let from_width = (size.width * 15 / 16) / columns.max(1);
    let from_height = band.height().saturating_sub(2) / GLYPH_ROWS as u32;
    let scale = from_width.min(from_height).max(1);

    let mut x = (size.width.saturating_sub(columns * scale)) / 2;
    let y = band.top + (band.height().saturating_sub(GLYPH_ROWS as u32 * scale)) / 2;
    for ch in text.chars() {
        draw_glyph(rgba, size, glyph(ch), x, y, scale, ink);
        x += (GLYPH_COLS + 1) * scale;
    }
}

/// Draws one glyph at (`x`, `y`), each font pixel `scale` pixels square.
fn draw_glyph(
    rgba: &mut [u8],
    size: Size,
    glyph: [u8; GLYPH_ROWS],
    x: u32,
    y: u32,
    scale: u32,
    ink: [u8; 3],
) {
    for (row, bits) in glyph.into_iter().enumerate() {
        for column in 0..GLYPH_COLS {
            // The leftmost column is the high bit of the five.
            if bits & (1 << (GLYPH_COLS - 1 - column)) == 0 {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = x + column * scale + dx;
                    let py = y + row as u32 * scale + dy;
                    if px < size.width && py < size.height {
                        put(rgba, size, px, py, ink);
                    }
                }
            }
        }
    }
}

/// Returns the 5x7 bitmap for `ch`, or a blank if the font lacks it.
///
/// Only the glyphs the pattern draws are written out, to avoid a font
/// dependency.
fn glyph(ch: char) -> [u8; GLYPH_ROWS] {
    match ch {
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        '6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        _ => [0x00; GLYPH_ROWS],
    }
}

/// Writes one opaque pixel.
fn put(rgba: &mut [u8], size: Size, x: u32, y: u32, colour: [u8; 3]) {
    let offset = ((y * size.width + x) * 4) as usize;
    rgba[offset..offset + 3].copy_from_slice(&colour);
    rgba[offset + 3] = 0xff;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small enough to paint quickly, large enough that every band has rows.
    const SIZE: Size = Size {
        width: 320,
        height: 240,
    };

    /// Paints one frame and returns its pixels.
    fn frame(count: u64, media: Duration, wall: SystemTime) -> Vec<u8> {
        Canvas::new(SIZE).paint(count, media, wall).to_vec()
    }

    /// Returns the colour at (`x`, `y`).
    fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 3] {
        let offset = ((y * SIZE.width + x) * 4) as usize;
        [rgba[offset], rgba[offset + 1], rgba[offset + 2]]
    }

    /// Returns the left edge of the sweeping bar, read off the pixels.
    fn bar_left(rgba: &[u8]) -> u32 {
        let y = Layout::new(SIZE).sweep.bottom - 1;
        (0..SIZE.width)
            .find(|&x| pixel(rgba, x, y) == WHITE)
            .expect("the bar is somewhere on the row")
    }

    /// A quarter of the sweep period moves the bar a quarter of the way across.
    #[test]
    fn the_bar_sweeps_at_the_documented_rate() {
        let start = bar_left(&frame(0, Duration::ZERO, SystemTime::UNIX_EPOCH));
        let later = bar_left(&frame(0, SWEEP / 4, SystemTime::UNIX_EPOCH));
        assert!(start < 4, "the sweep starts at the left edge, not {start}");
        let travelled = later - start;
        let expected = SIZE.width / 4;
        assert!(
            travelled.abs_diff(expected) <= 4,
            "a quarter of {SWEEP:?} moved the bar {travelled} px, expected about {expected}"
        );
    }

    /// The clock and counter lines format as drawn.
    #[test]
    fn the_clock_line_reads_as_time_of_day() {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_millis(45_296_123);
        assert_eq!(clock_text(wall), "T 12:34:56.123");
        assert_eq!(counter_text(1_000_042), "F 000042");
    }

    /// The marker is lit on the gate the tone beeps on, and dark otherwise.
    #[test]
    fn the_marker_lights_for_the_beep_window() {
        let layout = Layout::new(SIZE);
        let lit = |media| {
            let rgba = frame(0, media, SystemTime::UNIX_EPOCH);
            pixel(
                &rgba,
                SIZE.width - 1,
                layout.marker.top + layout.marker.height() / 2,
            ) == YELLOW
        };
        assert!(lit(Duration::ZERO));
        assert!(lit(BEEP_LENGTH / 2));
        assert!(!lit(BEEP_LENGTH + Duration::from_millis(1)));
        assert!(!lit(BEEP_PERIOD - Duration::from_millis(1)));
        assert!(lit(BEEP_PERIOD));
    }

    /// The tone keeps pace with the clock and peaks below full scale.
    #[test]
    fn the_tone_keeps_up_with_the_clock() {
        let (fanout, mut frames) = tokio::sync::broadcast::channel(64);
        let stop = CancellationToken::new();
        let format = AudioFormat {
            sample_rate: TONE_RATE,
            layout: audio::Layout::Mono,
        };
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || run_tone(440.0, format, Gate::Continuous, fanout, stop))
        };
        let started = Instant::now();
        let mut first = None;
        let mut last = Duration::ZERO;
        let mut peak = 0.0f32;
        while started.elapsed() < Duration::from_millis(600) {
            let Ok(frame) = frames.try_recv() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            let at = Duration::from(frame.timestamp);
            first.get_or_insert(at);
            last = at + TONE_FRAME;
            peak = frame
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|sample| f32::from_le_bytes(*sample).abs())
                .fold(peak, f32::max);
        }
        stop.cancel();
        thread.join().expect("the tone thread exits");
        let media = last - first.expect("the tone produced frames");
        let ratio = media.as_secs_f64() / started.elapsed().as_secs_f64();
        assert!(
            ratio > 0.9,
            "the tone ran at {:.0}% of real time",
            ratio * 100.0
        );
        assert!(peak > 0.1 && peak < 0.95, "peak {peak}");
    }
}
