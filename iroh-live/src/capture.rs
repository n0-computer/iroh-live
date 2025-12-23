use std::str::FromStr;

use anyhow::{Context, Result};
use nokhwa::{
    nokhwa_initialize,
    pixel_format::RgbFormat,
    utils::{CameraFormat, FrameFormat, RequestedFormat, RequestedFormatType, Resolution},
};
use tracing::{debug, info, trace, warn};
use xcap::{Monitor, VideoRecorder};

use crate::{
    av::{PixelFormat, VideoFormat, VideoFrame, VideoSource},
    ffmpeg::util::MjpgDecoder,
};

pub use nokhwa::utils::{CameraIndex, CameraInfo};

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
    fn name(&self) -> &str {
        "screen"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        self.video_recorder.start()?;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.video_recorder.stop()?;
        Ok(())
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
            raw: raw_frame.raw.into(),
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
        let cameras = Self::list()?;
        if cameras.is_empty() {
            return Err(anyhow::anyhow!("No cameras available"));
        }
        info!("Available cameras: {cameras:#?}");

        let camera_index = match std::env::var("IROH_LIVE_CAMERA").ok() {
            None => {
                // Order of cameras in nokhwa is reversed from usual order (primary camera is last).
                let first_camera = cameras.last().unwrap();
                info!(": {}", first_camera.human_name());
                first_camera.index().clone()
            }
            Some(camera_name) => match u32::from_str(&camera_name).ok() {
                Some(num) => CameraIndex::Index(num),
                None => CameraIndex::String(camera_name),
            },
        };
        Self::new_with(camera_index)
    }

    pub fn list() -> Result<Vec<CameraInfo>> {
        info!("Initializing camera capturer (nokhwa)");
        nokhwa_initialize(|granted| {
            debug!("User selected camera access: {}", granted);
        });

        let cameras = nokhwa::query(nokhwa::utils::ApiBackend::Auto)?;
        Ok(cameras)
    }

    pub fn new_with(camera_index: CameraIndex) -> Result<Self> {
        let mut camera = nokhwa::Camera::new(
            camera_index,
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution),
        )?;
        info!("Using camera: {}", camera.info().human_name());
        let available_formats = camera.compatible_camera_formats()?;
        debug!("Available formats: {available_formats:?}",);
        if let Some(format) = Self::select_format(available_formats, Resolution::new(1920, 1080)) {
            if let Err(err) = camera.set_camera_requset(RequestedFormat::new::<RgbFormat>(
                RequestedFormatType::Exact(format),
            )) {
                warn!(?format, "Failed to change camera format: {err:#}");
            }
        }
        info!("Using format: {}", camera.camera_format());
        let resolution = camera.resolution();
        Ok(Self {
            camera,
            mjpg_decoder: MjpgDecoder::new()?,
            width: resolution.width(),
            height: resolution.height(),
        })
    }

    fn select_format(
        mut formats: Vec<CameraFormat>,
        desired_resolution: Resolution,
    ) -> Option<CameraFormat> {
        formats.sort_by(|a, b| {
            a.resolution()
                .cmp(&b.resolution())
                .then(a.frame_rate().cmp(&b.frame_rate()))
        });
        formats
            .iter()
            .find(|format| format.resolution() >= desired_resolution)
            .or_else(|| formats.last())
            .cloned()
    }
}

impl VideoSource for CameraCapturer {
    fn name(&self) -> &str {
        "cam"
    }
    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        self.camera.open_stream()?;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.camera.stop_stream()?;
        Ok(())
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        let start = std::time::Instant::now();
        let frame = self
            .camera
            .frame()
            .context("Failed to capture camera frame")?;
        trace!("pop frame: capture took {:?}", start.elapsed());
        let start = std::time::Instant::now();
        let frame = match frame.source_frame_format() {
            FrameFormat::MJPEG if std::env::var("IROH_LIVE_MJPEG_FFMPEG").is_ok() => {
                trace!("decode ffmpeg");
                self.mjpg_decoder.decode_frame(frame.buffer())?
            }
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
        trace!("pop frame: decode took {:?}", start.elapsed());
        Ok(Some(frame))
    }
}
