use std::{thread, time::Duration};

use iroh::{SecretKey, endpoint::Connection};
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
/// [`NetStats`] for overlay display.
///
/// Records RTT, loss rate, and bandwidth estimates every 200ms.
/// The task runs until `shutdown` is cancelled. Callers should pass
/// the broadcast's shutdown token so the task stops when the
/// broadcast is dropped.
pub fn spawn_stats_recorder(
    conn: &Connection,
    net: moq_media::stats::NetStats,
    shutdown: CancellationToken,
) {
    let conn = conn.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prev_rx_bytes: u64 = 0;
        let mut prev_tx_bytes: u64 = 0;
        let mut prev_lost: u64 = 0;
        let mut prev_sent: u64 = 0;
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

            let rtt_ms = rtt.as_secs_f64() * 1000.0;
            net.rtt_ms.record(rtt_ms);

            // Path type and address labels.
            let path_type = if selected.is_relay() {
                "relayed"
            } else {
                "direct"
            };
            net.path_type.set(path_type);
            net.path_addr.set(format!("{:?}", selected.remote_addr()));

            // Path counts.
            let active = paths.iter().filter(|p| !p.is_closed()).count();
            net.paths_active.record(active as f64);

            // Delta-based loss rate (recent interval, not session-lifetime).
            let total_lost = stats.lost_packets;
            let total_sent = stats.udp_tx.datagrams;
            let delta_lost = total_lost.saturating_sub(prev_lost);
            let delta_sent = total_sent.saturating_sub(prev_sent);
            prev_lost = total_lost;
            prev_sent = total_sent;
            if delta_sent + delta_lost > 0 {
                let loss = delta_lost as f64 / (delta_sent + delta_lost) as f64 * 100.0;
                net.loss_pct.record(loss);
            }

            // Bandwidth from byte deltas.
            let now = std::time::Instant::now();
            let dt = now.duration_since(prev_time).as_secs_f64();
            if dt > 0.0 {
                let rx = stats.udp_rx.bytes;
                let tx = stats.udp_tx.bytes;
                let down_mbps = (rx.saturating_sub(prev_rx_bytes)) as f64 * 8.0 / dt / 1_000_000.0;
                let up_mbps = (tx.saturating_sub(prev_tx_bytes)) as f64 * 8.0 / dt / 1_000_000.0;
                net.bw_down_mbps.record(down_mbps);
                net.bw_up_mbps.record(up_mbps);
                prev_rx_bytes = rx;
                prev_tx_bytes = tx;
                prev_time = now;
            }
        }
    });
}
