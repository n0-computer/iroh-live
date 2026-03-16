//! Push-based video source for Android camera frames.
//!
//! Android cameras deliver frames via callbacks (CameraX ImageAnalysis or
//! Camera2 ImageReader). This module bridges that push model to moq-media's
//! pull-based [`VideoSource`] trait by buffering the latest frame and
//! returning it on the next `pop_frame()` call.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use moq_media::{
    format::{PixelFormat, VideoFormat, VideoFrame},
    traits::VideoSource,
};

/// A [`VideoSource`] that receives camera frames pushed from the app side.
///
/// Holds at most one frame — newer frames replace older ones that haven't
/// been consumed yet. This matches the real-time nature of camera capture
/// where only the latest frame matters.
#[derive(Debug)]
pub struct CameraFrameSource {
    pending_frame: Option<VideoFrame>,
    format: VideoFormat,
    /// Whether the encode pipeline has called `start()`.
    pub started: bool,
}

impl CameraFrameSource {
    /// Creates a camera source for the given dimensions.
    ///
    /// The `pixel_format` in the returned [`VideoFormat`] is set to RGBA
    /// as a default. Encoders that receive `FrameData::Nv12` frames bypass
    /// the pixel format field entirely — it only matters for packed formats.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pending_frame: None,
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [width, height],
            },
            started: false,
        }
    }

    /// Pushes a camera frame, replacing any unconsumed previous frame.
    pub fn push_frame(&mut self, frame: VideoFrame) {
        self.pending_frame = Some(frame);
    }
}

impl VideoSource for CameraFrameSource {
    fn name(&self) -> &str {
        "android-camera"
    }

    fn format(&self) -> VideoFormat {
        self.format.clone()
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        Ok(self.pending_frame.take())
    }

    fn start(&mut self) -> Result<()> {
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.started = false;
        Ok(())
    }
}

/// [`VideoSource`] wrapper around `Arc<Mutex<CameraFrameSource>>`.
///
/// The publish pipeline takes ownership of the source, but the JNI camera
/// callback needs a shared handle to push frames. This wrapper bridges the
/// two by delegating all trait methods through the mutex.
#[derive(Debug)]
pub struct SharedCameraSource {
    /// Shared reference to the underlying camera source.
    pub inner: Arc<Mutex<CameraFrameSource>>,
}

impl VideoSource for SharedCameraSource {
    fn name(&self) -> &str {
        "android-camera"
    }

    fn format(&self) -> VideoFormat {
        self.inner.lock().expect("poisoned").format.clone()
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        Ok(self.inner.lock().expect("poisoned").pending_frame.take())
    }

    fn start(&mut self) -> Result<()> {
        self.inner.lock().expect("poisoned").started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.inner.lock().expect("poisoned").started = false;
        Ok(())
    }
}
