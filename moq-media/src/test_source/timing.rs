//! A generated picture and tone built for diagnosing playback.
//!
//! The colour gradient in the parent module proves that bytes are moving and
//! nothing else. This pattern answers the questions someone asks while looking
//! at a live stream: is it smooth, how far behind is it, and do the picture and
//! the sound still agree? Every element earns its place by making one fault
//! visible.
//!
//! - A white bar sweeps left to right, one crossing every [`SWEEP`]. Judder and
//!   dropped frames stand out on a moving edge and hide completely on a static
//!   picture. The bar's edge is straight and spans most of the frame height, so
//!   tearing and shear break it into offset segments.
//! - A ruler across the top of the sweep band divides the width into tenths,
//!   one tick every 200 ms of the bar's travel, to read the rate off rather
//!   than guess at it.
//! - Vertical stripes at four pitches sit below the sweep. The three grey
//!   columns lose their stripes when the picture is scaled or the encoder runs
//!   out of bits; the magenta and green column has a flat luma by construction,
//!   so it goes a solid olive only when chroma detail is thrown away.
//! - A frame counter and a clock, drawn as digits sized to the frame. The
//!   counter makes a dropped frame countable. The clock makes latency
//!   measurable: photograph the publisher's screen and the player's screen
//!   together and subtract the two stamps, or compare the stamp against a wall
//!   clock.
//! - A marker band lights for [`BEEP_LENGTH`] every [`BEEP_PERIOD`], on the
//!   same media time as the tone's beep. Whether the flash and the beep land
//!   together is then something you see and hear rather than something you
//!   estimate.
//!
//! [`video`] and [`audio`] both stamp on a shared [`Clock`], so hand them the
//! same one: the flash and the beep are aligned by that clock and by nothing
//! else.

use std::time::{Duration, SystemTime};

use moq_mux::Clock;
use moq_video::{Frame, Size, Surface};
use n0_future::boxed::BoxStream;

use super::Gate;
use crate::publish::{AudioSource, VideoSource};

/// How long the bar takes to sweep the frame once.
///
/// Two seconds across the width puts a ruler tick every 200 ms, which is slow
/// enough to follow by eye and fast enough that a stall of a few frames is a
/// visible hesitation rather than a rounding error.
pub const SWEEP: Duration = Duration::from_secs(2);

/// How often the marker flashes and the tone beeps.
pub const BEEP_PERIOD: Duration = Duration::from_secs(1);

/// How long each flash and beep lasts.
///
/// Three frames at 30 fps: short enough to time against, long enough that no
/// single dropped frame can hide the whole event.
pub const BEEP_LENGTH: Duration = Duration::from_millis(100);

/// Frequency of the beep, in hertz. An octave above concert A, which carries
/// through a laptop speaker and a phone microphone alike.
pub const BEEP_HZ: f64 = 880.0;

/// When the marker is lit and the tone sounds, in media time.
const BEEP: Gate = Gate::Pulse {
    period: BEEP_PERIOD,
    length: BEEP_LENGTH,
};

/// The pattern at `size` and `framerate`, stamped on `clock`.
///
/// Give [`audio`] the same clock: the flash and the beep are one measurement,
/// and they only agree if both tracks land on one timeline.
///
/// # Examples
///
/// ```no_run
/// use moq_media::{test_source::timing, video::Size};
///
/// let clock = moq_mux::Clock::new();
/// let video = timing::video(Size::new(1280, 720), 30, clock);
/// let audio = timing::audio(48_000, 2, clock);
/// ```
pub fn video(size: Size, framerate: u32, clock: Clock) -> VideoSource {
    let framerate = u64::from(framerate.max(1));
    let canvas = Canvas::new(size);

    // Pace against an absolute schedule rather than sleeping one interval per
    // frame. A sleep always overshoots, so a relative wait makes the pattern
    // run slower than the clock it draws, which is the one fault a timing
    // source must not have.
    let started = tokio::time::Instant::now();

    let frames: BoxStream<Frame> = Box::pin(n0_future::stream::unfold(
        (0u64, canvas),
        move |(count, mut canvas)| async move {
            let due = Duration::from_micros(count * 1_000_000 / framerate);
            tokio::time::sleep_until(started + due).await;
            // Read the clock after the wait and paint from what it says, so the
            // digits describe the frame that carries them rather than the one
            // before it.
            let micros = clock.micros();
            let media = Duration::from_micros(micros);
            let rgba = canvas.paint(count, media, SystemTime::now());
            let surface = Surface::rgba(rgba, size).expect("the pattern is well formed");
            let timestamp =
                moq_net::Timestamp::from_micros(micros).expect("clock micros out of range");
            Some((Frame::new(surface, timestamp), (count + 1, canvas)))
        },
    ));
    VideoSource::Frames(frames)
}

/// The beeping tone that goes with [`video`], stamped on `clock`.
///
/// One [`BEEP_HZ`] pulse of [`BEEP_LENGTH`] every [`BEEP_PERIOD`], silent in
/// between, on the media timeline the marker flashes on.
pub fn audio(sample_rate: u32, channels: u32, clock: Clock) -> AudioSource {
    super::tone(
        BEEP_HZ,
        sample_rate,
        channels,
        Duration::from_micros(clock.micros()),
        BEEP,
    )
}

/// Rows of the frame one element of the pattern occupies.
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
/// The bands are cut in sixteenths of the height: two for each line of digits
/// and the rest split between the sweep, the stripes, and the marker. Every
/// element scales with the frame, so the pattern reads the same at 320x240 as
/// at 1920x1080.
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

    /// Rows the sweeping bar spans.
    ///
    /// The bar runs from the top of its own band to the bottom of the frame,
    /// crossing the stripes and the marker. A long straight edge is what makes
    /// tearing legible, and a short one shows nothing.
    fn bar(self) -> Band {
        Band {
            top: self.sweep.top,
            bottom: self.size.height,
        }
    }

    /// Width of the sweeping bar. Wide enough to survive an encoder at a low
    /// bitrate, narrow enough to place against a ruler tick.
    fn bar_width(self) -> u32 {
        (self.size.width / 64).max(4)
    }
}

/// Ink for the digits and the sweeping bar.
const WHITE: [u8; 3] = [0xff, 0xff, 0xff];

/// Behind everything that is not lit.
const BLACK: [u8; 3] = [0x00, 0x00, 0x00];

/// The lit marker. Nothing else in the pattern is yellow, so a camera pointed
/// at two screens at once tells the flashes apart from the rest.
const YELLOW: [u8; 3] = [0xff, 0xff, 0x00];

/// The ruler above the sweep.
const GREY: [u8; 3] = [0x60, 0x60, 0x60];

/// The marker between flashes. Dark, but not the black of the sweep band: the
/// band has to be findable in a still frame for a flash in the next one to mean
/// anything.
const DIM: [u8; 3] = [0x28, 0x28, 0x28];

/// The four stripe columns: pitch in pixels, then the two colours.
///
/// The greys go from one pixel to four, so the column where the stripes turn to
/// mush says how much detail the path is losing. The last pair is chroma alone:
/// magenta and this green have the same BT.601 luma (about 105), so the luma
/// plane across that column is flat and only the colour difference carries the
/// stripes. They survive 4:2:0 subsampling, which samples chroma every second
/// pixel; they do not survive a picture that has been scaled or a chroma
/// resampler cutting corners.
const STRIPES: [(u32, [u8; 3], [u8; 3]); 4] = [
    (1, WHITE, BLACK),
    (2, WHITE, BLACK),
    (4, WHITE, BLACK),
    (4, [0xff, 0x00, 0xff], [0x00, 0xb3, 0x00]),
];

/// The buffers one publication paints from.
///
/// Most of the frame is the same in every one: the stripes, the ruler, and the
/// black behind the digits. Painting those once and copying them back costs a
/// memcpy per frame instead of a pass over every pixel, which is what keeps a
/// 720p pattern comfortably inside its frame interval.
#[derive(Debug)]
struct Canvas {
    layout: Layout,
    /// Everything that does not change from frame to frame.
    background: Vec<u8>,
    /// The frame being painted.
    rgba: Vec<u8>,
}

impl Canvas {
    /// A canvas for `size`, with the static half of the pattern already drawn.
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
    /// `media` is the frame's own presentation time, and it drives the sweep
    /// and the marker: both then move at a constant rate on the timeline a
    /// player reconstructs, so a stall shows up as a jump rather than as smooth
    /// motion. `count` and `wall` are drawn as digits and describe nothing but
    /// themselves.
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

/// The frame counter line: six digits, which wrap after nine hours at 30 fps.
fn counter_text(count: u64) -> String {
    format!("F {:06}", count % 1_000_000)
}

/// The clock line: the time of day in UTC, to the millisecond.
///
/// UTC rather than local time, because the machine that publishes and the
/// machine that plays need not agree on a timezone, and the difference between
/// two stamps is what a latency measurement reads.
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

/// Left edge of the sweeping bar at `media`.
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
        // Integer division puts the last column one pixel wider on a width that
        // does not divide by four, which no measurement depends on.
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
/// Every band is the same on every row, so one row is painted pixel by pixel
/// and the remainder is memcpy. At 720p that is the difference between a pass
/// over the whole frame and a handful of copies.
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
///
/// The bar crosses one gap every tenth of [`SWEEP`], so the ticks turn "it
/// looks slow" into a number.
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
/// The size follows the frame so the digits stay readable in a photograph of a
/// small panel, which is how latency gets measured.
fn draw_line(rgba: &mut [u8], size: Size, band: Band, text: &str, ink: [u8; 3]) {
    let columns = text.chars().count() as u32 * (GLYPH_COLS + 1);
    // Leave a sixteenth of the width as a margin, and two rows of the band, so
    // no glyph touches an edge an encoder is about to blur.
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

/// Draws one glyph with its top-left corner at (`x`, `y`), each font pixel
/// `scale` pixels square.
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

/// The bitmap for `ch`, five columns by seven rows, or a blank for a character
/// the font does not carry.
///
/// A font stack for two labels and twelve digits would be a dependency to keep
/// current, so the glyphs the pattern draws are written out here and nothing
/// else is drawable.
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
    use n0_future::StreamExt;

    use super::*;

    /// Small enough to paint quickly, large enough that every band has rows.
    const SIZE: Size = Size {
        width: 320,
        height: 240,
    };

    /// Paints one frame and hands back the pixels.
    fn frame(count: u64, media: Duration, wall: SystemTime) -> Vec<u8> {
        Canvas::new(SIZE).paint(count, media, wall).to_vec()
    }

    /// The colour at (`x`, `y`).
    fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 3] {
        let offset = ((y * SIZE.width + x) * 4) as usize;
        [rgba[offset], rgba[offset + 1], rgba[offset + 2]]
    }

    /// The rows of `band`, for comparing one part of two frames.
    fn band(rgba: &[u8], band: Band) -> &[u8] {
        let row = SIZE.width as usize * 4;
        &rgba[band.top as usize * row..band.bottom as usize * row]
    }

    /// The left edge of the sweeping bar, read back off the painted pixels.
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

    /// The bar is back where it started one period on, so a viewer counting
    /// crossings is counting whole sweeps.
    #[test]
    fn the_bar_returns_to_the_left_each_period() {
        let start = bar_left(&frame(0, Duration::ZERO, SystemTime::UNIX_EPOCH));
        let wrapped = bar_left(&frame(0, SWEEP * 3, SystemTime::UNIX_EPOCH));
        assert_eq!(start, wrapped);
    }

    /// The counter reads differently a hundred frames apart, which is what
    /// makes a dropped frame countable off a recording.
    #[test]
    fn the_counter_digits_change_with_the_frame_number() {
        let layout = Layout::new(SIZE);
        let first = frame(0, Duration::ZERO, SystemTime::UNIX_EPOCH);
        let hundredth = frame(100, Duration::ZERO, SystemTime::UNIX_EPOCH);

        assert_ne!(
            band(&first, layout.counter),
            band(&hundredth, layout.counter)
        );
        // Only the counter moved: everything else in that frame is the same,
        // so a diff anywhere else would mean the bands overlap.
        assert_eq!(band(&first, layout.clock), band(&hundredth, layout.clock));
    }

    /// The clock reads the wall clock, so two screens photographed together
    /// carry the numbers a latency measurement subtracts.
    #[test]
    fn the_clock_digits_change_with_the_time_of_day() {
        let layout = Layout::new(SIZE);
        let early = SystemTime::UNIX_EPOCH + Duration::from_millis(45_296_123);
        let late = early + Duration::from_millis(500);

        let first = frame(7, Duration::ZERO, early);
        let second = frame(7, Duration::ZERO, late);
        assert_ne!(band(&first, layout.clock), band(&second, layout.clock));
        assert_eq!(band(&first, layout.counter), band(&second, layout.counter));
    }

    /// 12:34:56.123 UTC, spelled the way the digits are drawn.
    #[test]
    fn the_clock_line_reads_as_time_of_day() {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_millis(45_296_123);
        assert_eq!(clock_text(wall), "T 12:34:56.123");
        assert_eq!(counter_text(1_000_042), "F 000042");
    }

    /// The stripe columns alternate at the pitch they are declared with.
    #[test]
    fn the_stripes_alternate_at_four_pitches() {
        let layout = Layout::new(SIZE);
        let rgba = frame(0, Duration::ZERO, SystemTime::UNIX_EPOCH);
        let y = layout.stripes.top + layout.stripes.height() / 2;

        for (column, (pitch, first, second)) in STRIPES.into_iter().enumerate() {
            // Eight pixels into the column, clear of its boundaries and of the
            // sweeping bar, which is parked at the left edge at media zero.
            // Rounding down to a whole pair of stripes lands on the first
            // colour, whatever the pitch.
            let start = SIZE.width * column as u32 / STRIPES.len() as u32 + 8;
            let start = start - start % (2 * pitch);
            assert_eq!(
                pixel(&rgba, start, y),
                first,
                "column {column} does not start on its first colour"
            );
            assert_eq!(
                pixel(&rgba, start + pitch, y),
                second,
                "column {column} does not alternate every {pitch} px"
            );
            assert_eq!(
                pixel(&rgba, start + 2 * pitch, y),
                first,
                "column {column} does not repeat every {} px",
                2 * pitch
            );
        }
    }

    /// The marker is lit for the length of the beep and dark for the rest of
    /// the period.
    #[test]
    fn the_marker_lights_for_the_beep_window() {
        let layout = Layout::new(SIZE);
        let lit = |media| {
            let rgba = frame(0, media, SystemTime::UNIX_EPOCH);
            // Clear of the sweeping bar, which crosses this band too.
            pixel(
                &rgba,
                SIZE.width - 1,
                layout.marker.top + layout.marker.height() / 2,
            ) == YELLOW
        };

        assert!(lit(Duration::ZERO));
        assert!(lit(BEEP_LENGTH / 2));
        assert!(!lit(BEEP_LENGTH));
        assert!(!lit(BEEP_PERIOD / 2));
        assert!(lit(BEEP_PERIOD * 3));
    }

    /// The tone sounds exactly while the marker is lit.
    ///
    /// This is the measurement the pattern exists for: if the flash and the
    /// beep disagree here, no amount of watching a player will tell you whether
    /// the fault is the player's or the source's.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn the_beep_lands_on_the_flashing_frame() {
        let layout = Layout::new(SIZE);
        let clock = Clock::new();
        let AudioSource::Frames { mut frames, .. } = audio(48_000, 1, clock) else {
            panic!("the generated tone is a frame source");
        };

        // A little over two beep periods, so both a beep and the silence after
        // it are covered.
        for _ in 0..110 {
            let audio = frames.next().await.expect("the tone yields frames");
            let media = Duration::from(audio.timestamp);
            let peak = audio
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|sample| f32::from_le_bytes(*sample).abs())
                .fold(0.0f32, f32::max);

            let rgba = frame(0, media, SystemTime::UNIX_EPOCH);
            let marker = pixel(
                &rgba,
                SIZE.width - 1,
                layout.marker.top + layout.marker.height() / 2,
            );

            // The threshold clears the ramp at the edges of the pulse: a buffer
            // that overlaps the window at all still peaks well above it.
            assert_eq!(
                marker == YELLOW,
                peak > 0.05,
                "at {media:?} the marker is {marker:?} and the tone peaks at {peak}"
            );
        }
    }

    /// The stream is paced at the frame rate it was asked for.
    ///
    /// Real time rather than tokio's paused clock, because the timestamps come
    /// from a [`Clock`], which reads the machine's. The bounds are wide: this
    /// catches a stream that free-runs or that crawls, not a scheduling slice
    /// lost on a loaded machine.
    #[tokio::test(flavor = "current_thread")]
    async fn the_frames_are_paced_at_the_frame_rate() {
        const FRAMERATE: u32 = 30;
        let interval = Duration::from_secs(1) / FRAMERATE;
        let VideoSource::Frames(mut frames) = video(SIZE, FRAMERATE, Clock::new()) else {
            panic!("the pattern is a frame source");
        };

        let mut previous = Duration::from(
            frames
                .next()
                .await
                .expect("the pattern yields frames")
                .timestamp,
        );
        for _ in 0..3 {
            let next = Duration::from(
                frames
                    .next()
                    .await
                    .expect("the pattern yields frames")
                    .timestamp,
            );
            let step = next - previous;
            assert!(
                step > interval / 2 && step < interval * 3,
                "frames {step:?} apart at {FRAMERATE} fps"
            );
            previous = next;
        }
    }
}
