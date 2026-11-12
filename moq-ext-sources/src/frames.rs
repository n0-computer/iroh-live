//! Latest-frame-wins video frames, with a cursor per handle.
//!
//! Players and sources both hand out this type, and every renderer reads it.
//! It wraps a `tokio::sync::watch` channel. The producer overwrites one value,
//! so a reader that falls behind skips to the newest picture instead of
//! draining a backlog. Each frame keeps the instant it was sent, which a
//! reader gets from [`VideoFrames::sent_at`].

use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};

use tokio::sync::watch;

use crate::{Error, video};

/// A frame in a slot, with the instant it was sent.
type Sent = Option<(Arc<video::Frame>, Instant)>;

/// The producing end of a frame stream.
///
/// Dropping the last clone ends the stream: every reader's
/// [`next`](VideoFrames::next) returns the last frame, then `None`.
#[derive(derive_more::Debug, Clone)]
pub struct FrameSlot {
    #[debug(skip)]
    tx: watch::Sender<Sent>,
    failure: Arc<OnceLock<Arc<Error>>>,
}

impl Default for FrameSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSlot {
    /// Creates an empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tx: watch::Sender::new(None),
            failure: Default::default(),
        }
    }

    /// Replaces the current frame, waking every reader.
    ///
    /// Stamps the frame with the current instant, for [`VideoFrames::sent_at`].
    pub fn send(&self, frame: Arc<video::Frame>) {
        self.send_at(frame, Instant::now());
    }

    /// Replaces the current frame with one sent at `at`, for a stream that forwards another.
    pub(crate) fn send_at(&self, frame: Arc<video::Frame>, at: Instant) {
        self.tx.send_replace(Some((frame, at)));
    }

    /// Records why the stream is about to end, for [`FrameReader::failure`].
    ///
    /// The first failure wins.
    pub(crate) fn fail(&self, failure: Arc<Error>) {
        let _ = self.failure.set(failure);
    }

    /// Returns a reader starting at the current frame.
    #[must_use]
    pub fn frames(&self) -> VideoFrames {
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

/// A reader of a slot that does not hold it open.
///
/// A source keeps this, so its stream ends when the frames stop even while
/// handles to the source remain.
#[derive(derive_more::Debug, Clone)]
pub(crate) struct FrameReader {
    #[debug(skip)]
    rx: watch::Receiver<Sent>,
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
/// [`VideoSource::frames`](crate::VideoSource::frames) returns it for a local
/// preview, and a player returns it for playback. Each clone keeps its own
/// cursor. A frame comes as an `Arc` because upstream frames are not `Clone`:
/// a GPU surface is shared, not copied.
///
/// On a fresh handle, the first [`next`](Self::next) returns at once if a
/// picture is already there.
#[derive(derive_more::Debug, Clone)]
pub struct VideoFrames {
    #[debug(skip)]
    rx: watch::Receiver<Sent>,
    /// When the frame this handle last returned was sent.
    sent_at: Option<Instant>,
}

impl VideoFrames {
    /// Creates a handle whose first [`next`](Self::next) returns the current frame.
    fn new(mut rx: watch::Receiver<Sent>) -> Self {
        rx.mark_changed();
        Self { rx, sent_at: None }
    }

    /// Returns the newest frame, seen or not, without moving the cursor.
    ///
    /// For render loops that redraw every vsync.
    #[must_use]
    pub fn current(&self) -> Option<Arc<video::Frame>> {
        self.rx.borrow().as_ref().map(|(frame, _)| frame.clone())
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
            if let Some((frame, at)) = self.rx.borrow_and_update().clone() {
                self.sent_at = Some(at);
                return Some(frame);
            }
        }
    }

    /// Returns a newer frame without waiting, if there is one.
    ///
    /// For a render loop that draws only when the picture changed.
    #[must_use]
    pub fn try_next(&mut self) -> Option<Arc<video::Frame>> {
        let sent = self.rx.borrow_and_update();
        if !sent.has_changed() {
            return None;
        }
        let (frame, at) = sent.clone()?;
        self.sent_at = Some(at);
        Some(frame)
    }

    /// Returns when the last frame this handle returned entered the stream.
    ///
    /// That frame is the one [`next`](Self::next) or
    /// [`try_next`](Self::try_next) returned last. For a source, that is when the application pushed the frame or the
    /// capture thread handed it over, before any encoder saw it. For a player,
    /// it is when the picture was presented. Returns `None` before the first
    /// frame.
    #[must_use]
    pub fn sent_at(&self) -> Option<Instant> {
        self.sent_at
    }

    /// Waits until the producer is gone.
    ///
    /// Cancellation safe.
    pub async fn closed(&self) {
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
    async fn a_frame_keeps_the_instant_it_was_sent() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        assert_eq!(frames.sent_at(), None);
        let before = Instant::now();
        slot.send(frame(1));
        let after = Instant::now();
        frames.next().await.expect("a frame");
        let sent = frames.sent_at().expect("stamped");
        assert!(before <= sent && sent <= after, "{sent:?}");
        let earlier = before - Duration::from_millis(5);
        slot.send_at(frame(2), earlier);
        assert!(
            frames.current().is_some(),
            "current leaves the cursor alone"
        );
        assert_eq!(frames.sent_at(), Some(sent));
        frames.try_next().expect("a newer frame");
        assert_eq!(
            frames.sent_at(),
            Some(earlier),
            "a forwarded frame keeps its instant"
        );
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
