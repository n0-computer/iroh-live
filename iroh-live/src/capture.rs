use anyhow::{Context, Result};
use nokhwa::nokhwa_initialize;
use tracing::{debug, info};
use xcap::{Monitor, VideoRecorder};

use crate::av::{PixelFormat, VideoFormat, VideoFrame, VideoSource};

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
                .context("video recorder did not produce new frame")?,
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
    pub(crate) width: u32,
    pub(crate) height: u32,
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
        let image = frame
            .decode_image::<nokhwa::pixel_format::RgbAFormat>()
            .context("Failed to decode camera frame")?;
        Ok(Some(VideoFrame {
            format: self.format(),
            raw: image.into_raw().into(),
        }))
    }
}
