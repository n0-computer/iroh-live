use anyhow::{Context, Result};
use ffmpeg_next::{
    self as ffmpeg, Error, Packet, codec::Id, format::Pixel, frame::Video as FfmpegVideoFrame,
};
use iroh_moq::av::{PixelFormat, VideoFormat, VideoFrame, VideoSource};
use nokhwa::nokhwa_initialize;
use tracing::{debug, info};
use xcap::{Monitor, VideoRecorder};

use crate::ffmpeg::util::Rescaler;

pub struct ScreenCapturer {
    pub(crate) _monitor: Monitor,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) video_recorder: VideoRecorder,
    pub(crate) rx: std::sync::mpsc::Receiver<xcap::Frame>,
}

unsafe impl Send for ScreenCapturer {}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        self.video_recorder.stop().ok();
    }
}

impl ScreenCapturer {
    pub fn new() -> Result<Self> {
        info!("Initializing screen capturer with xcap");

        let monitors = Monitor::all().context("Failed to get monitors")?;
        if monitors.is_empty() {
            return Err(anyhow::anyhow!("No monitors available"));
        }
        debug!("Monitors: {monitors:?}");

        let monitor = monitors.into_iter().next().unwrap();
        let width = monitor.width()?;
        let height = monitor.height()?;
        let name = monitor
            .name()
            .unwrap_or_else(|_| "Unknown Monitor".to_string());

        info!("Using primary monitor: {} ({}x{})", name, width, height);

        let (video_recorder, rx) = monitor.video_recorder()?;
        video_recorder.start()?;

        Ok(Self {
            _monitor: monitor,
            video_recorder,
            rx,
            width,
            height,
        })
    }

    pub fn capture(&mut self) -> Result<FfmpegVideoFrame> {
        let mut raw_frame = None;
        // We are only interested in the latest frame.
        // Drain the channel to not build up memory.
        while let Ok(next) = self.rx.try_recv() {
            raw_frame = Some(next)
        }
        let raw_frame = match raw_frame {
            Some(frame) => frame,
            None => self
                .rx
                .recv()
                .context("video recorder did not produce new frame")?,
        };
        let mut frame = FfmpegVideoFrame::new(Pixel::RGBA, raw_frame.width, raw_frame.height);
        frame.data_mut(0).copy_from_slice(&raw_frame.raw);
        Ok(frame)
    }
}

pub struct CameraCapturer {
    pub(crate) camera: nokhwa::Camera,
    pub(crate) width: u32,
    pub(crate) height: u32,
    // decoder: MjpgDecoder,
    rescaler: Rescaler,
}

impl CameraCapturer {
    pub fn new() -> Result<Self> {
        info!("Initializing camera capturer");
        nokhwa_initialize(|granted| {
            debug!("User said {}", granted);
        });

        let cameras = nokhwa::query(nokhwa::utils::ApiBackend::Auto)?;
        if cameras.is_empty() {
            return Err(anyhow::anyhow!("No cameras available"));
        }
        debug!("cameras: {cameras:?}");

        let first_camera = cameras.last().unwrap();
        info!("Using camera: {}", first_camera.human_name());

        let mut camera = nokhwa::Camera::new(
            first_camera.index().clone(),
            nokhwa::utils::RequestedFormat::new::<nokhwa::pixel_format::RgbFormat>(
                nokhwa::utils::RequestedFormatType::AbsoluteHighestFrameRate,
            ),
        )?;

        camera.open_stream()?;

        // Get actual resolution
        let resolution = camera.resolution();
        info!(
            "Camera resolution: {}x{}",
            resolution.width(),
            resolution.height()
        );

        Ok(Self {
            camera,
            width: resolution.width(),
            height: resolution.height(),
            // decoder: MjpgDecoder::new()?,
            rescaler: Rescaler::new(ffmpeg::format::Pixel::RGBA, None)?,
        })
    }

    pub fn capture(&mut self) -> Result<FfmpegVideoFrame> {
        // let start = Instant::now();
        let frame = self
            .camera
            .frame()
            .context("Failed to capture camera frame")?;
        // let t_capture = start.elapsed();
        // TODO: Don't decode here but in ffmpeg?
        let image = frame
            .decode_image::<nokhwa::pixel_format::RgbAFormat>()
            .context("Failed to decode camera frame")?;
        let mut frame = FfmpegVideoFrame::new(Pixel::RGBA, image.width(), image.height());
        frame.data_mut(0).copy_from_slice(&image.as_raw());
        // let frame = self.decoder.decode_frame(&frame.buffer())?;
        // Ensure RGBA for downstream consumers
        let frame = self.rescaler.process(&frame)?.clone();
        // let t_decode = start.elapsed() - t_capture;

        // println!("camera frame {t_capture:?} {t_decode:?}",);
        Ok(frame)
    }
}

pub struct MjpgDecoder {
    dec: ffmpeg::decoder::Video, // <- video decoder, not codec::Context
}

impl MjpgDecoder {
    /// Initialize FFmpeg and create a Video decoder for MJPEG.
    pub fn new() -> Result<Self, Error> {
        ffmpeg::init()?;

        // Find the MJPEG decoder and create a context bound to it.
        let mjpeg = ffmpeg::decoder::find(Id::MJPEG).ok_or(Error::DecoderNotFound)?;

        // Create a codec::Context that's pre-bound to this decoder codec,
        // then get a video decoder out of it.
        let ctx = ffmpeg::codec::context::Context::new_with_codec(mjpeg);
        let dec = ctx.decoder().video()?; // has send_packet/receive_frame

        Ok(Self { dec })
    }

    /// Decode one complete MJPEG/JPEG frame from `mjpg_frame`.
    pub fn decode_frame(&mut self, mjpg_frame: &[u8]) -> Result<FfmpegVideoFrame, Error> {
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
        Ok(frame)
    }
}

// Copy plane 0 honoring stride into a tightly packed RGBA buffer
fn rgba_bytes_from_frame(frame: &ffmpeg_next::frame::Video) -> Vec<u8> {
    let width = frame.width();
    let height = frame.height();
    let stride = frame.stride(0) as usize;
    let row_bytes = (width as usize) * 4;
    let src = frame.data(0);
    let mut out = vec![0u8; row_bytes * (height as usize)];
    for y in 0..(height as usize) {
        let src_off = y * stride;
        let dst_off = y * row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    out
}

impl VideoSource for ScreenCapturer {
    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        let frame = self.capture()?; // already RGBA
        let raw = rgba_bytes_from_frame(&frame);
        Ok(Some(VideoFrame { raw }))
    }
}

impl VideoSource for CameraCapturer {
    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        let frame = self.capture()?; // converted to RGBA via rescaler
        let raw = rgba_bytes_from_frame(&frame);
        Ok(Some(VideoFrame { raw }))
    }
}
