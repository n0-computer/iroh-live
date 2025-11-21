use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ffmpeg_next::{
    self as ffmpeg, codec::Id, error::Error, format::Pixel, frame::Video as FfmpegVideoFrame,
    packet::Packet,
};

use anyhow::{Context, Result};
use iroh_moq::av::{PixelFormat, VideoFormat, VideoFrame, VideoSource};
use nokhwa::nokhwa_initialize;
use tracing::{debug, info};

use crate::ffmpeg::util::{Rescaler, rgba_bytes_from_frame};

#[derive(Clone)]
pub struct CameraCapturer {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    camera: nokhwa::Camera,
    width: u32,
    height: u32,
    current: Option<VideoFrame>,
    last: Option<Instant>,
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
            inner: Arc::new(Mutex::new(Inner {
                camera,
                width: resolution.width(),
                height: resolution.height(),
                current: None,
                last: None,
            })), // decoder: MjpgDecoder::new()?,
                 // rescaler: Rescaler::new(ffmpeg::format::Pixel::RGBA, None)?,
        })
    }
}

impl Inner {
    fn frame_interval(&self) -> Duration {
        Duration::from_secs(1) / self.camera.frame_rate()
    }

    pub fn current(&mut self) -> Result<VideoFrame> {
        let now = Instant::now();

        match self.current.as_ref() {
            None => {
                let frame = self.capture()?;
                self.current = Some(frame.clone());
                self.last = Some(now);
                Ok(frame)
            }
            Some(last) => {
                let take_new = match self.last {
                    None => true,
                    Some(last) if now.duration_since(last) > self.frame_interval() => true,
                    _ => false,
                };
                if take_new {
                    let frame = self.capture()?;
                    self.current = Some(frame.clone());
                    self.last = Some(now);
                    Ok(frame)
                } else {
                    Ok(last.clone())
                }
            }
        }
    }

    fn capture(&mut self) -> Result<VideoFrame> {
        // let start = Instant::now();
        let frame = self
            .camera
            .frame()
            .context("Failed to capture camera frame")?;
        // let t_capture = start.elapsed();
        // TODO: Don't decode here but in ffmpeg?
        // let image = frame
        //     .decode_image::<nokhwa::pixel_format::RgbAFormat>()
        //     .context("Failed to decode camera frame")?;
        // let mut frame = VideoFrame::new(Pixel::RGBA, image.width(), image.height());
        // frame.data_mut(0).copy_from_slice(&image.as_raw());

        // let frame = self.decoder.decode_frame(&frame.buffer())?;
        // // Ensure RGBA for downstream consumers
        // let frame = self.rescaler.process(&frame)?.clone();

        // println!("camera frame {t_capture:?} {t_decode:?}",);
        // TODO: don't clone
        let raw = frame.buffer().to_vec();
        let frame = VideoFrame { raw };
        Ok(frame)
    }
}

impl VideoSource for CameraCapturer {
    fn format(&self) -> VideoFormat {
        let inner = self.inner.lock().expect("poisoned");
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [inner.width, inner.height],
        }
    }

    fn current_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        let frame = self.inner.lock().expect("poisoned").current()?; // converted to RGBA via rescaler
        Ok(Some(frame))
        // let raw = rgba_bytes_from_frame(&frame);
        // Ok(Some(VideoFrame { raw }))
    }
}

// pub struct MjpgDecoder {
//     pub(crate) dec: ffmpeg::decoder::Video, // <- video decoder, not codec::Context
// }

// impl MjpgDecoder {
//     /// Initialize FFmpeg and create a Video decoder for MJPEG.
//     pub fn new() -> Result<Self, Error> {
//         ffmpeg::init()?;

//         // Find the MJPEG decoder and create a context bound to it.
//         let mjpeg = ffmpeg::decoder::find(Id::MJPEG).ok_or(Error::DecoderNotFound)?;

//         // Create a codec::Context that's pre-bound to this decoder codec,
//         // then get a video decoder out of it.
//         let ctx = ffmpeg::codec::context::Context::new_with_codec(mjpeg);
//         let dec = ctx.decoder().video()?; // has send_packet/receive_frame

//         Ok(Self { dec })
//     }

//     /// Decode one complete MJPEG/JPEG frame from `mjpg_frame`.
//     pub fn decode_frame(&mut self, mjpg_frame: &[u8]) -> Result<FfmpegVideoFrame, Error> {
//         // Make a packet that borrows/copies the data.
//         let packet = Packet::borrow(mjpg_frame);
//         // Feed & drain once — MJPEG is intra-only (one picture per packet).
//         self.dec.send_packet(&packet)?;
//         let mut frame = FfmpegVideoFrame::empty();
//         self.dec.receive_frame(&mut frame)?;

//         // MJPEG may output deprecated YUVJ* formats. Replace them with
//         // the non-deprecated equivalents and mark full range to keep semantics.
//         // This avoids ffmpeg warning: "deprecated pixel format used, make sure you did set range correctly".
//         use ffmpeg_next::util::color::Range;
//         match frame.format() {
//             Pixel::YUVJ420P => {
//                 frame.set_color_range(Range::JPEG);
//                 frame.set_format(Pixel::YUV420P);
//             }
//             Pixel::YUVJ422P => {
//                 frame.set_color_range(Range::JPEG);
//                 frame.set_format(Pixel::YUV422P);
//             }
//             Pixel::YUVJ444P => {
//                 frame.set_color_range(Range::JPEG);
//                 frame.set_format(Pixel::YUV444P);
//             }
//             _ => {}
//         }
//         Ok(frame)
//     }
// }
