use std::{
    thread,
    time::{Duration, Instant},
};

use byte_unit::{Bit, UnitType};
use iroh::{
    SecretKey,
    endpoint::{Connection, ConnectionStats, PathInfoList},
};
use moq_media::net::NetworkSignals;
use n0_watcher::Watcher;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Loads the iroh secret key from the `IROH_SECRET` environment variable,
/// or generates a new one and prints reuse instructions.
///
/// This pattern is shared across examples and applications that need a
/// stable endpoint identity across restarts.
pub fn secret_key_from_env() -> n0_error::Result<SecretKey> {
    Ok(match std::env::var("IROH_SECRET") {
        Ok(key) => key.parse()?,
        Err(_) => {
            let key = SecretKey::generate(&mut rand::rng());
            tracing::info!(
                "Generated new secret key. Reuse with IROH_SECRET={}",
                data_encoding::HEXLOWER.encode(&key.to_bytes())
            );
            key
        }
    })
}

/// Spawn a named OS thread and panic if spawning fails.
pub fn spawn_thread<F, T>(name: impl ToString, f: F) -> thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let name_str = name.to_string();
    thread::Builder::new()
        .name(name_str.clone())
        .spawn(f)
        .unwrap_or_else(|_| panic!("failed to spawn thread: {}", name_str))
}

#[derive(Debug)]
pub struct StatsSmoother {
    rate_up: Rate,
    rate_down: Rate,
    last_update: Instant,
    rtt: Duration,
}

impl Default for StatsSmoother {
    fn default() -> Self {
        Self::new()
    }
}

impl StatsSmoother {
    pub fn new() -> Self {
        Self {
            rate_up: Default::default(),
            rate_down: Default::default(),
            last_update: Instant::now(),
            rtt: Duration::from_secs(0),
        }
    }
    pub fn smoothed(
        &mut self,
        total: impl FnOnce() -> (ConnectionStats, PathInfoList),
    ) -> SmoothedStats<'_> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update);
        if elapsed >= Duration::from_secs(1) {
            let (stats, paths) = (total)();
            self.rate_down.update(elapsed, stats.udp_rx.bytes);
            self.rate_up.update(elapsed, stats.udp_tx.bytes);
            self.last_update = now;
            self.rtt = paths
                .iter()
                .find(|p| p.is_selected())
                .and_then(|p| p.rtt())
                .unwrap_or_default();
        }
        SmoothedStats {
            down: &self.rate_down,
            up: &self.rate_up,
            rtt: self.rtt,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Rate {
    /// Total bytes
    pub total: u64,
    /// Rate in bytes per second
    pub rate: f32,
    /// Rate rendered as a string
    pub rate_str: String,
}

impl Rate {
    fn update(&mut self, delta_time: Duration, new_total: u64) {
        let delta = new_total.saturating_sub(self.total);
        let delta_secs = delta_time.as_secs_f32();
        let rate = if delta_secs > 0.0 && delta > 0 {
            (delta as f32 * 8.0) / delta_secs
        } else {
            0.0
        };
        let bit = Bit::from_f32(rate).unwrap_or_default();
        let adjusted = bit.get_appropriate_unit(UnitType::Decimal);
        self.rate = rate;
        self.rate_str = format!("{adjusted:.2}/s");
        self.total = new_total;
    }
}

#[derive(Debug)]
pub struct SmoothedStats<'a> {
    pub rtt: Duration,
    pub down: &'a Rate,
    pub up: &'a Rate,
}

/// Spawns a background task that polls connection stats and produces
/// [`NetworkSignals`] for adaptive rendition selection.
///
/// The task runs until `shutdown` is cancelled or the connection closes.
/// Returns a `watch::Receiver<NetworkSignals>` that the caller can pass
/// to [`RemoteBroadcast::adaptive_video()`](moq_media::subscribe::RemoteBroadcast::adaptive_video).
pub fn spawn_signal_producer(
    conn: &Connection,
    shutdown: CancellationToken,
) -> watch::Receiver<NetworkSignals> {
    let (tx, rx) = watch::channel(NetworkSignals::default());
    let conn = conn.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prev_lost: u64 = 0;
        let mut prev_sent: u64 = 0;
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.cancelled() => break,
            }

            let paths = conn.paths().get();
            let Some(selected) = paths.iter().find(|p| p.is_selected()) else {
                continue;
            };

            let Some(stats) = selected.stats() else {
                continue;
            };
            let Some(rtt) = selected.rtt() else {
                continue;
            };

            // Delta-based loss rate.
            let total_lost = stats.lost_packets;
            let total_sent = stats.udp_tx.datagrams;
            let delta_lost = total_lost.saturating_sub(prev_lost);
            let delta_sent = total_sent.saturating_sub(prev_sent);
            prev_lost = total_lost;
            prev_sent = total_sent;

            let loss_rate = if delta_sent + delta_lost > 0 {
                delta_lost as f64 / (delta_sent + delta_lost) as f64
            } else {
                0.0
            };

            // Available bandwidth estimate from congestion window.
            let available_bps = if rtt.as_nanos() > 0 {
                (stats.cwnd as u128 * 8 * 1_000_000_000 / rtt.as_nanos()) as u64
            } else {
                0
            };

            let signals = NetworkSignals {
                rtt,
                loss_rate,
                available_bps,
                congestion_events: stats.congestion_events,
            };

            if tx.send(signals).is_err() {
                break; // all receivers dropped
            }
        }
    });
    rx
}

/// Spawns a background task that records connection stats into a
/// [`MetricsCollector`] for overlay display.
///
/// Records RTT, loss rate, and bandwidth estimates every 200ms.
/// Complements `spawn_signal_producer` — that one feeds adaptive
/// bitrate, this one feeds the debug overlay.
pub fn spawn_stats_recorder(
    conn: &Connection,
    metrics: moq_media::stats::MetricsCollector,
    shutdown: CancellationToken,
) {
    use moq_media::stats::*;
    let conn = conn.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prev_rx_bytes: u64 = 0;
        let mut prev_tx_bytes: u64 = 0;
        let mut prev_time = std::time::Instant::now();
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.cancelled() => break,
            }

            let paths = conn.paths().get();
            let Some(selected) = paths.iter().find(|p| p.is_selected()) else {
                continue;
            };
            let Some(stats) = selected.stats() else {
                continue;
            };
            let Some(rtt) = selected.rtt() else {
                continue;
            };

            metrics.record(NET_RTT_MS, rtt.as_secs_f64() * 1000.0);

            // Loss rate (same calculation as spawn_signal_producer).
            let total_lost = stats.lost_packets;
            let total_sent = stats.udp_tx.datagrams;
            if total_sent + total_lost > 0 {
                let loss = total_lost as f64 / (total_sent + total_lost) as f64 * 100.0;
                metrics.record(NET_LOSS_PCT, loss);
            }

            // Bandwidth from byte deltas.
            let now = std::time::Instant::now();
            let dt = now.duration_since(prev_time).as_secs_f64();
            if dt > 0.0 {
                let rx = stats.udp_rx.bytes;
                let tx = stats.udp_tx.bytes;
                let down_mbps = (rx.saturating_sub(prev_rx_bytes)) as f64 * 8.0 / dt / 1_000_000.0;
                let up_mbps = (tx.saturating_sub(prev_tx_bytes)) as f64 * 8.0 / dt / 1_000_000.0;
                metrics.record(NET_BW_DOWN_MBPS, down_mbps);
                metrics.record(NET_BW_UP_MBPS, up_mbps);
                prev_rx_bytes = rx;
                prev_tx_bytes = tx;
                prev_time = now;
            }
        }
    });
}
