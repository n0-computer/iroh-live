use crate::format::VideoFrame;

/// Horizontal gradient: black on left, white on right.
///
/// Catches banding artifacts and quantization step errors in smooth ramps.
pub(super) fn horizontal_gradient(w: u32, h: u32) -> VideoFrame {
    let mut raw = vec![0u8; (w * h * 4) as usize];
    let denom = w.saturating_sub(1).max(1);
    for y in 0..h {
        for x in 0..w {
            let v = (x * 255 / denom) as u8;
            let off = ((y * w + x) * 4) as usize;
            raw[off] = v;
            raw[off + 1] = v;
            raw[off + 2] = v;
            raw[off + 3] = 255;
        }
    }
    make_frame(w, h, raw)
}

/// Vertical gradient: black on top, white on bottom.
///
/// Tests the orthogonal axis to horizontal gradient — catches
/// direction-dependent transform bugs.
#[allow(dead_code, reason = "useful for future pattern tests")]
pub(super) fn vertical_gradient(w: u32, h: u32) -> VideoFrame {
    let mut raw = vec![0u8; (w * h * 4) as usize];
    let denom = h.saturating_sub(1).max(1);
    for y in 0..h {
        let v = (y * 255 / denom) as u8;
        for x in 0..w {
            let off = ((y * w + x) * 4) as usize;
            raw[off] = v;
            raw[off + 1] = v;
            raw[off + 2] = v;
            raw[off + 3] = 255;
        }
    }
    make_frame(w, h, raw)
}

/// Checkerboard pattern with given cell size.
///
/// Maximum spatial frequency pattern (at cell_size=1). Catches spatial
/// distortion, aliasing, and chroma subsampling artifacts. Cell sizes of
/// 1-2px are worst-case for 4:2:0 chroma subsampling.
pub(super) fn checkerboard(w: u32, h: u32, cell_size: u32) -> VideoFrame {
    let mut raw = vec![0u8; (w * h * 4) as usize];
    let cs = cell_size.max(1);
    for y in 0..h {
        for x in 0..w {
            let is_white = ((x / cs) + (y / cs)).is_multiple_of(2);
            let v = if is_white { 255 } else { 0 };
            let off = ((y * w + x) * 4) as usize;
            raw[off] = v;
            raw[off + 1] = v;
            raw[off + 2] = v;
            raw[off + 3] = 255;
        }
    }
    make_frame(w, h, raw)
}

/// Gray ramp: 256 discrete gray levels distributed across the width.
///
/// Tests full dynamic range reproduction and catches clamping bugs
/// (studio range [16-235] vs full range [0-255] mismatches).
pub(super) fn gray_ramp(w: u32, h: u32) -> VideoFrame {
    let mut raw = vec![0u8; (w * h * 4) as usize];
    let denom = w.saturating_sub(1).max(1);
    for y in 0..h {
        for x in 0..w {
            let v = (x * 255 / denom) as u8;
            let off = ((y * w + x) * 4) as usize;
            raw[off] = v;
            raw[off + 1] = v;
            raw[off + 2] = v;
            raw[off + 3] = 255;
        }
    }
    make_frame(w, h, raw)
}

/// Color ramp: hue sweep at full saturation across the width.
///
/// Tests chroma accuracy across the full hue range. Reveals
/// hue-dependent quantization and colorspace conversion errors.
pub(super) fn color_ramp(w: u32, h: u32) -> VideoFrame {
    let mut raw = vec![0u8; (w * h * 4) as usize];
    let denom = w.saturating_sub(1).max(1) as f32;
    for y in 0..h {
        for x in 0..w {
            let hue = x as f32 / denom * 360.0;
            let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
            let off = ((y * w + x) * 4) as usize;
            raw[off] = r;
            raw[off + 1] = g;
            raw[off + 2] = b;
            raw[off + 3] = 255;
        }
    }
    make_frame(w, h, raw)
}

/// Primary and secondary color patches for color accuracy testing.
///
/// Covers all six primary/secondary colors plus neutrals at extremes.
/// These are the points that diverge most between BT.601 and BT.709
/// colorspace matrices, making them ideal for detecting matrix mismatches.
pub(super) fn color_patches() -> Vec<(&'static str, u8, u8, u8)> {
    vec![
        ("red", 255, 0, 0),
        ("green", 0, 255, 0),
        ("blue", 0, 0, 255),
        ("cyan", 0, 255, 255),
        ("magenta", 255, 0, 255),
        ("yellow", 255, 255, 0),
        ("white", 255, 255, 255),
        ("gray_50", 128, 128, 128),
        ("near_black", 16, 16, 16),
        ("near_white", 235, 235, 235),
    ]
}

/// Standard test resolutions including edge cases.
///
/// Includes standard 16:9 presets plus odd dimensions that catch
/// alignment bugs (non-mod-2, non-mod-16 widths).
pub(super) fn test_resolutions() -> Vec<(&'static str, u32, u32)> {
    vec![
        ("180p", 320, 180),
        ("360p", 640, 360),
        ("720p", 1280, 720),
        ("1080p", 1920, 1080),
        ("odd_126x62", 126, 62),
    ]
}

fn make_frame(w: u32, h: u32, raw: Vec<u8>) -> VideoFrame {
    VideoFrame::new_rgba(raw.into(), w, h, std::time::Duration::ZERO)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0..60 => (c, x, 0.0),
        60..120 => (x, c, 0.0),
        120..180 => (0.0, c, x),
        180..240 => (0.0, x, c),
        240..300 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}
