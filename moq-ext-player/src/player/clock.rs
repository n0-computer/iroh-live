//! A player's playout clock: a `moq_mux::sync` clock shared by its tasks.
//!
//! Each media path folds its frames into the clock: video through
//! [`PlayoutClock::received`], audio through [`AudioLatency::received`]. The
//! clock's reference is the earliest arrival either has seen, and playback runs
//! the clock's delay behind it. Video waits for each picture with
//! [`PlayoutClock::wait_async`]. Audio does not own the clock: the speaker
//! stretches toward the time [`AudioLatency::anchor`] says its sound is due, so
//! a late audio path never holds the pictures back.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use moq_mux::{
    container::Kind,
    sync::{Consumer, Producer, Track},
};
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
    /// Hands out the audio paths' tracks on the shared clock.
    producer: Producer,
    /// Reads the schedule.
    consumer: Consumer,
    /// The video path's hold on the clock, which also holds the delay.
    video: Track,
    /// How far behind the live edge playback runs, as last set.
    delay: Mutex<Duration>,
    /// Wakes [`PlayoutClock::wait_async`] when the schedule moves or the clock closes.
    changed: Notify,
    /// Set by [`PlayoutClock::close`] to make every wait return at once.
    closed: AtomicBool,
}

impl PlayoutClock {
    /// Creates a playout clock that runs `delay` behind the live edge.
    pub(crate) fn new(delay: Duration) -> Self {
        let producer = Producer::new(delay);
        let video = producer.track(Kind::Video);
        video.set_target(delay);
        Self {
            inner: Arc::new(Inner {
                consumer: producer.consume(),
                producer,
                video,
                delay: Mutex::new(delay),
                changed: Notify::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Records the arrival of a video frame stamped `timestamp`.
    pub(crate) fn received(&self, timestamp: Timestamp) {
        if self.inner.video.received(timestamp, Instant::now()) {
            self.inner.changed.notify_waiters();
        }
    }

    /// Starts the video timeline over, for a broadcast on a new route.
    ///
    /// A publisher behind the new route may have restarted its timestamps, so
    /// the video arrivals so far stop holding the clock.
    pub(crate) fn restart(&self) {
        self.inner.video.reset();
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
        *self.inner.delay.lock().expect("poisoned")
    }

    /// Runs playback `delay` behind the live edge from now on.
    pub(crate) fn set_delay(&self, delay: Duration) {
        *self.inner.delay.lock().expect("poisoned") = delay;
        self.inner.video.set_target(delay);
        self.inner.changed.notify_waiters();
    }

    /// Registers an audio path, which folds its frames into the clock.
    ///
    /// Dropping the returned guard releases the audio path's hold on the clock.
    pub(crate) fn register_audio(&self) -> AudioLatency {
        AudioLatency {
            clock: self.clone(),
            track: self.inner.producer.track(Kind::Audio),
            #[cfg(feature = "playback")]
            anchored: None,
        }
    }

    /// Closes the clock, so current and later waits return at once.
    pub(crate) fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.changed.notify_waiters();
    }
}

/// An audio path's registration with a playout clock.
///
/// Created by [`PlayoutClock::register_audio`]. Dropping it releases the audio
/// path's arrivals, so video alone holds the clock.
#[derive(derive_more::Debug)]
#[debug("AudioLatency")]
pub(crate) struct AudioLatency {
    clock: PlayoutClock,
    track: Track,
    /// The last anchor handed out: when its end was due, the end, and the delay.
    #[cfg(feature = "playback")]
    anchored: Option<(Instant, Duration, Duration)>,
}

/// Where the speaker's written audio should sound, from [`AudioLatency::anchor`].
#[cfg(feature = "playback")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Anchor {
    /// When the end of the audio written so far is due.
    pub(crate) at: Instant,
    /// How far the clock's reference moved earlier since the last anchor, which
    /// the speaker skips rather than speeds through.
    pub(crate) jumped: Duration,
}

impl AudioLatency {
    /// Records the arrival of an audio frame stamped `timestamp`.
    pub(crate) fn received(&self, timestamp: Timestamp) {
        if self.track.received(timestamp, Instant::now()) {
            self.clock.inner.changed.notify_waiters();
        }
    }

    /// Returns when the audio written up to media time `end` is due to sound,
    /// or `None` before the clock has an anchor.
    #[cfg(feature = "playback")]
    pub(crate) fn anchor(&mut self, end: Duration) -> Option<Anchor> {
        let timestamp = Timestamp::try_from(end).ok()?;
        let at = self.clock.inner.consumer.due(timestamp)?;
        // The end as the clock placed it, so rounding never reads as a jump.
        let end = Duration::from(timestamp);
        let delay = self.clock.inner.consumer.delay();
        // The reference is `at - end - delay`. Compare it with the last one's.
        let nanos = |duration: Duration| duration.as_nanos() as i128;
        let jumped = self
            .anchored
            .replace((at, end, delay))
            .map_or(0, |(was, from, before)| {
                let earlier = match was.checked_duration_since(at) {
                    Some(ahead) => nanos(ahead),
                    None => -nanos(at.duration_since(was)),
                };
                earlier + nanos(end) - nanos(from) + nanos(delay) - nanos(before)
            });
        let jumped = Duration::from_nanos(jumped.clamp(0, u64::MAX as i128) as u64);
        Some(Anchor { at, jumped })
    }
}
