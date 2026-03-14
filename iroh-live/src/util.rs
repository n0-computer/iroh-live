use std::{
    thread,
    time::{Duration, Instant},
};

use byte_unit::{Bit, UnitType};
use iroh::endpoint::{Connection, ConnectionStats, PathInfoList};
use moq_media::net::NetworkSignals;
use n0_watcher::Watcher;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

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
                .map(|p| p.rtt())
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
        let bit = Bit::from_f32(rate).unwrap();
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

            let stats = selected.stats();
            let rtt = selected.rtt();

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
