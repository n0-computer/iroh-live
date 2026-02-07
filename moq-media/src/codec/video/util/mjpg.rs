use anyhow::Result;
use bytes::Bytes;
use image::ImageFormat;

use crate::av::{PixelFormat, VideoFormat, VideoFrame};

/// Stateless MJPEG decoder using the `image` crate.
#[derive(Debug)]
pub(crate) struct MjpgDecoder;

impl MjpgDecoder {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self)
    }

    /// Decode a single MJPEG/JPEG frame into an RGBA [`VideoFrame`].
    pub(crate) fn decode_frame(&mut self, data: &[u8]) -> Result<VideoFrame> {
        let img = image::load_from_memory_with_format(data, ImageFormat::Jpeg)?;
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        Ok(VideoFrame {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [w, h],
            },
            raw: Bytes::from(rgba.into_raw()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a minimal valid JPEG from a solid-color image.
    fn make_jpeg(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        use image::{ImageEncoder, Rgb, RgbImage, codecs::jpeg::JpegEncoder};
        let img = RgbImage::from_fn(w, h, |_, _| Rgb([r, g, b]));
        let mut buf = Vec::new();
        let encoder = JpegEncoder::new(&mut buf);
        encoder
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    #[test]
    fn decode_valid_jpeg() {
        let jpeg_data = make_jpeg(16, 16, 255, 0, 0);
        let mut decoder = MjpgDecoder::new().unwrap();
        let frame = decoder.decode_frame(&jpeg_data).unwrap();
        assert_eq!(frame.format.dimensions, [16, 16]);
        assert_eq!(frame.format.pixel_format, PixelFormat::Rgba);
        // RGBA data: 16*16*4 bytes
        assert_eq!(frame.raw.len(), 16 * 16 * 4);
        // Check first pixel is approximately red (JPEG is lossy)
        assert!(frame.raw[0] > 200, "R channel should be high");
        assert!(frame.raw[1] < 50, "G channel should be low");
        assert!(frame.raw[2] < 50, "B channel should be low");
        assert_eq!(frame.raw[3], 255, "A channel should be 255");
    }

    #[test]
    fn decode_various_sizes() {
        let mut decoder = MjpgDecoder::new().unwrap();
        for (w, h) in [(1, 1), (16, 16), (320, 240)] {
            let jpeg_data = make_jpeg(w, h, 128, 128, 128);
            let frame = decoder.decode_frame(&jpeg_data).unwrap();
            assert_eq!(frame.format.dimensions, [w, h]);
        }
    }

    #[test]
    fn decode_corrupt_jpeg() {
        let mut decoder = MjpgDecoder::new().unwrap();
        // Random bytes
        assert!(decoder.decode_frame(&[0xFF, 0xD8, 0x00, 0x42]).is_err());
        // Empty
        assert!(decoder.decode_frame(&[]).is_err());
        // Truncated
        let valid = make_jpeg(8, 8, 0, 0, 0);
        assert!(decoder.decode_frame(&valid[..valid.len() / 2]).is_err());
    }
}
