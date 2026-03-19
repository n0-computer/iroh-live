use std::time::Duration;

/// Transport-level network quality signals for adaptive rendition selection.
///
/// Produced by polling QUIC connection stats. Consumed by
/// [`AdaptiveVideoTrack`](crate::adaptive::AdaptiveVideoTrack) to decide
/// when to switch renditions.
#[derive(Debug, Clone, Copy)]
pub struct NetworkSignals {
    /// Round-trip time to the remote peer.
    pub rtt: Duration,
    /// Recent packet loss rate in `0.0..=1.0`, computed over a 200ms delta window.
    pub loss_rate: f64,
    /// Estimated available bandwidth in bits per second (`cwnd * 8 / rtt`).
    pub available_bps: u64,
    /// Monotonically increasing congestion event counter.
    pub congestion_events: u64,
}

impl Default for NetworkSignals {
    fn default() -> Self {
        Self {
            rtt: Duration::ZERO,
            loss_rate: 0.0,
            available_bps: 0,
            congestion_events: 0,
        }
    }
}
