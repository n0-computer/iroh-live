use crate::av::{
    DecodeConfig, DecodedFrame, PixelFormat, VideoDecoder, VideoEncoder, VideoFormat, VideoFrame,
};
use hang::catalog::VideoConfig;

/// Create a solid-color RGBA frame.
pub(crate) fn make_rgba_frame(w: u32, h: u32, r: u8, g: u8, b: u8) -> VideoFrame {
    let pixel = [r, g, b, 255u8];
    let raw: Vec<u8> = pixel.repeat((w * h) as usize);
    VideoFrame {
        format: VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [w, h],
        },
        raw: raw.into(),
    }
}

/// Create a synthetic test pattern frame with spatial detail and per-frame variation.
///
/// Generates SMPTE-style color bars in the top 2/3 of the frame and a horizontal
/// gradient in the bottom 1/3. A vertical stripe moves across the frame based on
/// `frame_index`, giving inter-frame motion for P-frame encoding.
pub(crate) fn make_test_pattern(w: u32, h: u32, frame_index: u32) -> VideoFrame {
    let mut raw = vec![0u8; (w * h * 4) as usize];

    // SMPTE color bars: white, yellow, cyan, green, magenta, red, blue
    let bars: [(u8, u8, u8); 7] = [
        (192, 192, 192),
        (192, 192, 0),
        (0, 192, 192),
        (0, 192, 0),
        (192, 0, 192),
        (192, 0, 0),
        (0, 0, 192),
    ];
    let bar_boundary = h * 2 / 3;

    for y in 0..h {
        for x in 0..w {
            let offset = ((y * w + x) * 4) as usize;
            let (r, g, b) = if y < bar_boundary {
                // Color bars region
                let bar_idx = (x * 7 / w) as usize;
                bars[bar_idx.min(6)]
            } else {
                // Horizontal gradient region
                let v = (x * 255 / w.max(1)) as u8;
                (v, v, v)
            };
            raw[offset] = r;
            raw[offset + 1] = g;
            raw[offset + 2] = b;
            raw[offset + 3] = 255;
        }
    }

    // Moving vertical stripe (16px wide, wraps around) for inter-frame motion
    let stripe_x = (frame_index * 8) % w;
    let stripe_w = 16.min(w);
    for y in 0..h {
        for dx in 0..stripe_w {
            let x = (stripe_x + dx) % w;
            let offset = ((y * w + x) * 4) as usize;
            raw[offset] = 255;
            raw[offset + 1] = 255;
            raw[offset + 2] = 255;
            raw[offset + 3] = 255;
        }
    }

    VideoFrame {
        format: VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [w, h],
        },
        raw: raw.into(),
    }
}

/// Encode `n` solid-color frames with any VideoEncoder, return packets.
pub(crate) fn video_encode(
    enc: &mut impl VideoEncoder,
    w: u32,
    h: u32,
    r: u8,
    g: u8,
    b: u8,
    n: usize,
) -> Vec<hang::Frame> {
    let mut packets = Vec::new();
    for _ in 0..n {
        enc.push_frame(make_rgba_frame(w, h, r, g, b)).unwrap();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }
    }
    packets
}

/// Encode `n` test-pattern frames with any VideoEncoder, return packets.
pub(crate) fn video_encode_pattern(
    enc: &mut impl VideoEncoder,
    w: u32,
    h: u32,
    n: usize,
) -> Vec<hang::Frame> {
    let mut packets = Vec::new();
    for i in 0..n {
        enc.push_frame(make_test_pattern(w, h, i as u32)).unwrap();
        while let Some(pkt) = enc.pop_packet().unwrap() {
            packets.push(pkt);
        }
    }
    packets
}

/// Decode all packets with any VideoDecoder type, return decoded frames.
pub(crate) fn video_decode<D: VideoDecoder>(
    config: &VideoConfig,
    packets: Vec<hang::Frame>,
) -> Vec<DecodedFrame> {
    let decode_config = DecodeConfig::default();
    let mut dec = D::new(config, &decode_config).unwrap();
    let mut frames = Vec::new();
    for pkt in packets {
        dec.push_packet(pkt).unwrap();
        if let Some(frame) = dec.pop_frame().unwrap() {
            frames.push(frame);
        }
    }
    frames
}

/// Assert decoded frames have correct dimensions and the center pixel
/// approximately matches the expected color (within `tolerance`).
#[allow(
    clippy::too_many_arguments,
    reason = "test helper with clear parameters"
)]
pub(crate) fn assert_video_roundtrip(
    frames: &[DecodedFrame],
    w: u32,
    h: u32,
    expected_r: u8,
    expected_g: u8,
    expected_b: u8,
    tolerance: u8,
    min_frames: usize,
) {
    assert!(
        frames.len() >= min_frames,
        "expected >= {min_frames} frames, got {}",
        frames.len()
    );
    // Check the last frame (encoder has stabilized by then)
    let last = frames.last().unwrap();
    let img = last.img();
    assert_eq!(img.width(), w);
    assert_eq!(img.height(), h);
    let pixel = img.get_pixel(w / 2, h / 2);
    assert!(
        pixel[0].abs_diff(expected_r) <= tolerance,
        "R: expected ~{expected_r}, got {} (tolerance {tolerance})",
        pixel[0]
    );
    assert!(
        pixel[1].abs_diff(expected_g) <= tolerance,
        "G: expected ~{expected_g}, got {} (tolerance {tolerance})",
        pixel[1]
    );
    assert!(
        pixel[2].abs_diff(expected_b) <= tolerance,
        "B: expected ~{expected_b}, got {} (tolerance {tolerance})",
        pixel[2]
    );
    assert_eq!(pixel[3], 255, "alpha should be 255");
}

/// Assert that decoded frames have correct dimensions and are not all black.
pub(crate) fn assert_video_not_black(frames: &[DecodedFrame], w: u32, h: u32, min_frames: usize) {
    assert!(
        frames.len() >= min_frames,
        "expected >= {min_frames} frames, got {}",
        frames.len()
    );
    let last = frames.last().unwrap();
    let img = last.img();
    assert_eq!(img.width(), w);
    assert_eq!(img.height(), h);
    // Sample several pixels across the frame and check they're not all zero.
    let mut total_brightness: u64 = 0;
    let sample_points = [
        (w / 4, h / 4),
        (w / 2, h / 4),
        (3 * w / 4, h / 4),
        (w / 4, h / 2),
        (w / 2, h / 2),
        (3 * w / 4, h / 2),
    ];
    for (x, y) in sample_points {
        let pixel = img.get_pixel(x, y);
        total_brightness += pixel[0] as u64 + pixel[1] as u64 + pixel[2] as u64;
    }
    assert!(
        total_brightness > 0,
        "decoded frame is all black — encoder/decoder pipeline broken"
    );
}
