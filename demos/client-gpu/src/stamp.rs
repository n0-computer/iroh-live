//! A frame id carried in the picture.
//!
//! The publisher rebases every source timestamp onto its own clock, so a
//! timestamp does not survive the round trip in a form the client can match.
//! The pixels do. The client paints a 16 bit word as black and white blocks in
//! the top left corner: a 12 bit frame id and a 4 bit check. The block width is
//! a fraction of the frame width, so the word survives scaling to another
//! rendition and the loss of an H.264 encoder.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use iroh_live::media::video::Frame;

/// Number of blocks in the word.
const BITS: usize = 16;
/// Ids wrap at this value.
const IDS: usize = 1 << 12;
const BLACK: u8 = 16;
const WHITE: u8 = 235;
/// How long a sent frame stays matchable.
const MAX_AGE: Duration = Duration::from_secs(10);

/// Returns the block side for a frame of `width`.
fn block(width: usize) -> usize {
    ((width / 32) & !1).max(4)
}

/// Returns the 16 bit word for `id`: the id, then an inverted nibble xor.
///
/// The check is inverted so that a black or a white corner is not a valid word.
fn encode(id: u16) -> u16 {
    let id = id & (IDS as u16 - 1);
    let check = !(id ^ (id >> 4) ^ (id >> 8)) & 0xf;
    (id << 4) | check
}

/// Paints `id` into the luma plane of the I420 `data`, of `width` by `height`.
pub fn write(data: &mut [u8], width: usize, height: usize, id: u16) {
    let side = block(width);
    if width < side * BITS || height < side {
        return;
    }
    let word = encode(id);
    for bit in 0..BITS {
        let value = if word >> (BITS - 1 - bit) & 1 == 1 {
            WHITE
        } else {
            BLACK
        };
        for y in 0..side {
            let start = y * width + bit * side;
            data[start..start + side].fill(value);
        }
    }
}

/// Reads the id painted into `frame`, if a valid word is there.
///
/// Returns `None` for a frame without CPU pixels, or one whose corner does not
/// hold a word, as the first pictures of a new stream may not.
pub fn read(frame: &Frame) -> Option<u16> {
    let image = frame.surface.to_i420().ok()?;
    let (width, height) = (image.width() as usize, image.height() as usize);
    let side = block(width);
    if width < side * BITS || height < side {
        return None;
    }
    let luma = image.data();
    let mut word = 0u16;
    for bit in 0..BITS {
        // The middle of the block only: its edges blur into the neighbours.
        let (lo, hi) = (side / 4, side - side / 4);
        let mut sum = 0usize;
        for y in lo..hi {
            for x in bit * side + lo..bit * side + hi {
                sum += usize::from(luma[y * width + x]);
            }
        }
        let mean = sum / ((hi - lo) * (hi - lo));
        word = word << 1 | u16::from(mean >= usize::from(BLACK / 2 + WHITE / 2));
    }
    let id = word >> 4;
    (encode(id) == word).then_some(id)
}

/// The instants at which frames were captured, by id.
#[derive(Debug)]
pub struct SentLog {
    next: Mutex<u16>,
    at: Mutex<Vec<Option<Instant>>>,
}

impl Default for SentLog {
    fn default() -> Self {
        Self {
            next: Mutex::new(0),
            at: Mutex::new(vec![None; IDS]),
        }
    }
}

impl SentLog {
    /// Takes the next id and records `now` as its capture time.
    pub fn next_id(&self, now: Instant) -> u16 {
        let mut next = self.next.lock().expect("poisoned");
        let id = *next;
        *next = (id + 1) % IDS as u16;
        self.at.lock().expect("poisoned")[usize::from(id)] = Some(now);
        id
    }

    /// Returns the time since the frame `id` was captured, if it is known.
    pub fn since(&self, id: u16) -> Option<Duration> {
        let at = self.at.lock().expect("poisoned")[usize::from(id) % IDS]?;
        Some(at.elapsed()).filter(|age| *age < MAX_AGE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_not_flat() {
        for id in 0..IDS as u16 {
            let word = encode(id);
            assert_ne!(word, 0);
            assert_ne!(word, u16::MAX);
            assert_eq!(word >> 4, id);
        }
    }

    #[test]
    fn a_written_id_reads_back() {
        let (width, height) = (640, 360);
        let mut data = vec![128u8; width * height * 3 / 2];
        write(&mut data, width, height, 1234);
        let frame = i420_frame(data, width, height);
        assert_eq!(read(&frame), Some(1234));
    }

    #[test]
    fn a_flat_picture_has_no_id() {
        let frame = i420_frame(vec![16u8; 640 * 360 * 3 / 2], 640, 360);
        assert_eq!(read(&frame), None);
    }

    fn i420_frame(data: Vec<u8>, width: usize, height: usize) -> Frame {
        use iroh_live::media::video::{I420, Size, Surface};
        let size = Size::new(width as u32, height as u32);
        let image = I420::new(size, data).expect("a valid size");
        let timestamp = moq_net::Timestamp::from_micros(0).expect("in range");
        Frame::new(Surface::I420(image), timestamp)
    }
}
