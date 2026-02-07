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
