//! Pieces an application needs around the transport, rather than in it.
//!
//! The endpoint identity to bind with, the local-network address lookup a short
//! ticket leans on, and the two background samplers that turn a QUIC
//! connection's path statistics into something a caller can act on:
//! [`NetworkSignals`] for the adaptation loop, and
//! [`NetStats`](moq_media::stats::NetStats) for a user interface to draw.
//! [`Live::subscribe`](crate::Live::subscribe) and [`Call`](crate::Call) wire
//! both samplers up already; reach for them directly only when the session and
//! the broadcast came from somewhere else.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use iroh::{
    SecretKey,
    endpoint::{Builder as EndpointBuilder, Connection, PathId, PathStats},
};
use moq_media::net::NetworkSignals;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

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

/// Whether an endpoint answers for itself over mDNS, or only looks others up.
///
/// The distinction matters to a node that nobody is meant to dial: `irl` run
/// with `--no-serve` accepts no sessions, so announcing it on the local network
/// would advertise an endpoint that refuses every connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanPresence {
    /// Announce this endpoint and answer mDNS queries for it, so peers on the
    /// same network can resolve its id without leaving the network.
    Announce,
    /// Resolve other endpoints without publishing anything about this one.
    LookupOnly,
}

impl LanPresence {
    /// Returns the presence an endpoint that accepts sessions when `serve`
    /// holds should take on the local network.
    pub fn serving(serve: bool) -> Self {
        if serve {
            Self::Announce
        } else {
            Self::LookupOnly
        }
    }

    fn announces(self) -> bool {
        matches!(self, Self::Announce)
    }
}

/// Adds mDNS address lookup to `builder`.
///
/// A ticket names an endpoint id and no addresses, which leaves two ways to
/// turn that id into somewhere to send packets. Pkarr and DNS cover the case
/// where both ends have internet. mDNS covers the case where neither does: two
/// laptops on a conference network with no route out still find each other,
/// because the lookup never leaves the link. Between them they do the job the
/// addresses in a ticket used to do, and they do it with addresses that are
/// current rather than however old the ticket is.
///
/// Not fallible: mDNS wants a multicast socket, and a sandbox or a phone
/// without a multicast lock will not give it one. That costs local-network
/// lookup and nothing else, so a failure is logged and the endpoint binds
/// without it.
pub async fn with_mdns(builder: EndpointBuilder, presence: LanPresence) -> EndpointBuilder {
    let options = iroh_mdns_peer_lookup::Options::new()
        .announce(presence.announces())
        .build();
    match iroh_mdns_peer_lookup::lookup(options).await {
        Ok(lookup) => {
            debug!(announce = presence.announces(), "mDNS address lookup ready");
            builder.address_lookup(lookup)
        }
        Err(err) => {
            warn!(
                error = %err,
                "could not start mDNS address lookup; peers on this network will \
                 only be found through pkarr and DNS, which need internet"
            );
            builder
        }
    }
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
#[derive(Debug)]
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
#[derive(Debug)]
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
        let mut interval = tokio::time::interval(SIGNAL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sampler = Sampler::default();
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
            let signals = sampler.sample(selected.id(), &selected.stats(), Instant::now());

            // Five a second is too much for anything but a trace, and it is
            // exactly what is wanted when an adaptation decision has to be
            // explained after the fact.
            trace!(
                rtt_ms = signals.rtt.as_millis() as u64,
                rtt_samples = signals.rtt_samples,
                min_rtt_ms = signals.min_rtt.as_millis() as u64,
                loss_rate = signals.loss_rate,
                goodput_kbps = ?signals.goodput_bps.map(|bps| bps / 1000),
                congestion_events = signals.congestion_events,
                "network signals",
            );

            if tx.send(signals).is_err() {
                break; // all receivers dropped
            }
        }
    });
    rx
}

/// How often the signal producer reads the path.
const SIGNAL_INTERVAL: Duration = Duration::from_millis(200);

/// Turns one reading of a path's statistics into a [`NetworkSignals`].
///
/// Everything a signal needs that one reading cannot give lives here: the
/// counters from the previous reading, for the loss delta; the round trip
/// minimum and the path it was taken on; how many distinct round trip samples
/// have been seen; and the window of received byte counts the goodput is
/// measured across. Kept apart from the task that drives it so the arithmetic
/// can be tested against readings made up for the purpose, with no connection
/// under it.
#[derive(Debug, Default)]
struct Sampler {
    prev_lost: u64,
    prev_sent: u64,
    /// The path's round trip with nothing queued in front of it. Tracked here
    /// rather than read from the transport because QUIC keeps no such figure,
    /// and taken as a minimum because that is what an unqueued sample looks
    /// like.
    baseline: Option<PathBaseline>,
    /// The last round trip read out, so that repeats of it can be told from
    /// fresh samples.
    prev_rtt: Option<Duration>,
    rtt_samples: u64,
    /// Timestamped `udp_rx.bytes` readings, oldest first, spanning
    /// `GOODPUT_WINDOW`. Goodput is the difference across the whole span
    /// rather than between the last two, so one bursty tick does not move it.
    received: VecDeque<(Instant, u64)>,
}

impl Sampler {
    /// Folds a reading of `stats`, taken on `path` at `now`, into the signals.
    fn sample(&mut self, path: PathId, stats: &PathStats, now: Instant) -> NetworkSignals {
        let rtt = stats.rtt;

        // A minimum measured on one path says nothing about the next: a
        // connection that fell back to a relay is on a longer link, not on a
        // queue in front of the one it had.
        let min_rtt = match &mut self.baseline {
            Some(known) if known.path == path => known.min_rtt.record(rtt, now),
            _ => {
                self.baseline = Some(PathBaseline {
                    path,
                    min_rtt: WindowedMin::new(MIN_RTT_WINDOW, rtt, now),
                });
                rtt
            }
        };

        // QUIC updates its round trip estimate only when an acknowledgement
        // brings it a new sample, and a subscriber sends few packets that ask
        // to be acknowledged, so the same figure is read out over and over.
        // Counting the changes lets a consumer weigh a fresh reading
        // differently from a repeat of the last one.
        if self.prev_rtt != Some(rtt) {
            self.prev_rtt = Some(rtt);
            self.rtt_samples += 1;
        }

        // Delta-based loss rate. This counts what this endpoint sent and failed
        // to get acknowledged, so on a subscriber it is loss among
        // acknowledgements; see `NetworkSignals::loss_rate` for what that is
        // and is not worth.
        let delta_lost = stats.lost_packets.saturating_sub(self.prev_lost);
        let delta_sent = stats.udp_tx.datagrams.saturating_sub(self.prev_sent);
        self.prev_lost = stats.lost_packets;
        self.prev_sent = stats.udp_tx.datagrams;
        let loss_rate = match delta_sent + delta_lost {
            0 => 0.0,
            total => delta_lost as f64 / total as f64,
        };

        // Downlink goodput from received bytes. The congestion window this
        // would otherwise be derived from belongs to the sending direction, and
        // a subscriber that sends only acknowledgements keeps a window that
        // measures how little it sends; bytes arriving are the one thing a
        // receiver can measure about the direction it cares about.
        self.received.push_back((now, stats.udp_rx.bytes));
        // Keep the newest sample from before the window opens, so the
        // difference spans the whole window rather than stopping short of it.
        while self.received.len() > 2 && now.duration_since(self.received[1].0) >= GOODPUT_WINDOW {
            self.received.pop_front();
        }

        NetworkSignals {
            rtt,
            rtt_samples: self.rtt_samples,
            min_rtt,
            loss_rate,
            goodput_bps: goodput(&self.received),
            congestion_events: stats.congestion_events,
        }
    }
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

    /// A reading with `rtt`, `lost` packets, `sent` datagrams and `received`
    /// bytes on the counters, which is what one tick of the producer sees.
    fn reading(rtt_ms: u64, lost: u64, sent: u64, received: u64) -> PathStats {
        let mut stats = PathStats::default();
        stats.rtt = Duration::from_millis(rtt_ms);
        stats.lost_packets = lost;
        stats.udp_tx.datagrams = sent;
        stats.udp_rx.bytes = received;
        stats
    }

    /// Loss is the change since the last reading, not the connection's total:
    /// a hundred packets lost an hour ago say nothing about the link now.
    #[test]
    fn loss_is_measured_between_readings() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let first = sampler.sample(PathId::ZERO, &reading(20, 100, 1000, 0), t0);
        assert!(
            first.loss_rate > 0.09,
            "the first reading counts from zero: {first:?}"
        );

        // Ten more sent, none lost since.
        let quiet = sampler.sample(
            PathId::ZERO,
            &reading(20, 100, 1010, 0),
            t0 + SIGNAL_INTERVAL,
        );
        assert_eq!(quiet.loss_rate, 0.0);

        // Ten more sent, ten lost: half of what moved.
        let bad = sampler.sample(
            PathId::ZERO,
            &reading(20, 110, 1020, 0),
            t0 + 2 * SIGNAL_INTERVAL,
        );
        assert!((bad.loss_rate - 0.5).abs() < 1e-9, "{bad:?}");
    }

    /// QUIC hands the same round trip out until it takes another sample, so a
    /// repeat is not a new reading and is not counted as one.
    #[test]
    fn a_repeated_round_trip_is_one_sample() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let mut samples = 0;
        for tick in 0..5u32 {
            let at = t0 + tick * SIGNAL_INTERVAL;
            samples = sampler
                .sample(PathId::ZERO, &reading(20, 0, 0, 0), at)
                .rtt_samples;
        }
        assert_eq!(samples, 1, "five ticks of one figure are one sample");
        let fresh = sampler.sample(
            PathId::ZERO,
            &reading(25, 0, 0, 0),
            t0 + 5 * SIGNAL_INTERVAL,
        );
        assert_eq!(fresh.rtt_samples, 2);
    }

    /// Goodput is bytes across the whole window, and is nothing until the
    /// readings span it: a rate from two ticks is a rate from one burst.
    #[test]
    fn goodput_needs_a_full_window() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        // 100 kB every 200ms is 4 Mbit/s.
        let mut last = None;
        for tick in 0..=5u32 {
            let at = t0 + tick * SIGNAL_INTERVAL;
            let bytes = u64::from(tick) * 100_000;
            last = sampler
                .sample(PathId::ZERO, &reading(20, 0, 0, bytes), at)
                .goodput_bps;
            if at.duration_since(t0) < GOODPUT_WINDOW {
                assert_eq!(
                    last, None,
                    "no figure before the window spans, at tick {tick}"
                );
            }
        }
        let bps = last.expect("a second of readings has a rate");
        assert!(
            (3_500_000..=4_500_000).contains(&bps),
            "expected about 4 Mbit/s, got {bps}"
        );
    }

    /// A round trip minimum measured on one path says nothing about the next:
    /// a connection that fell back to a relay is on a longer link, not behind a
    /// queue on the one it had.
    #[test]
    fn the_round_trip_baseline_starts_over_on_a_new_path() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let direct = sampler.sample(PathId::ZERO, &reading(2, 0, 0, 0), t0);
        assert_eq!(direct.min_rtt, Duration::from_millis(2));

        // Same path, longer reading: the minimum holds, and this reads as a queue.
        let queued = sampler.sample(PathId::ZERO, &reading(40, 0, 0, 0), t0 + SIGNAL_INTERVAL);
        assert_eq!(queued.min_rtt, Duration::from_millis(2));

        // A different path at 40ms is a 40ms path, not a 2ms path with a queue.
        let relayed = sampler.sample(PathId::MAX, &reading(40, 0, 0, 0), t0 + 2 * SIGNAL_INTERVAL);
        assert_eq!(relayed.min_rtt, Duration::from_millis(40));
    }

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
