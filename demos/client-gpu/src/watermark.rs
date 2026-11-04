//! A text watermark drawn into an I420 picture with a 5x7 bitmap font.

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the glyph for `c` as seven rows of five bits, leftmost bit first.
///
/// Only the characters a watermark needs are drawn: digits, a few letters and
/// punctuation. Anything else is blank.
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        _ => [0; 7],
    }
}

/// Returns the UTC wall-clock time of day as `HH:MM:SS.mmm`.
pub fn wall_clock() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        now.subsec_millis()
    )
}

/// Draws `text` white on black at the bottom left of the I420 picture `data`.
///
/// The scale follows the width, so the text fits at any size.
pub fn draw(data: &mut [u8], width: usize, height: usize, text: &str) {
    let scale = (width / 130).clamp(1, 6);
    let advance = 6 * scale;
    let (text_width, text_height) = (text.chars().count() * advance, 7 * scale);
    // Even coordinates, so the box lines up with the 2x2 chroma samples.
    let pad = 2 * scale;
    let x0 = 4.min(width) & !1;
    let y0 = height.saturating_sub(text_height + 2 * pad + 4) & !1;
    let box_w = (text_width + 2 * pad).min(width - x0);
    let box_h = (text_height + 2 * pad).min(height - y0);

    let luma = width * height;
    for y in y0..y0 + box_h {
        data[y * width + x0..y * width + x0 + box_w].fill(16);
    }
    let (_, chroma) = data.split_at_mut(luma);
    let (u, v) = chroma.split_at_mut(luma / 4);
    for plane in [u, v] {
        for y in y0 / 2..(y0 + box_h) / 2 {
            let row = y * (width / 2);
            plane[row + x0 / 2..row + (x0 + box_w) / 2].fill(128);
        }
    }
    for (index, c) in text.chars().enumerate() {
        for (row, bits) in glyph(c).into_iter().enumerate() {
            for col in 0..5 {
                if bits >> (4 - col) & 1 == 0 {
                    continue;
                }
                let x = x0 + pad + index * advance + col * scale;
                let y = y0 + pad + row * scale;
                for dy in 0..scale {
                    if x + scale > width || y + dy >= height {
                        continue;
                    }
                    let start = (y + dy) * width + x;
                    data[start..start + scale].fill(235);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_stays_inside_small_pictures() {
        for (width, height) in [(64, 48), (320, 180), (1280, 720)] {
            let mut data = vec![0u8; width * height * 3 / 2];
            draw(&mut data, width, height, "GPU 12:34:56.789 BLUR");
            assert!(data.contains(&235));
        }
    }
}
