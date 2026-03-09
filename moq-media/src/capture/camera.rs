use std::{env, str::FromStr, time::Instant};

use anyhow::{Context, Result};
use nokhwa::{
    nokhwa_initialize,
    pixel_format::{RgbAFormat, RgbFormat},
    query,
    utils::{
        ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
        Resolution,
    },
};
use tracing::{debug, info, trace, warn};

use crate::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    processing::mjpg::MjpgDecoder,
    traits::VideoSource,
};

/// Information about an available camera.
#[derive(Debug, Clone)]
pub struct CameraInfo {
    /// Numeric index of this camera.
    pub index: u32,
    /// Human-readable name of this camera.
    pub name: String,
}

/// List all available cameras.
pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    nokhwa_initialize(|granted| {
        debug!("User selected camera access: {}", granted);
    });
    let cameras = query(ApiBackend::Auto)?;
    Ok(cameras
        .iter()
        .enumerate()
        .map(|(i, info)| CameraInfo {
            index: i as u32,
            name: info.human_name().to_string(),
        })
        .collect())
}

#[derive(derive_more::Debug)]
pub struct CameraCapturer {
    #[debug(skip)]
    pub(crate) camera: nokhwa::Camera,
    #[debug(skip)]
    pub(crate) mjpg_decoder: MjpgDecoder,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl CameraCapturer {
    /// Create a new camera capturer using the default camera selection.
    ///
    /// Uses the `IROH_LIVE_CAMERA` environment variable if set, otherwise picks the last camera.
    pub fn new() -> Result<Self> {
        info!("Initializing camera capturer (nokhwa)");
        nokhwa_initialize(|granted| {
            debug!("User selected camera access: {}", granted);
        });

        let cameras = query(ApiBackend::Auto)?;
        if cameras.is_empty() {
            return Err(anyhow::anyhow!("No cameras available"));
        }
        info!("Available cameras: {cameras:?}");

        let camera_index = match env::var("IROH_LIVE_CAMERA").ok() {
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
        Self::open(camera_index)
    }

    /// Create a new camera capturer for a specific camera index.
    pub fn with_index(index: u32) -> Result<Self> {
        info!("Initializing camera capturer (nokhwa) with index {index}");
        nokhwa_initialize(|granted| {
            debug!("User selected camera access: {}", granted);
        });
        Self::open(CameraIndex::Index(index))
    }

    fn open(camera_index: CameraIndex) -> Result<Self> {
        let mut camera = nokhwa::Camera::new(
            camera_index,
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution),
        )?;
        info!("Using camera: {}", camera.info().human_name());
        let available_formats = camera.compatible_camera_formats()?;
        debug!("Available formats: {available_formats:?}",);
        if let Some(format) = Self::select_format(available_formats, Resolution::new(1920, 1080))
            && let Err(err) = camera.set_camera_requset(RequestedFormat::new::<RgbFormat>(
                RequestedFormatType::Exact(format),
            ))
        {
            warn!(?format, "Failed to change camera format: {err:#}");
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
        // Pick the smallest format that meets or exceeds the desired resolution,
        // but don't go above 1920x1080 to avoid exceeding encoder limits.
        let max_resolution = Resolution::new(1920, 1080);
        formats
            .iter()
            .find(|format| {
                format.resolution() >= desired_resolution && format.resolution() <= max_resolution
            })
            // Fall back to the largest format at or below the max.
            .or_else(|| {
                formats
                    .iter()
                    .rev()
                    .find(|format| format.resolution() <= max_resolution)
            })
            // Last resort: smallest available format.
            .or_else(|| formats.first())
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
        let start = Instant::now();
        let frame = self
            .camera
            .frame()
            .context("Failed to capture camera frame")?;
        trace!("pop frame: capture took {:?}", start.elapsed());
        let start = Instant::now();
        let frame = match frame.source_frame_format() {
            FrameFormat::MJPEG => {
                trace!("decode mjpeg");
                self.mjpg_decoder.decode_frame(frame.buffer())?
            }
            _ => {
                let image = frame
                    .decode_image::<RgbAFormat>()
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
