use anyhow::{Context, Result};
use nokhwa::{nokhwa_initialize, utils::FrameFormat};
use tracing::{debug, info};
use xcap::{Monitor, VideoRecorder};

use crate::{
    av::{PixelFormat, VideoFormat, VideoFrame, VideoSource},
    ffmpeg::util::mjpg_decoder::MjpgDecoder,
};

pub struct ScreenCapturer {
    pub(crate) _monitor: Monitor,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) video_recorder: VideoRecorder,
    pub(crate) rx: std::sync::mpsc::Receiver<xcap::Frame>,
}

// TODO: Review if sound.
unsafe impl Send for ScreenCapturer {}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        self.video_recorder.stop().ok();
    }
}

impl ScreenCapturer {
    pub fn new() -> Result<Self> {
        info!("Initializing screen capturer (xcap)");

        let monitors = Monitor::all().context("Failed to get monitors")?;
        if monitors.is_empty() {
            return Err(anyhow::anyhow!("No monitors available"));
        }
        info!("Available monitors: {monitors:?}");

        let monitor = monitors.into_iter().next().unwrap();
        let width = monitor.width()?;
        let height = monitor.height()?;
        let name = monitor
            .name()
            .unwrap_or_else(|_| "Unknown Monitor".to_string());

        info!("Using monitor: {} ({}x{})", name, width, height);

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
}

impl VideoSource for ScreenCapturer {
    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
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
                .context("Screen recorder did not produce new frame")?,
        };
        Ok(Some(VideoFrame {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [raw_frame.width, raw_frame.height],
            },
            raw: raw_frame.raw,
        }))
    }
}

pub struct CameraCapturer {
    pub(crate) camera: nokhwa::Camera,
    pub(crate) mjpg_decoder: MjpgDecoder,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl CameraCapturer {
    pub fn new() -> Result<Self> {
        info!("Initializing camera capturer (nokhwa)");
        nokhwa_initialize(|granted| {
            debug!("User selected camera access: {}", granted);
        });

        let cameras = nokhwa::query(nokhwa::utils::ApiBackend::Auto)?;
        if cameras.is_empty() {
            return Err(anyhow::anyhow!("No cameras available"));
        }
        info!("Available cameras: {cameras:?}");

        let first_camera = cameras.last().unwrap();
        info!("Using camera: {}", first_camera.human_name());

        let mut camera = nokhwa::Camera::new(
            first_camera.index().clone(),
            nokhwa::utils::RequestedFormat::new::<nokhwa::pixel_format::RgbFormat>(
                nokhwa::utils::RequestedFormatType::AbsoluteHighestFrameRate,
            ),
        )?;

        camera.open_stream()?;
        info!(
            "Available formats: {:#?}",
            camera.compatible_camera_formats()
        );

        info!("Using format: {:?}", camera.camera_format());

        let resolution = camera.resolution();

        Ok(Self {
            camera,
            mjpg_decoder: MjpgDecoder::new()?,
            width: resolution.width(),
            height: resolution.height(),
        })
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
        let frame = self
            .camera
            .frame()
            .context("Failed to capture camera frame")?;
        let frame = match frame.source_frame_format() {
            FrameFormat::MJPEG => self.mjpg_decoder.decode_frame(frame.buffer())?,
            _ => {
                let image = frame
                    .decode_image::<nokhwa::pixel_format::RgbAFormat>()
                    .context("Failed to decode camera frame")?;
                VideoFrame {
                    format: self.format(),
                    raw: image.into_raw().into(),
                }
            }
        };
        Ok(Some(frame))
    }
}
