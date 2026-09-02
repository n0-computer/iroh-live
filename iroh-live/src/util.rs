use std::{
    collections::VecDeque,
    thread,
    time::{Duration, Instant},
};

use iroh::{SecretKey, endpoint::Connection};
use moq_media::net::NetworkSignals;
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
            let key = SecretKey::generate();
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

/// The span the downlink goodput estimate is taken across.
///
/// A second, because one 200ms sample at video frame rates holds only a handful
/// of frames, and a keyframe landing in one of them doubles the reading.
const GOODPUT_WINDOW: Duration = Duration::from_secs(1);

/// The rate below which what arrives is taken for control traffic rather than
/// media, and goodput is reported as unmeasured.
///
/// A subscriber receiving nothing still receives something: acknowledgements of
/// its own acknowledgements, keep-alives and path probes, which measured on an
/// idle connection come to single-digit kbit/s. A ratio against a rendition's
/// bitrate would read that as a link that had all but failed, and a publisher
/// going quiet is not that. Kept low all the same, because the smallest rung on
/// a ladder is small: 320x240 video comes in under 100 kbit/s.
const GOODPUT_FLOOR_BPS: u64 = 16_000;

/// Spawns a background task that polls connection stats and produces
/// [`NetworkSignals`] for adaptive rendition selection.
///
/// The task runs until `shutdown` is cancelled or the connection closes.
/// Returns a `watch::Receiver<NetworkSignals>` that the caller can pass
/// to [`VideoTrack::enable_adaptation`](moq_media::subscribe::VideoTrack::enable_adaptation).
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
        // The path's round trip with nothing queued in front of it. Tracked
        // here rather than read from the transport because QUIC keeps no such
        // figure, and taken as a running minimum because that is what an
        // unqueued sample looks like.
        let mut min_rtt: Option<Duration> = None;
        // Timestamped `udp_rx.bytes` readings, oldest first, spanning
        // `GOODPUT_WINDOW`. Goodput is the difference across the whole span
        // rather than between the last two, so one bursty tick does not move it.
        let mut received: VecDeque<(Instant, u64)> = VecDeque::new();
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.cancelled() => break,
            }

            let paths = conn.paths();
            let Some(selected) = paths.iter().find(|p| p.is_selected()) else {
                continue;
            };

            let stats = selected.stats();
            let rtt = selected.rtt();
            let baseline = min_rtt.map_or(rtt, |seen| seen.min(rtt));
            min_rtt = Some(baseline);

            // Delta-based loss rate. This counts what this endpoint sent and
            // failed to get acknowledged, so on a subscriber it is loss among
            // acknowledgements; see `NetworkSignals::loss_rate` for what that
            // is and is not worth.
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

            // Downlink goodput from received bytes. The congestion window this
            // would otherwise be derived from belongs to the sending direction,
            // and a subscriber that sends only acknowledgements keeps a window
            // that measures how little it sends; bytes arriving are the one
            // thing a receiver can measure about the direction it cares about.
            let now = Instant::now();
            received.push_back((now, stats.udp_rx.bytes));
            // Keep the newest sample from before the window opens, so the
            // difference spans the whole window rather than stopping short of it.
            while received.len() > 2 && now.duration_since(received[1].0) >= GOODPUT_WINDOW {
                received.pop_front();
            }
            let goodput_bps = goodput(&received);

            let signals = NetworkSignals {
                rtt,
                min_rtt: baseline,
                loss_rate,
                goodput_bps,
                congestion_events: stats.congestion_events,
            };
            // Five a second is too much for anything but a trace, and it is
            // exactly what is wanted when an adaptation decision has to be
            // explained after the fact.
            tracing::trace!(
                rtt_ms = rtt.as_millis() as u64,
                min_rtt_ms = baseline.as_millis() as u64,
                loss_rate,
                goodput_kbps = ?goodput_bps.map(|bps| bps / 1000),
                congestion_events = stats.congestion_events,
                "network signals",
            );

            if tx.send(signals).is_err() {
                break; // all receivers dropped
            }
        }
    });
    rx
}

/// Returns the goodput across `samples` of received byte counts.
///
/// `None` until they span [`GOODPUT_WINDOW`], and `None` again once the rate
/// falls to [`GOODPUT_FLOOR_BPS`] or below, which means nothing that could be
/// media is arriving.
fn goodput(samples: &VecDeque<(Instant, u64)>) -> Option<u64> {
    let (oldest_at, oldest_bytes) = *samples.front()?;
    let (newest_at, newest_bytes) = *samples.back()?;
    let span = newest_at.duration_since(oldest_at);
    if span < GOODPUT_WINDOW {
        return None;
    }
    let bytes = newest_bytes.saturating_sub(oldest_bytes) as f64;
    let bps = (bytes * 8.0 / span.as_secs_f64()) as u64;
    (bps > GOODPUT_FLOOR_BPS).then_some(bps)
}

/// Spawns a background task that records connection stats into a
/// [`NetStats`](moq_media::stats::NetStats) for a UI to draw.
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
        let mut prev_time = Instant::now();
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.cancelled() => break,
            }

            let paths = conn.paths();
            let Some(selected) = paths.iter().find(|p| p.is_selected()) else {
                continue;
            };
            let stats = selected.stats();
            let rtt = selected.rtt();

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
            let active = paths.iter().count();
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
            let now = Instant::now();
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
