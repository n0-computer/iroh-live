//! A player's playout clock: a `moq_mux::timeline` shared by its tasks.
//!
//! The video path folds each decoded frame into the clock with
//! [`PlayoutClock::received`] and waits for it with
//! [`PlayoutClock::wait_async`]. While audio plays, the audio path owns the
//! anchor through an [`AudioLatency`] guard: each write reports the media time
//! it ends at and the sound queued ahead of it, so a picture shows when its
//! sound plays. Without audio, video anchors the clock, and playback runs the
//! clock's delay behind the live edge.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use moq_mux::timeline::{Audio, Consumer, Producer};
use moq_net::{Timestamp, time::Instant};
use tokio::sync::Notify;

/// The clock that keeps a player's audio and video in step.
///
/// Clones share state. Each player has one, shared by its video and audio
/// paths.
#[derive(Clone, derive_more::Debug)]
#[debug("PlayoutClock")]
pub(crate) struct PlayoutClock {
    inner: Arc<Inner>,
}

struct Inner {
    /// The clock the tasks update, and its delay.
    ///
    /// [`PlayoutClock::set_delay`] starts a new clock, so every task updates
    /// it through here rather than through a clone of its own.
    current: Mutex<Current>,
    /// Reads whichever clock the producer last started.
    consumer: Consumer,
    /// Wakes [`PlayoutClock::wait_async`] when the schedule moves or the clock closes.
    changed: Notify,
    /// Set by [`PlayoutClock::close`] to make every wait return at once.
    closed: AtomicBool,
}

struct Current {
    producer: Producer,
    /// How far behind the live edge playback runs.
    delay: Duration,
}

impl PlayoutClock {
    /// Creates a playout clock that runs `delay` behind the live edge.
    pub(crate) fn new(delay: Duration) -> Self {
        let producer = Producer::new(delay);
        Self {
            inner: Arc::new(Inner {
                consumer: producer.consume(),
                current: Mutex::new(Current { producer, delay }),
                changed: Notify::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Records the arrival of a video frame stamped `timestamp`.
    pub(crate) fn received(&self, timestamp: Timestamp) {
        let moved = self.current().producer.video(timestamp, Instant::now());
        if moved {
            self.inner.changed.notify_waiters();
        }
    }

    /// Starts the video timeline over, for a broadcast on a new route.
    ///
    /// A publisher behind the new route may have restarted its timestamps.
    /// While audio plays, its next write moves the anchor instead.
    pub(crate) fn restart(&self) {
        self.current().producer.video_restarted();
        self.inner.changed.notify_waiters();
    }

    /// Waits until the frame stamped `timestamp` is due.
    ///
    /// Recomputes the deadline whenever the schedule moves during the wait.
    /// Returns `true` when the frame should render, and `false` once the clock
    /// is closed.
    ///
    /// Cancellation safe: the wait keeps no state outside the future.
    pub(crate) async fn wait_async(&self, timestamp: Timestamp) -> bool {
        loop {
            // Register before reading the deadline, so a `close` or a move
            // between the two is not missed.
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();

            if self.inner.closed.load(Ordering::Acquire) {
                return false;
            }
            // No anchor yet: render now.
            let Some(due) = self.inner.consumer.due(timestamp) else {
                return true;
            };
            let now = Instant::now();
            if due <= now {
                return true;
            }
            // A move or a close wakes the wait early, so a shutdown does not
            // wait out the playout delay.
            tokio::select! {
                () = tokio::time::sleep(due.duration_since(now)) => return true,
                () = changed => {}
            }
        }
    }

    /// Returns how far behind the live edge playback runs.
    pub(crate) fn latency(&self) -> Duration {
        self.current().delay
    }

    /// Runs playback `delay` behind the live edge from now on.
    ///
    /// Starts a new clock, so the schedule moves once: the next video frame or
    /// audio write anchors it again.
    pub(crate) fn set_delay(&self, delay: Duration) {
        {
            let mut current = self.current();
            current.producer = current.producer.clone().reset(delay);
            current.delay = delay;
        }
        self.inner.changed.notify_waiters();
    }

    /// Registers an audio path, which owns the anchor while it plays.
    ///
    /// Dropping the returned guard hands the anchor back to video.
    pub(crate) fn register_audio(&self) -> AudioLatency {
        AudioLatency {
            clock: self.clone(),
        }
    }

    /// Closes the clock, so current and later waits return at once.
    pub(crate) fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.changed.notify_waiters();
    }

    fn current(&self) -> std::sync::MutexGuard<'_, Current> {
        self.inner.current.lock().expect("poisoned")
    }
}

/// An audio path's registration with a playout clock.
///
/// Created by [`PlayoutClock::register_audio`]. Dropping it tells the clock
/// that the speaker went quiet, so video takes the anchor back.
#[derive(Debug)]
pub(crate) struct AudioLatency {
    clock: PlayoutClock,
}

impl AudioLatency {
    /// Reports a write to the speaker that ends at media time `end`.
    ///
    /// `buffered` is the sound queued ahead of the speaker after the write,
    /// so `end` sounds `buffered` from now.
    pub(crate) fn set(&self, end: Duration, buffered: Duration) {
        let moved = {
            let current = self.clock.current();
            let audio = Audio {
                end,
                buffered,
                target: current.delay,
            };
            current.producer.audio(audio, Instant::now())
        };
        if moved {
            self.clock.inner.changed.notify_waiters();
        }
    }
}

impl Drop for AudioLatency {
    fn drop(&mut self) {
        self.clock.current().producer.audio_stopped();
        self.clock.inner.changed.notify_waiters();
    }
}
