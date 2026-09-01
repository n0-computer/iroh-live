//! Push-based video source for Android camera frames.
//!
//! Android delivers camera frames through callbacks (CameraX `ImageAnalysis` or
//! Camera2 `ImageReader`), while `moq_media::publish` reads a stream. This
//! bridges the two: the app pushes a frame from whichever thread the callback
//! runs on, and the publish task pulls the newest one.
//!
//! Newer frames replace unconsumed older ones. That is the right policy for a
//! camera, where a frame the encoder never got to is stale rather than owed.

use std::sync::Arc;

use moq_media::{
    frame_channel::{FrameReceiver, FrameSender, frame_channel},
    publish::VideoSource,
};
use moq_video::{Frame, Size, Surface};
use n0_error::{Result, stack_error};

/// Errors raised while pushing a camera frame.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum CameraError {
    /// The pixel buffer did not match the declared size and format.
    #[error("invalid camera frame")]
    Frame {
        /// What the surface rejected.
        #[error(source, std_err)]
        source: moq_video::Error,
    },
}

/// The app side of the bridge: push frames here from the camera callback.
///
/// Cheap to clone, and safe to hold across threads, so the JNI layer can keep
/// one alive for the lifetime of the camera session.
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
    /// Fails if `rgba` does not match the size this sink was created with.
    pub fn push_rgba(&self, rgba: &[u8], timestamp: moq_net::Timestamp) -> Result<(), CameraError> {
        let surface = Surface::rgba(rgba, self.size)
            .map_err(|source| n0_error::e!(CameraError::Frame { source }))?;
        self.frames.send(Frame::new(surface, timestamp));
        Ok(())
    }

    /// Pushes a frame the caller already built, for a source that can hand over
    /// something better than packed RGBA.
    pub fn push(&self, frame: Frame) {
        self.frames.send(frame);
    }

    /// The size every pushed frame must have.
    pub fn size(&self) -> Size {
        self.size
    }
}

/// Creates a camera bridge for a fixed capture size.
///
/// The returned [`VideoSource`] goes to
/// [`VideoPublisher::set`](moq_media::publish::VideoPublisher::set); the
/// [`CameraSink`] goes to whatever drives the camera.
pub fn camera(size: Size) -> (CameraSink, VideoSource) {
    let (tx, rx) = frame_channel();
    let sink = CameraSink { frames: tx, size };
    let receiver = Arc::new(rx);
    let source = VideoSource::Frames(Box::pin(n0_future::stream::unfold(
        receiver,
        |receiver: Arc<FrameReceiver<Frame>>| async move {
            let frame = receiver.recv().await?;
            Some((frame, receiver))
        },
    )));
    (sink, source)
}
