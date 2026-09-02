//! Playback policy for subscribed broadcasts.
//!
//! [`SyncMode`] decides whether the playout clock gates video against audio.
//! [`PlaybackPolicy::max_latency`] decides how much buffered media a subscriber
//! tolerates before skipping to the live edge, and is passed straight through
//! to `moq_video::decode::Config::latency_max`.
//! [`PlaybackPolicy::gpu_frames`] says what the subscriber will do with the
//! decoded frames, which decides where the decoder leaves them.

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

    /// Whether decoded frames should be left on the GPU rather than downloaded
    /// to CPU memory, passed to the decoder as `gpu_frames`.
    ///
    /// Set it when the frames go to a renderer: a hardware decoder that can
    /// share its decode surface then hands one over, and the picture reaches a
    /// texture without a round trip through system memory. Leave it clear for a
    /// subscriber that reads the pixels, such as one writing them to a file,
    /// since sharing a surface costs the decoder an allocation per picture and
    /// buys such a consumer nothing.
    ///
    /// Best effort: only backends that can do it honor it, and a frame that
    /// does come back on the GPU still converts to I420 on demand, so nothing
    /// downstream has to know which happened.
    pub gpu_frames: bool,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self {
            sync: SyncMode::default(),
            max_latency: Duration::from_millis(150),
            gpu_frames: false,
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

    /// Returns a copy that asks the decoder to leave frames on the GPU.
    ///
    /// See [`gpu_frames`](Self::gpu_frames) for what that costs a subscriber
    /// that does not draw them.
    #[must_use]
    pub fn with_gpu_frames(mut self, gpu_frames: bool) -> Self {
        self.gpu_frames = gpu_frames;
        self
    }
}
