use std::time::Duration;

use anyhow::Result;
use ffmpeg_next::{
    self as ffmpeg,
    software::scaling::{self, Flags},
    util::{format::pixel::Pixel, frame::video::Video as FfmpegFrame},
};
use image::RgbaImage;

use crate::av::{PixelFormat, VideoFormat, VideoFrame};

pub(crate) mod mjpg_decoder;

pub(crate) struct Rescaler {
    pub(crate) target_format: Pixel,
    pub(crate) target_width_height: Option<(u32, u32)>,
    pub(crate) ctx: Option<scaling::Context>,
    pub(crate) out_frame: FfmpegFrame,
}

// I think the ffmpeg structs are send-safe.
// We want to create the encoder before moving it to a thread.
unsafe impl Send for Rescaler {}

impl Rescaler {
    pub fn new(target_format: Pixel, target_width_height: Option<(u32, u32)>) -> Result<Self> {
        Ok(Self {
            target_format,
            ctx: None,
            target_width_height,
            out_frame: FfmpegFrame::empty(),
        })
    }

    pub fn process(&mut self, frame: &FfmpegFrame) -> Result<&FfmpegFrame, ffmpeg::Error> {
        let (target_width, target_height) = self
            .target_width_height
            .unwrap_or_else(|| (frame.width(), frame.height()));
        let out_frame_needs_reset = self.out_frame.width() != target_width
            || self.out_frame.height() != target_height
            || self.out_frame.format() != self.target_format;
        if out_frame_needs_reset {
            self.out_frame = FfmpegFrame::new(self.target_format, target_width, target_height);
        }
        let ctx = match self.ctx {
            None => self.ctx.insert(scaling::Context::get(
                frame.format(),
                frame.width(),
                frame.height(),
                self.out_frame.format(),
                self.out_frame.width(),
                self.out_frame.height(),
                Flags::BILINEAR,
            )?),
            Some(ref mut ctx) => ctx,
        };
        // This resets the contxt if any parameters changed.
        ctx.cached(
            frame.format(),
            frame.width(),
            frame.height(),
            self.out_frame.format(),
            self.out_frame.width(),
            self.out_frame.height(),
            Flags::BILINEAR,
        );

        ctx.run(&frame, &mut self.out_frame)?;
        Ok(&self.out_frame)
    }
}

#[derive(Default, Debug)]
pub(crate) struct StreamClock {
    pub(crate) last_timestamp: Option<Duration>,
}

impl StreamClock {
    pub(crate) fn frame_delay(&mut self, encoded_frame: &hang::Frame) -> Duration {
        // Compute interframe delay from provided timestamps
        let delay = match self.last_timestamp {
            None => Duration::default(),
            Some(last_timestamp) => encoded_frame.timestamp.saturating_sub(last_timestamp),
        };
        self.last_timestamp = Some(encoded_frame.timestamp);
        delay
    }
}

/// Convert the ffmpeg frame into an [image] frame.
///
/// Note: This does not do any color conversion. Make sure the frame is in the correct color format before.
///
/// This allocates the full frame into a vec, which we need anyway to cross the thread boundary.
pub(crate) fn ffmpeg_frame_to_image(frame: &ffmpeg_next::util::frame::Video) -> image::RgbaImage {
    let width = frame.width();
    let height = frame.height();
    let bytes_per_pixel = 4usize; // BGRA
    let src = frame.data(0);
    // ffmpeg frames may have padding at end of each line; copy row-by-row.
    let stride = frame.stride(0) as usize;
    let row_bytes = (width as usize) * bytes_per_pixel;
    let mut out = vec![0u8; row_bytes * (height as usize)];
    for y in 0..(height as usize) {
        let src_off = y * stride;
        let dst_off = y * row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    RgbaImage::from_raw(width, height, out).expect("valid image buffer")
}

pub(crate) fn pixel_to_ffmpeg(value: PixelFormat) -> Pixel {
    match value {
        PixelFormat::Rgba => Pixel::RGBA,
        PixelFormat::Bgra => Pixel::BGRA,
    }
}

pub(crate) fn ffmpeg_to_pixel(value: Pixel) -> Option<PixelFormat> {
    match value {
        Pixel::RGBA => Some(PixelFormat::Rgba),
        Pixel::BGRA => Some(PixelFormat::Bgra),
        _ => None,
    }
}

/// Convert the ffmpeg frame into a [`VideoFrame`]
///
/// Returns `None` if the frame has an unsupported pixel format.
///
/// This allocates the full frame into a vec, which we need anyway to cross the thread boundary.
pub(crate) fn ffmpeg_frame_to_video_frame(
    frame: &ffmpeg_next::util::frame::Video,
) -> Option<VideoFrame> {
    let pixel_format = ffmpeg_to_pixel(frame.format())?;
    let width = frame.width();
    let height = frame.height();
    let bytes_per_pixel = 4usize; // RGBA/BGRA
    let src = frame.data(0);
    // ffmpeg frames may have padding at end of each line; copy row-by-row.
    let stride = frame.stride(0) as usize;
    let row_bytes = (width as usize) * bytes_per_pixel;
    let mut out = vec![0u8; row_bytes * (height as usize)];
    for y in 0..(height as usize) {
        let src_off = y * stride;
        let dst_off = y * row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    Some(VideoFrame {
        format: VideoFormat {
            dimensions: [width, height],
            pixel_format,
        },
        raw: out,
    })
}
