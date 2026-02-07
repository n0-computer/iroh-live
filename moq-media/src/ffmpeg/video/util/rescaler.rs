use anyhow::Result;
use ffmpeg_next::software::scaling::Flags;
use ffmpeg_next::{
    self as ffmpeg,
    software::scaling::{self},
    util::{format::pixel::Pixel, frame::video::Video as FfmpegFrame},
};

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
    pub(crate) fn new(
        target_format: Pixel,
        target_width_height: Option<(u32, u32)>,
    ) -> Result<Self> {
        Ok(Self {
            target_format,
            ctx: None,
            target_width_height,
            out_frame: FfmpegFrame::empty(),
        })
    }

    pub(crate) fn set_target_dimensions(&mut self, w: u32, h: u32) {
        self.target_width_height = Some((w, h));
    }

    pub(crate) fn process<'a: 'b, 'b>(
        &'a mut self,
        frame: &'b FfmpegFrame,
    ) -> Result<&'b FfmpegFrame, ffmpeg::Error> {
        // Short-circuit if possible.
        if self.target_width_height.is_none() && self.target_format == frame.format() {
            return Ok(frame);
        }
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
        // This resets the context if any parameters changed.
        ctx.cached(
            frame.format(),
            frame.width(),
            frame.height(),
            self.out_frame.format(),
            self.out_frame.width(),
            self.out_frame.height(),
            Flags::BILINEAR,
        );

        ctx.run(frame, &mut self.out_frame)?;
        Ok(&self.out_frame)
    }
}
