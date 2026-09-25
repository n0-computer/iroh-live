//! Latest-frame-wins video frames, with a cursor per handle.
//!
//! Players and sources both hand out this type, and every renderer reads it.
//! It wraps a `tokio::sync::watch` channel. The producer overwrites one value,
//! so a reader that falls behind skips to the newest picture instead of
//! draining a backlog.

use std::sync::{Arc, OnceLock};

use tokio::sync::watch;

use crate::{error::Error, video};

/// The producing end of a frame stream.
///
/// Dropping the last clone ends the stream: every reader's
/// [`next`](VideoFrames::next) returns the last frame, then `None`.
#[derive(derive_more::Debug, Clone)]
pub(crate) struct FrameSlot {
    #[debug(skip)]
    tx: watch::Sender<Option<Arc<video::Frame>>>,
    failure: Arc<OnceLock<Arc<Error>>>,
}

impl FrameSlot {
    /// Creates an empty slot.
    pub(crate) fn new() -> Self {
        Self {
            tx: watch::Sender::new(None),
            failure: Default::default(),
        }
    }

    /// Replaces the current frame, waking every reader.
    pub(crate) fn send(&self, frame: Arc<video::Frame>) {
        self.tx.send_replace(Some(frame));
    }

    /// Records why the stream is about to end, for [`FrameReader::failure`].
    pub(crate) fn fail(&self, failure: Arc<Error>) {
        let _ = self.failure.set(failure);
    }

    /// Returns a reader starting at the current frame.
    pub(crate) fn frames(&self) -> VideoFrames {
        VideoFrames::new(self.tx.subscribe())
    }

    /// Returns a handle that reads the slot without keeping it open.
    pub(crate) fn reader(&self) -> FrameReader {
        FrameReader {
            rx: self.tx.subscribe(),
            failure: self.failure.clone(),
        }
    }
}

/// Reads a slot without holding it open.
///
/// A source keeps this, so its stream ends when the frames stop even while
/// handles to the source remain.
#[derive(derive_more::Debug, Clone)]
pub(crate) struct FrameReader {
    #[debug(skip)]
    rx: watch::Receiver<Option<Arc<video::Frame>>>,
    failure: Arc<OnceLock<Arc<Error>>>,
}

impl FrameReader {
    /// Returns a reader starting at the current frame.
    pub(crate) fn frames(&self) -> VideoFrames {
        VideoFrames::new(self.rx.clone())
    }

    /// Returns why the stream ended, if it failed.
    pub(crate) fn failure(&self) -> Option<Arc<Error>> {
        self.failure.get().cloned()
    }
}

/// Latest-frame-wins video frames.
///
/// Returned by [`VideoSource::frames`](crate::VideoSource::frames) for a local
/// preview and by [`Player::video`](crate::Player::video) for playback. Each
/// clone keeps its own cursor. A frame comes as an `Arc` because upstream
/// frames are not `Clone`: a GPU surface is shared, not copied.
///
/// On a fresh handle, the first [`next`](Self::next) returns at once if a
/// picture is already there.
#[derive(derive_more::Debug, Clone)]
pub struct VideoFrames {
    #[debug(skip)]
    rx: watch::Receiver<Option<Arc<video::Frame>>>,
}

impl VideoFrames {
    fn new(mut rx: watch::Receiver<Option<Arc<video::Frame>>>) -> Self {
        rx.mark_changed();
        Self { rx }
    }

    /// Returns the newest frame, seen or not, without moving the cursor.
    ///
    /// For render loops that redraw every vsync.
    pub fn current(&self) -> Option<Arc<video::Frame>> {
        self.rx.borrow().clone()
    }

    /// Waits for a frame newer than the last this handle returned.
    ///
    /// Frames that arrive faster than the handle reads them coalesce: only the
    /// newest is returned. Returns `None` once the producer is gone and this
    /// handle has seen its last frame.
    ///
    /// Cancellation safe.
    pub async fn next(&mut self) -> Option<Arc<video::Frame>> {
        loop {
            self.rx.changed().await.ok()?;
            if let Some(frame) = self.rx.borrow_and_update().clone() {
                return Some(frame);
            }
        }
    }

    /// Returns a newer frame without waiting, if there is one.
    ///
    /// For a render loop that draws only when the picture changed.
    pub fn try_next(&mut self) -> Option<Arc<video::Frame>> {
        let frame = self.rx.borrow_and_update();
        if frame.has_changed() {
            frame.clone()
        } else {
            None
        }
    }

    /// Waits until the producer is gone. Cancellation safe.
    pub(crate) async fn closed(&self) {
        let mut rx = self.rx.clone();
        while rx.changed().await.is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::video::{Size, Surface};

    fn frame(micros: u64) -> Arc<video::Frame> {
        let size = Size::new(2, 2);
        let surface = Surface::rgba(&[0u8; 16], size).expect("a 2x2 picture");
        let timestamp = moq_net::Timestamp::from_micros(micros).expect("in range");
        Arc::new(video::Frame::new(surface, timestamp))
    }

    fn micros(frame: &video::Frame) -> u64 {
        frame.timestamp.as_micros() as u64
    }

    #[tokio::test]
    async fn every_handle_keeps_its_own_cursor() {
        let slot = FrameSlot::new();
        let mut first = slot.frames();
        let mut second = slot.frames();
        slot.send(frame(1));
        assert_eq!(first.next().await.as_deref().map(micros), Some(1));
        assert_eq!(second.next().await.as_deref().map(micros), Some(1));
    }

    #[tokio::test]
    async fn frames_that_outrun_a_reader_coalesce() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        slot.send(frame(1));
        slot.send(frame(2));
        slot.send(frame(3));
        assert_eq!(frames.next().await.as_deref().map(micros), Some(3));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), frames.next())
                .await
                .is_err(),
            "three sends before one read are one frame",
        );
    }

    #[test]
    fn try_next_takes_only_what_is_new() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        assert!(frames.try_next().is_none());
        slot.send(frame(4));
        assert_eq!(frames.current().as_deref().map(micros), Some(4));
        assert_eq!(frames.try_next().as_deref().map(micros), Some(4));
        assert!(frames.try_next().is_none(), "the same frame twice");
    }

    #[tokio::test]
    async fn a_fresh_handle_sees_the_current_frame() {
        let slot = FrameSlot::new();
        slot.send(frame(5));
        let mut frames = slot.frames();
        assert_eq!(frames.next().await.as_deref().map(micros), Some(5));
    }

    #[tokio::test]
    async fn the_last_frame_is_read_before_the_close() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        slot.send(frame(9));
        drop(slot);
        assert_eq!(frames.next().await.as_deref().map(micros), Some(9));
        assert!(frames.next().await.is_none());
    }

    #[tokio::test]
    async fn a_reader_does_not_hold_the_slot_open() {
        let slot = FrameSlot::new();
        let reader = slot.reader();
        slot.fail(Arc::new(n0_error::e!(Error::Closed)));
        drop(slot);
        assert!(reader.frames().next().await.is_none());
        assert!(reader.failure().is_some());
    }
}
