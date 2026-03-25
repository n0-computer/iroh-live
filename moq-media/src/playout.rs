//! Playback policy for subscribed broadcasts.
//!
//! - [`SyncMode`] controls how audio and video are aligned at playout time.
//!   Currently unused (sync is disabled), but preserved for the public API.
//! - [`FreshnessPolicy`] controls how much stale media we keep after a stall
//!   before skipping forward. This drives Hang's ordered consumer.

use std::time::Duration;

/// A/V synchronization behavior at playout time.
///
/// Currently unused — both audio and video play independently with PTS-based
/// pacing. Will be re-enabled when A/V sync is re-added.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SyncMode {
    /// No synchronization — frames are rendered as decoded.
    #[default]
    Unmanaged,
}

impl SyncMode {
    // /// Creates [`SyncMode::AudioMaster`] with a 150 ms video hold budget.
    // pub fn audio_master() -> Self {
    //     Self::AudioMaster {
    //         video_hold_budget: Duration::from_millis(150),
    //     }
    // }

    // /// Creates [`SyncMode::AudioMaster`] with an explicit hold budget.
    // pub fn audio_master_with_hold(video_hold_budget: Duration) -> Self {
    //     Self::AudioMaster { video_hold_budget }
    // }

    /// Creates [`SyncMode::Unmanaged`].
    pub fn unmanaged() -> Self {
        Self::Unmanaged
    }
}

/// Freshness policy for ordered media delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreshnessPolicy {
    /// Maximum duration of stale media we keep before skipping forward.
    /// Passed to Hang's ordered consumer as `max_latency`.
    pub max_stale_duration: Duration,
}

impl Default for FreshnessPolicy {
    fn default() -> Self {
        Self {
            max_stale_duration: Duration::from_millis(150),
        }
    }
}

impl FreshnessPolicy {
    /// Creates a freshness policy with the given maximum stale duration.
    pub fn new(max_stale_duration: Duration) -> Self {
        Self { max_stale_duration }
    }
}

/// Public playback policy for a subscribed broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackPolicy {
    /// Cross-track synchronization policy.
    pub sync: SyncMode,
    /// Staleness policy for ordered delivery and decode recovery.
    pub freshness: FreshnessPolicy,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self::unmanaged()
    }
}

impl PlaybackPolicy {
    // /// Default interactive policy: audio-master sync with 150 ms hold, 500 ms freshness.
    // pub fn audio_master() -> Self {
    //     Self {
    //         sync: SyncMode::audio_master(),
    //         freshness: FreshnessPolicy::default(),
    //     }
    // }

    // /// Audio-master with explicit hold budget.
    // pub fn audio_master_with_hold(video_hold_budget: Duration) -> Self {
    //     Self {
    //         sync: SyncMode::audio_master_with_hold(video_hold_budget),
    //         freshness: FreshnessPolicy::default(),
    //     }
    // }

    /// Unmanaged — no sync, default freshness.
    pub fn unmanaged() -> Self {
        Self {
            sync: SyncMode::unmanaged(),
            freshness: FreshnessPolicy::default(),
        }
    }

    /// Returns a copy with a different freshness policy.
    pub fn with_freshness(mut self, freshness: FreshnessPolicy) -> Self {
        self.freshness = freshness;
        self
    }

    /// Returns a copy with a different sync mode.
    pub fn with_sync(mut self, sync: SyncMode) -> Self {
        self.sync = sync;
        self
    }
}
