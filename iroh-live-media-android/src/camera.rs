//! Push-based video source for Android camera frames.
//!
//! Android delivers camera frames through callbacks, from CameraX
//! `ImageAnalysis` or Camera2 `ImageReader`. A broadcast reads a
//! [`VideoSource`]. This module connects the two with
//! [`VideoSource::push`](iroh_live_media::VideoSource::push): the app pushes
//! frames from the callback thread, and the broadcast encodes the newest one.
//!
//! A new frame replaces an older one the encoder has not taken yet. For a
//! camera, the current picture is worth more than a stale one.

use iroh_live_media::{Closed, FrameSender, VideoFormat, VideoSource};
use moq_video::{Frame, Rate, Size, Surface};
use n0_error::stack_error;

/// Errors from pushing a camera frame.
#[stack_error(derive, add_meta)]
pub enum CameraError {
    /// The pixel buffer does not match the declared size and format.
    #[error("invalid camera frame")]
    Frame {
        /// The error from building the surface.
        #[error(source, std_err)]
        source: moq_video::Error,
    },
    /// The source has closed.
    #[error("the camera source has closed")]
    Closed {
        /// The error from the frame sender.
        #[error(source, std_err)]
        source: Closed,
    },
}

/// The app side of a camera bridge.
///
/// Push frames here from the camera callback. Clones are cheap and can move
/// across threads. The [`VideoSource`] ends when every clone is dropped.
#[derive(Debug, Clone)]
pub struct CameraSink {
    frames: FrameSender<Frame>,
    size: Size,
}

impl CameraSink {
    /// Pushes one RGBA frame, replacing any frame not yet taken.
    ///
    /// `rgba` is tightly packed, `width * height * 4` bytes.
    ///
    /// # Errors
    ///
    /// Fails if `rgba` does not match the size this sink was created with, or
    /// if the source has closed.
    pub fn push_rgba(&self, rgba: &[u8], timestamp: moq_net::Timestamp) -> Result<(), CameraError> {
        let surface = Surface::rgba(rgba, self.size)
            .map_err(|source| n0_error::e!(CameraError::Frame { source }))?;
        self.push(Frame::new(surface, timestamp))
    }

    /// Pushes a frame the caller built.
    ///
    /// Use this when the camera can hand over something better than packed
    /// RGBA.
    ///
    /// # Errors
    ///
    /// Fails if the source has closed.
    pub fn push(&self, frame: Frame) -> Result<(), CameraError> {
        self.frames
            .push(frame)
            .map_err(|source| n0_error::e!(CameraError::Closed { source }))
    }

    /// Returns a watcher for whether anything is encoding the frames.
    ///
    /// Use it to stop capturing while nobody watches.
    pub fn demand(&self) -> n0_watcher::Direct<bool> {
        self.frames.demand()
    }

    /// Returns the size every pushed frame must have.
    pub fn size(&self) -> Size {
        self.size
    }
}

/// Creates a camera bridge for a fixed capture size and rate.
///
/// Pass the [`VideoSource`] to
/// [`LocalBroadcast::set_video`](iroh_live_media::LocalBroadcast::set_video)
/// and the [`CameraSink`] to the code that drives the camera.
pub fn camera(size: Size, rate: Rate) -> (CameraSink, VideoSource) {
    let (frames, source) = VideoSource::push(VideoFormat { size, rate });
    (CameraSink { frames, size }, source)
}
