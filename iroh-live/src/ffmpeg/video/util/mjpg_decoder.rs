use ffmpeg_next::{
    self as ffmpeg, Error, Packet, codec::Id, format::Pixel, frame::Video as FfmpegVideoFrame,
};

use crate::{
    av::VideoFrame,
    ffmpeg::util::{Rescaler, ffmpeg_frame_to_video_frame},
};

pub struct MjpgDecoder {
    dec: ffmpeg::decoder::Video,
    rescaler: Rescaler,
}

impl MjpgDecoder {
    /// Initialize FFmpeg and create a Video decoder for MJPEG.
    pub fn new() -> anyhow::Result<Self> {
        ffmpeg::init()?;

        // Find the MJPEG decoder and create a context bound to it.
        let mjpeg = ffmpeg::decoder::find(Id::MJPEG).ok_or(Error::DecoderNotFound)?;

        // Create a codec::Context that's pre-bound to this decoder codec,
        // then get a video decoder out of it.
        let ctx = ffmpeg::codec::context::Context::new_with_codec(mjpeg);
        let dec = ctx.decoder().video()?; // has send_packet/receive_frame

        let rescaler = Rescaler::new(Pixel::RGBA, None)?;

        Ok(Self { dec, rescaler })
    }

    /// Decode one complete MJPEG/JPEG frame from `mjpg_frame`.
    pub fn decode_frame(&mut self, mjpg_frame: &[u8]) -> Result<VideoFrame, Error> {
        // Make a packet that borrows/copies the data.
        let packet = Packet::borrow(mjpg_frame);
        // Feed & drain once — MJPEG is intra-only (one picture per packet).
        self.dec.send_packet(&packet)?;
        let mut frame = FfmpegVideoFrame::empty();
        self.dec.receive_frame(&mut frame)?;

        // MJPEG may output deprecated YUVJ* formats. Replace them with
        // the non-deprecated equivalents and mark full range to keep semantics.
        // This avoids ffmpeg warning: "deprecated pixel format used, make sure you did set range correctly".
        use ffmpeg_next::util::color::Range;
        match frame.format() {
            Pixel::YUVJ420P => {
                frame.set_color_range(Range::JPEG);
                frame.set_format(Pixel::YUV420P);
            }
            Pixel::YUVJ422P => {
                frame.set_color_range(Range::JPEG);
                frame.set_format(Pixel::YUV422P);
            }
            Pixel::YUVJ444P => {
                frame.set_color_range(Range::JPEG);
                frame.set_format(Pixel::YUV444P);
            }
            _ => {}
        }
        let frame = self.rescaler.process(&frame)?;
        let frame = ffmpeg_frame_to_video_frame(frame).expect("valid pixel format set in rescaler");
        Ok(frame)
    }
}
