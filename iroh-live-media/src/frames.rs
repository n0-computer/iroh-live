//! Latest-frame-wins video frames, with a cursor per handle.
//!
//! Every renderer in the crate reads the same type: a player's decoded
//! pictures, a source's captured ones, and anything that scans them for a code.
//! The producer overwrites one slot, so a reader that falls behind skips to the
//! newest picture instead of draining a backlog, and a backlog of GPU surfaces
//! never builds up out of the decoder's pool. What each reader keeps is only how
//! far it has read, so two readers of one stream both see every picture that
//! is current when they look, rather than splitting the stream between them.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use tokio::sync::Notify;

use crate::{error::Error, video};

/// What the slot holds, behind its lock.
#[derive(Default)]
struct State {
    /// The newest frame, seen by anyone or not.
    latest: Option<Arc<video::Frame>>,
    /// Bumped on every frame, so a reader tells new from seen by comparing.
    generation: u64,
    /// Set once the producer is gone.
    closed: bool,
    /// Why it went, if it failed rather than ended.
    failure: Option<Arc<Error>>,
}

/// The shared slot a producer writes and every [`VideoFrames`] reads.
struct Slot {
    state: Mutex<State>,
    notify: Notify,
}

impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().expect("poisoned");
        f.debug_struct("Slot")
            .field("generation", &state.generation)
            .field("closed", &state.closed)
            .finish()
    }
}

/// The producing end of a frame slot.
///
/// Crate-private: sources and players hold one, and hand out [`VideoFrames`].
/// Dropping the last clone closes the slot, and every reader's
/// [`next`](VideoFrames::next) then returns `None`.
#[derive(Debug, Clone)]
pub(crate) struct FrameSlot {
    slot: Arc<Slot>,
    /// Closes the slot when the last producer handle goes.
    _closer: Arc<Closer>,
}

/// Closes the slot on drop.
#[derive(Debug)]
struct Closer(Arc<Slot>);

impl Drop for Closer {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("poisoned");
        state.closed = true;
        drop(state);
        self.0.notify.notify_waiters();
    }
}

impl FrameSlot {
    /// Creates an empty, open slot.
    pub(crate) fn new() -> Self {
        let slot = Arc::new(Slot {
            state: Mutex::new(State::default()),
            notify: Notify::new(),
        });
        Self {
            _closer: Arc::new(Closer(slot.clone())),
            slot,
        }
    }

    /// Replaces the current frame, waking every reader.
    pub(crate) fn send(&self, frame: Arc<video::Frame>) {
        let mut state = self.slot.state.lock().expect("poisoned");
        state.latest = Some(frame);
        state.generation += 1;
        drop(state);
        self.slot.notify.notify_waiters();
    }

    /// Closes the slot now, recording why when it failed.
    ///
    /// Readers see the last frame through [`VideoFrames::current`] and `None`
    /// from their next [`VideoFrames::next`].
    pub(crate) fn close(&self, failure: Option<Arc<Error>>) {
        let mut state = self.slot.state.lock().expect("poisoned");
        state.closed = true;
        if state.failure.is_none() {
            state.failure = failure;
        }
        drop(state);
        self.slot.notify.notify_waiters();
    }

    /// Returns why the slot closed, if it failed.
    #[cfg(test)]
    fn failure(&self) -> Option<Arc<Error>> {
        self.slot.state.lock().expect("poisoned").failure.clone()
    }

    /// Returns a reader starting at the current frame.
    pub(crate) fn frames(&self) -> VideoFrames {
        VideoFrames::new(self.slot.clone())
    }

    /// Returns a handle that reads the slot without keeping it open.
    pub(crate) fn reader(&self) -> FrameReader {
        FrameReader {
            slot: self.slot.clone(),
        }
    }
}

/// Reads a slot without holding it open.
///
/// What a source keeps, so that the slot closes when the frames stop coming
/// rather than when the last handle to the source goes.
#[derive(Debug, Clone)]
pub(crate) struct FrameReader {
    slot: Arc<Slot>,
}

impl FrameReader {
    /// Returns a reader starting at the current frame.
    pub(crate) fn frames(&self) -> VideoFrames {
        VideoFrames::new(self.slot.clone())
    }

    /// Returns why the slot closed, if it failed.
    pub(crate) fn failure(&self) -> Option<Arc<Error>> {
        self.slot.state.lock().expect("poisoned").failure.clone()
    }
}

/// Latest-frame-wins video frames. Each clone keeps its own cursor.
///
/// Returned by [`VideoSource::frames`](crate::VideoSource::frames) for a local
/// preview and by [`Player::video`](crate::Player::video) for playback. A frame
/// comes as an `Arc` because upstream frames are not `Clone`: a GPU surface is
/// shared, not copied, and every reader of one stream sees the same picture.
///
/// A fresh handle reports the frame current when it was created as new, so the
/// first [`next`](Self::next) returns at once if a picture is already there.
pub struct VideoFrames {
    slot: Arc<Slot>,
    /// The generation this handle last returned.
    seen: u64,
}

impl fmt::Debug for VideoFrames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrames")
            .field("seen", &self.seen)
            .field("slot", &self.slot)
            .finish()
    }
}

impl Clone for VideoFrames {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot.clone(),
            seen: self.seen,
        }
    }
}

impl VideoFrames {
    fn new(slot: Arc<Slot>) -> Self {
        Self { slot, seen: 0 }
    }

    /// Returns the newest frame, seen or not.
    ///
    /// For render loops that redraw every vsync and want whatever is current.
    /// Does not move this handle's cursor.
    pub fn current(&self) -> Option<Arc<video::Frame>> {
        self.slot.state.lock().expect("poisoned").latest.clone()
    }

    /// Waits for a frame newer than the last this handle returned.
    ///
    /// Frames that arrive faster than the handle reads them coalesce: only the
    /// newest is returned. Returns `None` once the producer is gone and this
    /// handle has seen its last frame.
    ///
    /// Cancellation safe: dropping the future loses nothing, and the next call
    /// returns the same frame it would have.
    pub async fn next(&mut self) -> Option<Arc<video::Frame>> {
        loop {
            // Registered before the check, so a frame or a close landing
            // between the two wakes this rather than being missed.
            let notified = self.slot.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.slot.state.lock().expect("poisoned");
                if state.generation != self.seen
                    && let Some(frame) = &state.latest
                {
                    self.seen = state.generation;
                    return Some(frame.clone());
                }
                if state.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Returns a frame newer than the last this handle returned without
    /// waiting, or `None` when there is none.
    ///
    /// For a render loop that draws only when the picture changed.
    pub fn try_next(&mut self) -> Option<Arc<video::Frame>> {
        let state = self.slot.state.lock().expect("poisoned");
        if state.generation == self.seen {
            return None;
        }
        let frame = state.latest.clone()?;
        self.seen = state.generation;
        Some(frame)
    }

    /// Waits until the producer is gone. Cancellation safe.
    pub(crate) async fn closed(&self) {
        loop {
            let notified = self.slot.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.slot.state.lock().expect("poisoned").closed {
                return;
            }
            notified.await;
        }
    }

    /// Reports whether a frame newer than the last this handle returned is
    /// waiting, without taking it.
    pub fn has_new(&self) -> bool {
        let state = self.slot.state.lock().expect("poisoned");
        state.generation != self.seen && state.latest.is_some()
    }

    /// Reports whether the producer is gone.
    pub fn is_closed(&self) -> bool {
        self.slot.state.lock().expect("poisoned").closed
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
        // The old `FrameReceiver::take` handed the frame to whichever holder
        // asked first and nothing to the other.
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

    #[tokio::test]
    async fn current_does_not_move_the_cursor() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        slot.send(frame(7));
        assert_eq!(frames.current().as_deref().map(micros), Some(7));
        assert!(frames.has_new());
        assert_eq!(frames.next().await.as_deref().map(micros), Some(7));
        assert!(!frames.has_new());
        assert_eq!(frames.current().as_deref().map(micros), Some(7));
    }

    #[test]
    fn try_next_takes_only_what_is_new() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        assert!(frames.try_next().is_none());
        slot.send(frame(4));
        assert_eq!(frames.try_next().as_deref().map(micros), Some(4));
        assert!(frames.try_next().is_none(), "the same frame twice");
    }

    #[tokio::test]
    async fn a_clone_starts_where_its_original_was() {
        let slot = FrameSlot::new();
        let mut original = slot.frames();
        slot.send(frame(1));
        original.next().await;
        let mut clone = original.clone();
        slot.send(frame(2));
        assert_eq!(clone.next().await.as_deref().map(micros), Some(2));
        assert_eq!(original.next().await.as_deref().map(micros), Some(2));
    }

    #[tokio::test]
    async fn the_last_frame_is_read_before_the_close() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        slot.send(frame(9));
        drop(slot);
        assert_eq!(frames.next().await.as_deref().map(micros), Some(9));
        assert!(frames.next().await.is_none());
        assert!(frames.is_closed());
    }

    #[tokio::test]
    async fn a_waiting_reader_wakes_on_close() {
        let slot = FrameSlot::new();
        let mut frames = slot.frames();
        let waiter = tokio::spawn(async move { frames.next().await.is_none() });
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(slot);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .expect("the close woke the reader")
                .expect("the task ran")
        );
    }

    #[tokio::test]
    async fn a_close_with_a_failure_keeps_it() {
        let slot = FrameSlot::new();
        let failure = Arc::new(n0_error::e!(Error::Closed));
        slot.close(Some(failure.clone()));
        assert!(Arc::ptr_eq(&slot.failure().expect("kept"), &failure));
        assert!(slot.frames().next().await.is_none());
    }
}
