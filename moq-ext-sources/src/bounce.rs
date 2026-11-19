//! A test picture drawn for any instant: a bouncing square on a drifting hue.
//!
//! The background drifts through the hues once every [`HUE_PERIOD`], so the
//! average colour of the picture changes. A white square runs left and right
//! at a constant speed, one edge to the other in [`CROSSING`], and turns at the
//! edges. Both follow the time elapsed since the picture started, not a frame
//! count, so the motion stays smooth and comparable whichever instants a
//! consumer draws it at.

use std::{f64::consts::TAU, time::Duration};

use moq_net::Timestamp;

use crate::{
    Error,
    video::{self, I420, Size, Surface},
};

/// How long the background takes through all hues.
const HUE_PERIOD: Duration = Duration::from_secs(12);

/// How long the square takes from one edge to the other.
const CROSSING: Duration = Duration::from_millis(1500);

/// The luma of the background.
const BACKGROUND: u8 = 110;

/// The luma of the square.
const WHITE: u8 = 235;

/// The chroma of a grey, which the square has.
const NEUTRAL: u8 = 128;

/// How far the background's chroma swings from [`NEUTRAL`].
const SATURATION: f64 = 50.0;

/// A bouncing square on a background that drifts through the hues.
///
/// [`draw`](Self::draw) returns the picture for any elapsed time, so a
/// consumer that decides itself when to send a frame can draw it at exactly
/// that instant. [`VideoSource::test_bounce`](crate::VideoSource::test_bounce)
/// draws it at a fixed rate instead.
///
/// The square is two ninths of the height, 80 pixels at 640x360, and sits in
/// the middle row. The pictures are I420.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use moq_ext_sources::BouncePattern;
///
/// let pattern = BouncePattern::new(moq_video::Size::new(640, 360))?;
/// let frame = pattern.draw(Duration::from_millis(250))?;
/// assert_eq!(frame.timestamp.as_micros(), Ok(250_000));
/// # Ok::<_, moq_ext_sources::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct BouncePattern {
    size: Size,
    side: u32,
}

impl BouncePattern {
    /// Returns the pattern drawn at `size`.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] if a dimension of `size` is zero or
    /// odd, which I420 cannot hold.
    pub fn new(size: Size) -> Result<Self, Error> {
        let Size { width, height } = size;
        if width == 0 || height == 0 || width % 2 == 1 || height % 2 == 1 {
            return Err(Error::invalid(format!(
                "the bounce pattern needs an even, non-zero size, not {width}x{height}"
            )));
        }
        // Even, so the square covers whole chroma samples where it can.
        let side = (height * 2 / 9 / 2 * 2).clamp(2, width);
        Ok(Self { size, side })
    }

    /// Returns the size the pattern is drawn at.
    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    /// Returns the left edge of the square `elapsed` after the start, in pixels.
    ///
    /// The square starts at the left edge and bounces between the edges at a
    /// constant speed.
    #[must_use]
    pub fn square_left(&self, elapsed: Duration) -> u32 {
        let travel = f64::from(self.size.width - self.side);
        // Goes from 0 to 2 over a round trip. Past 1 the square is on its way back.
        let phase = (elapsed.as_secs_f64() / CROSSING.as_secs_f64()).rem_euclid(2.0);
        let along = if phase <= 1.0 { phase } else { 2.0 - phase };
        (along * travel).round() as u32
    }

    /// Returns the picture `elapsed` after the start, stamped with `elapsed`.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidConfig`] if `elapsed` is beyond what a
    /// timestamp holds.
    pub fn draw(&self, elapsed: Duration) -> Result<video::Frame, Error> {
        let timestamp = Timestamp::from_micros(elapsed.as_micros() as u64)
            .map_err(|_| Error::invalid(format!("{elapsed:?} is beyond a timestamp")))?;
        let surface = self
            .surface(elapsed)
            .map_err(|err| Error::invalid(err.to_string()))?;
        Ok(video::Frame::new(surface, timestamp))
    }

    /// Returns the picture `elapsed` after the start.
    ///
    /// Only fails if the planes do not fit the size, which [`new`](Self::new) rules out.
    pub(crate) fn surface(&self, elapsed: Duration) -> Result<Surface, video::Error> {
        let (width, height) = (self.size.width as usize, self.size.height as usize);
        let side = self.side as usize;
        let hue = TAU * (elapsed.as_secs_f64() / HUE_PERIOD.as_secs_f64()).fract();
        let u = (f64::from(NEUTRAL) + SATURATION * hue.cos()) as u8;
        let v = (f64::from(NEUTRAL) + SATURATION * hue.sin()) as u8;
        let luma = width * height;
        let quarter = luma / 4;
        let mut data = vec![BACKGROUND; luma];
        data.resize(luma + quarter, u);
        data.resize(luma + 2 * quarter, v);

        let x = self.square_left(elapsed) as usize;
        let y = (height - side) / 2;
        for row in y..y + side {
            data[row * width + x..row * width + x + side].fill(WHITE);
        }
        // Every chroma sample the square touches, so its edges carry no hue.
        let half_width = width / 2;
        let columns = x / 2..(x + side).div_ceil(2);
        for row in y / 2..(y + side).div_ceil(2) {
            for plane in [luma, luma + quarter] {
                let at = plane + row * half_width;
                data[at + columns.start..at + columns.end].fill(NEUTRAL);
            }
        }
        Ok(Surface::I420(I420::new(self.size, data)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Size = Size {
        width: 640,
        height: 360,
    };

    /// Returns the luma plane of the picture at `elapsed`.
    fn luma(pattern: &BouncePattern, elapsed: Duration) -> Vec<u8> {
        let frame = pattern.draw(elapsed).unwrap();
        let size = pattern.size();
        frame.surface.to_i420().unwrap().data()[..(size.width * size.height) as usize].to_vec()
    }

    /// Returns the left edge of the square, read off the middle row.
    fn left_in_picture(pattern: &BouncePattern, elapsed: Duration) -> u32 {
        let size = pattern.size();
        let row = (size.height / 2 * size.width) as usize;
        let luma = luma(pattern, elapsed);
        (0..size.width)
            .find(|&x| luma[row + x as usize] == WHITE)
            .expect("the square is on the middle row")
    }

    /// The frame is I420 at the size asked for, stamped with the elapsed time.
    #[test]
    fn frames_are_i420_at_the_size_and_time() {
        let pattern = BouncePattern::new(SIZE).unwrap();
        let frame = pattern.draw(Duration::from_millis(1234)).unwrap();
        assert_eq!(frame.timestamp.as_micros(), Ok(1_234_000));
        let Surface::I420(image) = &frame.surface else {
            panic!("not I420");
        };
        assert_eq!(image.size(), SIZE);
        assert_eq!(image.data().len(), 640 * 360 * 3 / 2);
    }

    /// The square is 80 pixels at 640x360 and scales with the height.
    #[test]
    fn the_square_scales_with_the_height() {
        assert_eq!(BouncePattern::new(SIZE).unwrap().side, 80);
        assert_eq!(BouncePattern::new(Size::new(1280, 720)).unwrap().side, 160);
        assert_eq!(BouncePattern::new(Size::new(4, 360)).unwrap().side, 4);
        let tiny = BouncePattern::new(Size::new(2, 2)).unwrap();
        tiny.draw(Duration::from_secs(1)).unwrap();
    }

    /// An odd or empty size has no I420 picture.
    #[test]
    fn odd_sizes_are_refused() {
        for size in [Size::new(641, 360), Size::new(640, 361), Size::new(0, 360)] {
            assert!(BouncePattern::new(size).is_err(), "{size:?}");
        }
    }

    /// The square starts at the left, reaches the right after one crossing, and comes back.
    #[test]
    fn the_square_bounces_between_the_edges() {
        let pattern = BouncePattern::new(SIZE).unwrap();
        let right = 640 - 80;
        for (elapsed, expected) in [
            (Duration::ZERO, 0),
            (CROSSING / 2, right / 2),
            (CROSSING, right),
            (CROSSING * 3 / 2, right / 2),
            (CROSSING * 2, 0),
            (CROSSING * 41, right),
        ] {
            assert_eq!(pattern.square_left(elapsed), expected, "at {elapsed:?}");
            assert_eq!(
                left_in_picture(&pattern, elapsed),
                expected,
                "at {elapsed:?}"
            );
        }
    }

    /// The square moves the same distance every 50 ms for a minute, folded at the edges.
    #[test]
    fn the_square_moves_at_a_constant_speed() {
        let pattern = BouncePattern::new(SIZE).unwrap();
        let step = Duration::from_millis(50);
        let right = f64::from(640 - 80);
        let expected = right * step.as_secs_f64() / CROSSING.as_secs_f64();
        let xs: Vec<f64> = (0..1200u32)
            .map(|k| f64::from(pattern.square_left(step * k)))
            .collect();
        for pair in xs.windows(2) {
            let moved = (pair[1] - pair[0]).abs();
            let at_edge = pair.iter().any(|&x| x <= expected || x >= right - expected);
            assert!(
                (moved - expected).abs() <= 1.0 || at_edge,
                "moved {moved} px, expected {expected:.1} px, at {pair:?}"
            );
            assert!(moved > 0.0, "paused at {pair:?}");
        }
    }

    /// The background drifts through the hues and the square stays grey.
    #[test]
    fn the_background_drifts_through_the_hues() {
        let pattern = BouncePattern::new(SIZE).unwrap();
        let chroma = |elapsed: Duration| {
            let frame = pattern.draw(elapsed).unwrap();
            let data = frame.surface.to_i420().unwrap().data().to_vec();
            let (u, v) = (640 * 360, 640 * 360 * 5 / 4);
            // The top-left sample is background, and the square starts at the left.
            let square = 90 * 320 + 10;
            (data[u], data[v], data[u + square], data[v + square])
        };
        assert_eq!(chroma(Duration::ZERO), (178, 128, 128, 128));
        assert_eq!(chroma(HUE_PERIOD / 4).1, 178);
        assert_eq!(chroma(HUE_PERIOD / 2).0, 78);
        assert_eq!(chroma(HUE_PERIOD).0, 178);
    }
}
