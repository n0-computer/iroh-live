//! Playback policy for subscribed broadcasts.
//!
//! [`SyncMode`] controls how audio and video are aligned at playout time.
//! [`PlaybackPolicy::max_latency`] controls how much buffered media we
//! tolerate before skipping forward — this drives Hang's ordered
//! consumer.

use std::time::Duration;

/// A/V synchronization behavior at playout time.
///
/// [`Synced`](Self::Synced) enables the shared playout clock (ported
/// from `moq/js` commit `53fe78d8`, `js/watch/src/sync.ts`).
/// [`Unmanaged`](Self::Unmanaged) uses PTS-cadence pacing with no
/// cross-track synchronization.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, derive_more::Display, strum::VariantArray)]
pub enum SyncMode {
    /// Shared playout clock — the default for live playback.
    ///
    /// Video frames are gated by [`crate::sync::Sync::wait`], which
    /// accounts for network jitter and codec latency to keep audio and
    /// video aligned.
    #[default]
    #[display("Synced")]
    Synced,

    /// No synchronization — frames are rendered as decoded.
    ///
    /// Uses PTS-cadence pacing in the video decode loop. Suitable for
    /// tests, file playback, and single-track scenarios.
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

    /// Maximum span of buffered media before skipping forward to the
    /// live edge. Passed to Hang's ordered consumer as `max_latency`.
    ///
    /// Increase for more continuity through congestion. Decrease for
    /// faster recovery after a stall. The JS equivalent is the
    /// `latency` parameter on the container consumer.
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
