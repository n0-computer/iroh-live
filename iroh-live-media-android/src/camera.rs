//! Push-based video source for Android camera frames.
//!
//! Android delivers camera frames through callbacks (CameraX `ImageAnalysis` or
//! Camera2 `ImageReader`), while a broadcast reads a
//! [`VideoSource`]. This bridges the two with
//! [`VideoSource::push`](iroh_live_media::VideoSource::push): the app pushes a
//! frame from whichever thread the callback runs on, and the broadcast encodes
//! the newest one.
//!
//! Newer frames replace unconsumed older ones. That is the right policy for a
//! camera, where a frame the encoder never got to is stale rather than owed.

use iroh_live_media::{Closed, FrameSender, VideoFormat, VideoSource};
use moq_video::{Frame, Rate, Size, Surface};
use n0_error::stack_error;

/// Errors raised while pushing a camera frame.
#[stack_error(derive, add_meta)]
#[non_exhaustive]
pub enum CameraError {
    /// The pixel buffer did not match the declared size and format.
    #[error("invalid camera frame")]
    Frame {
        /// What the surface rejected.
        #[error(source, std_err)]
        source: moq_video::Error,
    },
    /// The source the frames were for has gone.
    #[error("the camera source has closed")]
    Closed {
        /// What the sender reported.
        #[error(source, std_err)]
        source: Closed,
    },
}

/// The app side of the bridge: push frames here from the camera callback.
///
/// Cheap to clone, and safe to hold across threads, so the JNI layer can keep
/// one alive for the lifetime of the camera session. The source ends once
/// every clone is dropped.
#[derive(Debug, Clone)]
pub struct CameraSink {
    frames: FrameSender<Frame>,
    size: Size,
}

impl CameraSink {
    /// Pushes one RGBA frame, replacing any frame not yet consumed.
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

    /// Pushes a frame the caller already built, for a source that can hand over
    /// something better than packed RGBA.
    ///
    /// # Errors
    ///
    /// Fails if the source has closed.
    pub fn push(&self, frame: Frame) -> Result<(), CameraError> {
        self.frames
            .push(frame)
            .map_err(|source| n0_error::e!(CameraError::Closed { source }))
    }

    /// Reports whether anything is encoding the frames, so a camera session can
    /// stop capturing while nobody watches.
    pub fn demand(&self) -> n0_watcher::Direct<bool> {
        self.frames.demand()
    }

    /// The size every pushed frame must have.
    pub fn size(&self) -> Size {
        self.size
    }
}

/// Creates a camera bridge for a fixed capture size and rate.
///
/// The returned [`VideoSource`] goes to
/// [`LocalBroadcast::set_video`](iroh_live_media::LocalBroadcast::set_video);
/// the [`CameraSink`] goes to whatever drives the camera.
pub fn camera(size: Size, rate: Rate) -> (CameraSink, VideoSource) {
    let (frames, source) = VideoSource::push(VideoFormat::new(size, rate));
    (CameraSink { frames, size }, source)
}
