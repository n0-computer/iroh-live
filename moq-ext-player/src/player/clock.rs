//! A player's playout clock: a `moq_mux::sync` clock shared by its tasks.
//!
//! Each media path folds its frames into the clock: video through
//! [`PlayoutClock::received`], audio through [`AudioClock::received`]. The
//! clock's reference is the earliest arrival either has seen, and playback runs
//! the clock's delay behind it. Video waits for each picture with
//! [`PlayoutClock::wait`]. Audio does not own the clock: the speaker
//! stretches toward the time [`AudioClock::anchor`] says its sound is due, so
//! a late audio path never holds the pictures back.

use std::{sync::Arc, time::Duration};

use moq_mux::{
    container::Kind,
    sync::{Consumer, Producer, Track},
};
use moq_net::{Timestamp, time::Instant};
use tokio_util::sync::CancellationToken;

/// The clock that keeps a player's audio and video in step.
///
/// Clones share state. Each player has one, shared by its video and audio
/// paths.
#[derive(Clone, derive_more::Debug)]
#[debug("PlayoutClock")]
pub(crate) struct PlayoutClock {
    inner: Arc<Inner>,
}

/// The state the clones of a [`PlayoutClock`] share.
struct Inner {
    /// Hands out the audio paths' tracks on the shared clock.
    producer: Producer,
    /// Reads the schedule.
    consumer: Consumer,
    /// The video path's hold on the clock and its target.
    video: Track,
    /// Cancelled by [`PlayoutClock::close`] to make every wait return at once.
    closed: CancellationToken,
}

impl PlayoutClock {
    /// Creates a playout clock that runs `delay` behind the live edge until a track asks for more.
    pub(crate) fn new(delay: Duration) -> Self {
        let producer = Producer::new(delay);
        let video = producer.track(Kind::Video);
        video.set_target(delay);
        Self {
            inner: Arc::new(Inner {
                consumer: producer.consume(),
                producer,
                video,
                closed: CancellationToken::new(),
            }),
        }
    }

    /// Records the arrival of a video frame stamped `timestamp`.
    pub(crate) fn received(&self, timestamp: Timestamp) {
        self.inner.video.received(timestamp, Instant::now());
    }

    /// Starts the video timeline over, for a broadcast on a new route.
    ///
    /// A publisher behind the new route may have restarted its timestamps, so
    /// the video arrivals so far stop holding the clock.
    pub(crate) fn restart(&self) {
        self.inner.video.reset();
    }

    /// Waits until the frame stamped `timestamp` is due.
    ///
    /// Recomputes the deadline whenever the schedule moves during the wait.
    /// Returns `true` when the frame should render, and `false` once the clock
    /// is closed.
    ///
    /// Cancellation safe: the wait keeps no state outside the future.
    pub(crate) async fn wait(&self, timestamp: Timestamp) -> bool {
        let consumer = &self.inner.consumer;
        let mut schedule = consumer.schedule();
        loop {
            if self.inner.closed.is_cancelled() {
                return false;
            }
            // No anchor yet: render now.
            let Some(due) = schedule.due(timestamp) else {
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
                next = consumer.changed(schedule) => schedule = next,
                () = self.inner.closed.cancelled() => return false,
            }
        }
    }

    /// Returns how far behind the live edge playback runs: the largest target of its tracks.
    pub(crate) fn delay(&self) -> Duration {
        self.inner.consumer.delay()
    }

    /// Asks the clock to run at least `target` behind the live edge for the video.
    ///
    /// Returns whether that moved the schedule.
    pub(crate) fn set_video_target(&self, target: Duration) -> bool {
        self.inner.video.set_target(target)
    }

    /// Registers an audio path, which folds its frames into the clock.
    ///
    /// Dropping the returned guard releases the audio path's hold on the clock.
    pub(crate) fn register_audio(&self) -> AudioClock {
        AudioClock {
            #[cfg(feature = "playback")]
            clock: self.clone(),
            track: self.inner.producer.track(Kind::Audio),
        }
    }

    /// Closes the clock, so current and later waits return at once.
    pub(crate) fn close(&self) {
        self.inner.closed.cancel();
    }
}

/// An audio path's registration with a playout clock.
///
/// Created by [`PlayoutClock::register_audio`]. Dropping it releases the audio
/// path's arrivals, so video alone holds the clock.
#[derive(derive_more::Debug)]
#[debug("AudioClock")]
pub(crate) struct AudioClock {
    /// The shared clock, which places the speaker's audio.
    #[cfg(feature = "playback")]
    clock: PlayoutClock,
    track: Track,
}

impl AudioClock {
    /// Records the arrival of an audio frame stamped `timestamp`.
    pub(crate) fn received(&self, timestamp: Timestamp) {
        self.track.received(timestamp, Instant::now());
    }

    /// Asks the clock to run at least `target` behind the live edge for the audio.
    ///
    /// Returns whether that moved the schedule. The ask ends when this
    /// registration is dropped.
    #[cfg(feature = "playback")]
    pub(crate) fn set_target(&self, target: Duration) -> bool {
        self.track.set_target(target)
    }

    /// Returns when the audio written up to media time `end` is due to sound.
    ///
    /// Returns `None` before the clock has an anchor.
    #[cfg(feature = "playback")]
    pub(crate) fn anchor(&self, end: Duration) -> Option<Instant> {
        let timestamp = Timestamp::try_from(end).ok()?;
        self.clock.inner.consumer.due(timestamp)
    }
}
