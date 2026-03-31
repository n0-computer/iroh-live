//! macOS native camera capture is not yet functional. Use the nokhwa backend instead.
//!
//! The AVFoundation camera implementation is stubbed out because the objc2
//! delegate pattern for `AVCaptureVideoDataOutputSampleBufferDelegate` has
//! not been completed. The struct and trait impls are preserved for future
//! work, but all methods return errors or empty results.
//!
//! On macOS, enable the `nokhwa` feature for working camera capture.

use anyhow::Result;
use rusty_codecs::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};

use crate::types::{CameraConfig, CameraInfo};

/// Lists available cameras via AVFoundation.
///
/// Returns an empty list because the native backend is not functional.
/// Use the nokhwa backend for camera enumeration on macOS.
pub fn cameras() -> Result<Vec<CameraInfo>> {
    Ok(vec![])
}

/// AVFoundation camera capturer for macOS and iOS.
///
/// Not yet functional. All constructors return an error directing the
/// caller to the nokhwa backend.
#[derive(derive_more::Debug)]
pub struct AppleCameraCapturer {
    width: u32,
    height: u32,
}

impl AppleCameraCapturer {
    /// Attempts to create a camera capturer for the given device.
    ///
    /// Always returns an error because the native backend is not implemented.
    pub fn new(_info: &CameraInfo, _config: &CameraConfig) -> Result<Self> {
        anyhow::bail!(
            "AVFoundation camera capture is not yet implemented; \
             enable the `nokhwa` feature for camera support on macOS"
        )
    }

    /// Attempts to open the default camera.
    ///
    /// Always returns an error because the native backend is not implemented.
    pub fn open_default() -> Result<Self> {
        anyhow::bail!(
            "AVFoundation camera capture is not yet implemented; \
             enable the `nokhwa` feature for camera support on macOS"
        )
    }
}

impl VideoSource for AppleCameraCapturer {
    fn name(&self) -> &str {
        "apple-camera"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        anyhow::bail!("AVFoundation camera capture is not yet implemented")
    }

    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        anyhow::bail!("AVFoundation camera capture is not yet implemented")
    }
}
