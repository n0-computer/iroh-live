//! Pieces an application needs around the transport, rather than in it.
//!
//! The endpoint identity to bind with, and the two background samplers that
//! turn a QUIC connection's path statistics into something a caller can act on:
//! [`NetworkSignals`] for the adaptation loop, and
//! [`NetStats`](moq_media::stats::NetStats) for a user interface to draw.
//! [`Live::subscribe`](crate::Live::subscribe) and [`Call`](crate::Call) wire
//! both up already; reach for them directly only when the session and the
//! broadcast came from somewhere else.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use iroh::{
    SecretKey,
    endpoint::{Connection, PathId},
};
use moq_media::net::NetworkSignals;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, trace};

/// Loads the iroh secret key from the `IROH_SECRET` environment variable, or
/// generates one and logs how to keep it.
///
/// An endpoint's identity is its secret key, so a node that generates a fresh
/// one on every start is a different node to its peers every time, and every
/// ticket it ever handed out is stale. Applications that want a stable identity
/// across restarts read it from the environment through here.
///
/// # Errors
///
/// Fails if `IROH_SECRET` is set to something that is not a secret key.
pub fn secret_key_from_env() -> n0_error::Result<SecretKey> {
    Ok(match std::env::var("IROH_SECRET") {
        Ok(key) => key.parse()?,
        Err(_) => {
            let key = SecretKey::generate();
            info!(
                secret = %data_encoding::HEXLOWER.encode(&key.to_bytes()),
                "generated a secret key; reuse this identity with IROH_SECRET",
            );
            key
        }
    })
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

/// The span the round trip minimum is taken across.
///
/// The minimum is what makes the current round trip mean anything, and a running
/// minimum over the whole connection stops meaning anything as soon as the path
/// underneath it moves. A fallback from a direct path to a relay takes an iroh
/// connection from a couple of milliseconds to tens of them, and every later
/// reading then looks like a queue that will never drain. Long enough to outlast
/// a real queue, which drains in round trips rather than in minutes, and short
/// enough that a baseline which moved is forgotten inside a minute.
const MIN_RTT_WINDOW: Duration = Duration::from_secs(15);

/// The path a round trip minimum was measured on, and the minimum itself.
///
/// The two travel together because neither survives the other: a minimum from
/// the path before this one describes a different link.
struct PathBaseline {
    path: PathId,
    min_rtt: WindowedMin,
}

/// A minimum over a sliding span, kept as two tumbling halves.
///
/// Holds the minimum of everything recorded in the current half and of
/// everything in the half before it, retiring the older one wholesale when the
/// span elapses. That covers between one and two spans rather than exactly one,
/// which is what not keeping every sample costs, and it errs on the right side:
/// a minimum that expired early would read the path's own delay as a queue.
struct WindowedMin {
    span: Duration,
    current: Duration,
    previous: Option<Duration>,
    since: Instant,
}

impl WindowedMin {
    /// Starts a minimum over `span` at `first`.
    fn new(span: Duration, first: Duration, now: Instant) -> Self {
        Self {
            span,
            current: first,
            previous: None,
            since: now,
        }
    }

    /// Records `sample` and returns the minimum over the span.
    fn record(&mut self, sample: Duration, now: Instant) -> Duration {
        if now.duration_since(self.since) >= self.span {
            self.previous = Some(self.current);
            self.current = sample;
            self.since = now;
        } else {
            self.current = self.current.min(sample);
        }
        self.previous
            .map_or(self.current, |previous| previous.min(self.current))
    }
}

/// Spawns a background task that polls connection stats and produces
/// [`NetworkSignals`] for adaptive rendition selection.
///
/// The task runs until `shutdown` is cancelled, the connection closes, or
/// every receiver is dropped. Returns a `watch::Receiver<NetworkSignals>` that
/// the caller can pass to
/// [`VideoTrack::enable_adaptation`](moq_media::subscribe::VideoTrack::enable_adaptation).
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
        // The path's round trip with nothing queued in front of it. Tracked here
        // rather than read from the transport because QUIC keeps no such figure,
        // and taken as a minimum because that is what an unqueued sample looks
        // like.
        let mut baseline: Option<PathBaseline> = None;
        // The last round trip read out, so that repeats of it can be told from
        // fresh samples.
        let mut prev_rtt: Option<Duration> = None;
        let mut rtt_samples: u64 = 0;
        // Timestamped `udp_rx.bytes` readings, oldest first, spanning
        // `GOODPUT_WINDOW`. Goodput is the difference across the whole span
        // rather than between the last two, so one bursty tick does not move it.
        let mut received: VecDeque<(Instant, u64)> = VecDeque::new();
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.cancelled() => break,
                // A connection that has gone has no more stats to give, and a
                // subscription outlives it often enough to matter: the peer
                // vanishing does not drop the broadcast this token belongs to,
                // so without this the task samples a dead path five times a
                // second for as long as the application keeps the subscription.
                _ = conn.closed() => break,
            }

            let paths = conn.paths();
            let Some(selected) = paths.iter().find(|p| p.is_selected()) else {
                continue;
            };

            let stats = selected.stats();
            let rtt = stats.rtt;
            let now = Instant::now();

            // A minimum measured on one path says nothing about the next: a
            // connection that fell back to a relay is on a longer link, not on a
            // queue in front of the one it had.
            let path = selected.id();
            let min_rtt = match &mut baseline {
                Some(known) if known.path == path => known.min_rtt.record(rtt, now),
                _ => {
                    baseline = Some(PathBaseline {
                        path,
                        min_rtt: WindowedMin::new(MIN_RTT_WINDOW, rtt, now),
                    });
                    rtt
                }
            };

            // QUIC updates its round trip estimate only when an acknowledgement
            // brings it a new sample, and a subscriber sends few packets that
            // ask to be acknowledged, so the same figure is read out over and
            // over. Counting the changes lets a consumer weigh a fresh reading
            // differently from a repeat of the last one.
            if prev_rtt != Some(rtt) {
                prev_rtt = Some(rtt);
                rtt_samples += 1;
            }

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
            received.push_back((now, stats.udp_rx.bytes));
            // Keep the newest sample from before the window opens, so the
            // difference spans the whole window rather than stopping short of it.
            while received.len() > 2 && now.duration_since(received[1].0) >= GOODPUT_WINDOW {
                received.pop_front();
            }
            let goodput_bps = goodput(&received);

            let signals = NetworkSignals {
                rtt,
                rtt_samples,
                min_rtt,
                loss_rate,
                goodput_bps,
                congestion_events: stats.congestion_events,
            };
            // Five a second is too much for anything but a trace, and it is
            // exactly what is wanted when an adaptation decision has to be
            // explained after the fact.
            trace!(
                rtt_ms = rtt.as_millis() as u64,
                rtt_samples,
                min_rtt_ms = min_rtt.as_millis() as u64,
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
/// Records RTT, loss rate, and bandwidth estimates every 200ms. The task runs
/// until `shutdown` is cancelled or the connection closes. Callers should pass
/// the broadcast's shutdown token so the task stops when the broadcast is
/// dropped.
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
                _ = conn.closed() => break,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windowed_minimum_forgets_the_path_it_came_from() {
        // The failure this covers: an iroh connection that falls back from a
        // direct path to a relay goes from a couple of milliseconds to tens of
        // them and stays there. A minimum that never forgets the first figure
        // makes every later round trip read as a queue that will never drain.
        let span = Duration::from_secs(15);
        let start = Instant::now();
        let mut min = WindowedMin::new(span, Duration::from_millis(2), start);

        assert_eq!(
            min.record(Duration::from_millis(60), start),
            Duration::from_millis(2)
        );

        // Still inside the first half, so the direct path's figure stands.
        let same_half = start + span / 2;
        assert_eq!(
            min.record(Duration::from_millis(60), same_half),
            Duration::from_millis(2),
        );

        // One half retired, and the figure it held is now the older of the two
        // the minimum covers.
        let next_half = start + span;
        assert_eq!(
            min.record(Duration::from_millis(60), next_half),
            Duration::from_millis(2),
        );

        // Both halves now come from the relay, so the direct path is gone.
        let after = start + span * 2;
        assert_eq!(
            min.record(Duration::from_millis(60), after),
            Duration::from_millis(60),
        );
    }

    #[test]
    fn a_windowed_minimum_takes_a_dip_immediately() {
        // The other direction has to be instant: a round trip lower than
        // anything seen is the path telling us the queue drained, and waiting a
        // window to believe it would call a clear path congested for that long.
        let span = Duration::from_secs(15);
        let start = Instant::now();
        let mut min = WindowedMin::new(span, Duration::from_millis(60), start);

        assert_eq!(
            min.record(Duration::from_millis(3), start + span / 4),
            Duration::from_millis(3),
        );
    }
}
