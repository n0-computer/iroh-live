use std::time::Duration;

use bytes::{BufMut, BytesMut};
use ffmpeg_next::util::{format::pixel::Pixel, frame::video::Video as FfmpegFrame};
use hang::Timestamp;
use image::{Delay, RgbaImage};

pub(crate) use self::mjpg_decoder::MjpgDecoder;
pub(crate) use self::rescaler::Rescaler;
use crate::av::{self, PixelFormat, VideoFormat, VideoFrame};

mod mjpg_decoder;
mod rescaler;

#[derive(Default, Debug)]
pub(crate) struct StreamClock {
    pub(crate) last_timestamp: Option<hang::Timestamp>,
}

impl StreamClock {
    pub(crate) fn frame_delay(&mut self, timestamp: &hang::Timestamp) -> Duration {
        // Compute interframe delay from provided timestamps
        let delay = match self.last_timestamp {
            None => Duration::ZERO,
            Some(last_timestamp) => timestamp
                .checked_sub(last_timestamp)
                .unwrap_or(Timestamp::ZERO)
                .into(),
        };
        self.last_timestamp = Some(*timestamp);
        delay
    }
}

impl av::VideoFrame {
    pub fn to_ffmpeg(&self) -> FfmpegFrame {
        // Wrap raw RGBA/BGRA data into an ffmpeg frame and encode
        let pixel = match self.format.pixel_format {
            av::PixelFormat::Rgba => Pixel::RGBA,
            av::PixelFormat::Bgra => Pixel::BGRA,
        };
        let [w, h] = self.format.dimensions;
        let mut ff = FfmpegFrame::new(pixel, w, h);
        let stride = ff.stride(0);
        let row_bytes = (w as usize) * 4;
        for y in 0..(h as usize) {
            let dst_off = y * stride;
            let src_off = y * row_bytes;
            ff.data_mut(0)[dst_off..dst_off + row_bytes]
                .copy_from_slice(&self.raw[src_off..src_off + row_bytes]);
        }
        ff
    }
}

impl av::DecodedFrame {
    pub fn from_ffmpeg(frame: &FfmpegFrame, delay: Duration, timestamp: Duration) -> Self {
        let image = ffmpeg_frame_to_image(frame);
        // Compute interframe delay from provided timestamps
        let delay = Delay::from_saturating_duration(delay);
        Self {
            frame: image::Frame::from_parts(image, 0, 0, delay),
            timestamp,
        }
    }
}

/// Convert the ffmpeg frame into an [image] frame.
///
/// Note: This does not do any color conversion. Make sure the frame is in the correct color format before.
///
/// This allocates the full frame into a vec, which we need anyway to cross the thread boundary.
pub(crate) fn ffmpeg_frame_to_image(frame: &FfmpegFrame) -> image::RgbaImage {
    let width = frame.width();
    let height = frame.height();
    let bytes_per_pixel = 4usize; // BGRA
    let src = frame.data(0);
    // ffmpeg frames may have padding at end of each line; copy row-by-row.
    let stride = frame.stride(0);
    let row_bytes = (width as usize) * bytes_per_pixel;
    let mut out = vec![0u8; row_bytes * (height as usize)];
    for y in 0..(height as usize) {
        let src_off = y * stride;
        let dst_off = y * row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    RgbaImage::from_raw(width, height, out).expect("valid image buffer")
}

impl PixelFormat {
    pub fn to_ffmpeg(&self) -> Pixel {
        match self {
            Self::Rgba => Pixel::RGBA,
            Self::Bgra => Pixel::BGRA,
        }
    }

    pub fn from_ffmpeg(value: Pixel) -> Option<Self> {
        match value {
            Pixel::RGBA => Some(Self::Rgba),
            Pixel::BGRA => Some(Self::Bgra),
            _ => None,
        }
    }
}

/// Convert the ffmpeg frame into a [`VideoFrame`]
///
/// Returns `None` if the frame has an unsupported pixel format.
///
/// This allocates the full frame into a vec, which we need anyway to cross the thread boundary.
pub(crate) fn ffmpeg_frame_to_video_frame(frame: &FfmpegFrame) -> Option<VideoFrame> {
    let pixel_format = PixelFormat::from_ffmpeg(frame.format())?;
    let width = frame.width();
    let height = frame.height();
    let bytes_per_pixel = 4usize; // RGBA/BGRA
    let src = frame.data(0);
    // ffmpeg frames may have padding at end of each line; copy row-by-row.
    let stride = frame.stride(0);
    let row_bytes = (width as usize) * bytes_per_pixel;
    let mut out = BytesMut::with_capacity(row_bytes * (height as usize));
    // let mut out = vec![0u8; row_bytes * (height as usize)];
    for y in 0..(height as usize) {
        let src_off = y * stride;
        // let dst_off = y * row_bytes;
        out.put(&src[src_off..src_off + row_bytes]);
        // out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    Some(VideoFrame {
        format: VideoFormat {
            dimensions: [width, height],
            pixel_format,
        },
        raw: out.freeze(),
    })
}
