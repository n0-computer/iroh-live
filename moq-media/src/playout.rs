//! Playback policy for subscribed broadcasts.
//!
//! [`SyncMode`] decides whether the playout clock gates video against audio.
//! [`PlaybackPolicy::max_latency`] decides how much buffered media a subscriber
//! tolerates before skipping to the live edge, and is passed straight through
//! to `moq_video::decode::Config::latency_max`.

use std::time::Duration;

/// Whether the playout clock gates video at all.
///
/// [`Synced`](Self::Synced) runs the shared clock, ported from `moq/js` commit
/// `53fe78d8`. [`Unmanaged`](Self::Unmanaged) does nothing: a decoded frame
/// goes straight to the renderer.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, derive_more::Display, strum::VariantArray)]
pub enum SyncMode {
    /// The shared playout clock, and the default for live playback.
    ///
    /// Video frames are held by [`crate::sync::Sync::wait_async`] until they
    /// are due, which accounts for network jitter and for how much audio is
    /// still queued at the speaker.
    #[default]
    #[display("Synced")]
    Synced,

    /// No synchronization: a frame goes to the renderer as soon as it decodes.
    ///
    /// Right for a video-only broadcast, where there is nothing to align
    /// against and the clock would only add latency.
    #[display("Off")]
    Unmanaged,
}

/// Playback policy for a subscribed broadcast.
///
/// Set at construction time via
/// [`RemoteBroadcast::with_playback_policy`](crate::subscribe::RemoteBroadcast::with_playback_policy),
/// or update before resubscribing via
/// [`RemoteBroadcast::set_playback_policy`](crate::subscribe::RemoteBroadcast::set_playback_policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackPolicy {
    /// Cross-track synchronization policy.
    pub sync: SyncMode,

    /// The most buffered media a subscriber tolerates before skipping to the
    /// live edge, passed to the decoder as `latency_max`.
    ///
    /// Raise it for continuity through congestion, lower it for faster
    /// recovery after a stall.
    pub max_latency: Duration,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self {
            sync: SyncMode::default(),
            max_latency: Duration::from_millis(150),
        }
    }
}

impl PlaybackPolicy {
    /// Synced playout with the default 150 ms latency budget.
    pub fn synced() -> Self {
        Self::default()
    }

    /// Unmanaged playout with the default 150 ms latency budget.
    pub fn unmanaged() -> Self {
        Self {
            sync: SyncMode::Unmanaged,
            ..Self::default()
        }
    }

    /// Returns a copy with a different maximum latency.
    #[must_use]
    pub fn with_max_latency(mut self, max_latency: Duration) -> Self {
        self.max_latency = max_latency;
        self
    }

    /// Returns a copy with a different sync mode.
    #[must_use]
    pub fn with_sync(mut self, sync: SyncMode) -> Self {
        self.sync = sync;
        self
    }
}
