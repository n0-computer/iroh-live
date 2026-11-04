//! Link measurement, from a MoQ session's statistics and its connection's paths.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use iroh::endpoint::{Connection, PathId};
use moq_net::session::Stats;

/// How often [`watch`] reads the session.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// The span loss and goodput are measured across.
///
/// A subscriber sends little but acknowledgements, a few per tick, so over a
/// shorter span one lost acknowledgement reads as a large loss rate.
const WINDOW: Duration = Duration::from_secs(2);

/// The fewest packets a window needs for a loss rate.
///
/// Below it the rate is zero.
const MIN_PACKETS: u64 = 20;

/// The span the round trip minimum is taken across.
///
/// Long enough to outlast a queue, short enough to forget a baseline that
/// moved without a path change.
const MIN_RTT_WINDOW: Duration = Duration::from_secs(15);

/// One reading of a link.
///
/// Measured by this endpoint, so the loss rate is of the packets it sends: on
/// a subscriber, mostly acknowledgements. `None` means not measured yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinkSample {
    /// The smoothed round trip time.
    pub rtt: Option<Duration>,
    /// The smallest smoothed round trip time on this path in the last 15 s.
    pub min_rtt: Option<Duration>,
    /// The share of packets sent in the last two seconds that were lost.
    ///
    /// The rate is in `0.0..=1.0`.
    pub loss_rate: Option<f64>,
    /// The rate UDP bytes arrived at over the last two seconds, in bits/s.
    ///
    /// The count includes packet overhead and duplicates.
    pub goodput_bps: Option<u64>,
    /// The peer's estimate of the path to this endpoint, in bits per second.
    pub delivery_bps: Option<u64>,
    /// Bumped whenever the link moves to another path or relay session.
    pub path_generation: u64,
    /// Whether a direct session's path goes through an iroh relay server.
    pub relayed: bool,
}

/// Which path a reading is of: a QUIC path, or one session of a relay link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKey {
    Quic(PathId),
    Session(u64),
}

/// Samples `session` every [`SAMPLE_INTERVAL`] until `connection` closes.
///
/// Each sample goes to `publish`. A sampler that outlives the call carries the
/// path generation on across the sessions of one link. Holding the session's
/// bandwidth estimate is what asks the peer to probe.
///
/// The future is cancel-safe: dropping it only stops the sampling.
pub async fn watch(
    session: moq_net::Session,
    connection: Connection,
    sampler: Arc<Mutex<LinkSampler>>,
    mut publish: impl FnMut(LinkSample),
) {
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let recv = session.recv_bandwidth();
    loop {
        interval.tick().await;
        if connection.close_reason().is_some() {
            return;
        }
        let delivery = recv.as_ref().and_then(|recv| recv.peek());
        let sample = sampler.lock().expect("poisoned").sample_connection(
            &session,
            &connection,
            delivery,
            Instant::now(),
        );
        if let Some(sample) = sample {
            publish(sample);
        }
    }
}

/// The counters one reading carries, and when it was taken.
#[derive(Debug, Clone, Copy)]
struct Reading {
    at: Instant,
    rtt: Option<Duration>,
    received_bytes: u64,
    sent_packets: u64,
    lost_packets: u64,
}

/// Turns readings of a session's statistics into [`LinkSample`]s.
///
/// Keeps the history rates and minimums are taken over, and the path
/// generation. [`watch`] drives one on a timer. Call the `sample_*` methods
/// directly to read on your own schedule.
#[derive(Debug, Default)]
pub struct LinkSampler {
    path: Option<PathKey>,
    path_generation: u64,
    /// This path's readings over the last [`MIN_RTT_WINDOW`], oldest first.
    readings: VecDeque<Reading>,
}

impl LinkSampler {
    /// Starts a new session of the link.
    ///
    /// The next reading counts as a new path even if the QUIC path id repeats,
    /// and history starts over.
    pub fn new_session(&mut self) {
        if self.path.take().is_some() {
            self.path_generation += 1;
        }
        self.readings.clear();
    }

    /// Returns the generation the link is at.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.path_generation
    }

    /// Reads a direct session and the connection whose selected path it rides.
    ///
    /// `delivery` is the peer's bandwidth estimate, as `peek` returns it.
    /// Returns `None` while the connection has no selected path.
    pub fn sample_connection(
        &mut self,
        session: &moq_net::Session,
        connection: &Connection,
        delivery: Option<moq_net::bandwidth::Rate>,
        now: Instant,
    ) -> Option<LinkSample> {
        let paths = connection.paths();
        let selected = paths.iter().find(|path| path.is_selected())?;
        let mut sample = self.sample(
            PathKey::Quic(selected.id()),
            &session.stats(),
            delivery,
            now,
        );
        sample.relayed = selected.is_relay();
        Some(sample)
    }

    /// Reads `stats` of session number `session` of a link that reconnects.
    ///
    /// A new session number starts a new path, as a new QUIC path does.
    pub fn sample_session(&mut self, session: u64, stats: &Stats, now: Instant) -> LinkSample {
        self.sample(PathKey::Session(session), stats, None, now)
    }

    /// Folds `stats`, read on `path` at `now`, into a sample.
    ///
    /// A new path starts its history over, so no rate or minimum mixes two
    /// paths.
    fn sample(
        &mut self,
        path: PathKey,
        stats: &Stats,
        recv: Option<moq_net::bandwidth::Rate>,
        now: Instant,
    ) -> LinkSample {
        if self.path != Some(path) {
            if self.path.is_some() {
                self.path_generation += 1;
            }
            self.path = Some(path);
            self.readings.clear();
        }
        let latest = Reading {
            at: now,
            rtt: stats.rtt.filter(|rtt| !rtt.is_zero()),
            received_bytes: stats.bytes_received.unwrap_or(0),
            sent_packets: stats.packets_sent.unwrap_or(0),
            lost_packets: stats.packets_lost.unwrap_or(0),
        };
        self.readings.push_back(latest);
        while self
            .readings
            .front()
            .is_some_and(|first| now.duration_since(first.at) > MIN_RTT_WINDOW)
        {
            self.readings.pop_front();
        }
        // The newest reading at least a window old, so a rate spans it all.
        let base = self
            .readings
            .iter()
            .rev()
            .find(|reading| now.duration_since(reading.at) >= WINDOW);
        LinkSample {
            rtt: latest.rtt,
            min_rtt: self.readings.iter().filter_map(|reading| reading.rtt).min(),
            loss_rate: base.map(|base| {
                // The sent count includes the packets later declared lost.
                let lost = latest.lost_packets.saturating_sub(base.lost_packets);
                let sent = latest.sent_packets.saturating_sub(base.sent_packets);
                match sent {
                    sent if sent < MIN_PACKETS => 0.0,
                    sent => (lost as f64 / sent as f64).min(1.0),
                }
            }),
            goodput_bps: base.map(|base| {
                let bytes = latest.received_bytes.saturating_sub(base.received_bytes);
                (bytes as f64 * 8.0 / now.duration_since(base.at).as_secs_f64()) as u64
            }),
            delivery_bps: recv.map(moq_net::bandwidth::Rate::as_bps),
            path_generation: self.path_generation,
            relayed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECT: PathKey = PathKey::Quic(PathId::ZERO);
    const BACKUP: PathKey = PathKey::Quic(PathId::MAX);

    /// Returns session statistics with `rtt` in milliseconds and the counters.
    fn stats(rtt_ms: u64, lost: u64, sent: u64, received: u64) -> Stats {
        let mut stats = Stats::default();
        stats.rtt = Some(Duration::from_millis(rtt_ms));
        stats.packets_lost = Some(lost);
        stats.packets_sent = Some(sent);
        stats.bytes_received = Some(received);
        stats
    }

    /// Feeds `ticks` readings that grow by `per_tick`, and returns the last sample.
    fn run(
        sampler: &mut LinkSampler,
        t0: Instant,
        ticks: u32,
        per_tick: (u64, u64, u64),
    ) -> LinkSample {
        let (sent, lost, received) = per_tick;
        let mut last = LinkSample::default();
        for tick in 1..=ticks {
            let n = u64::from(tick);
            last = sampler.sample(
                DIRECT,
                &stats(20, n * lost, n * sent, n * received),
                None,
                t0 + tick * SAMPLE_INTERVAL,
            );
        }
        last
    }

    #[test]
    fn rates_need_a_full_window() {
        let mut sampler = LinkSampler::default();
        let t0 = Instant::now();
        let early = run(&mut sampler, t0, 5, (20, 2, 20_000));
        assert_eq!(early.loss_rate, None);
        assert_eq!(early.goodput_bps, None);
        let full = run(&mut sampler, t0, 12, (20, 2, 20_000));
        let loss = full.loss_rate.expect("a loss rate");
        assert!((loss - 2.0 / 20.0).abs() < 0.01, "{full:?}");
        let bps = full.goodput_bps.expect("a goodput");
        assert!((700_000..=900_000).contains(&bps), "{bps}");
    }

    #[test]
    fn a_lost_packet_among_a_handful_is_not_loss() {
        let mut sampler = LinkSampler::default();
        let full = run(&mut sampler, Instant::now(), 12, (1, 0, 0));
        assert_eq!(full.loss_rate, Some(0.0), "{full:?}");
    }

    /// A new path starts over: generation, baseline and rate windows.
    #[test]
    fn a_new_path_starts_its_history_over() {
        let mut sampler = LinkSampler::default();
        let t0 = Instant::now();
        run(&mut sampler, t0, 12, (10, 5, 0));
        let later = t0 + 13 * SAMPLE_INTERVAL;
        let backup = sampler.sample(BACKUP, &stats(40, 300, 400, 0), None, later);
        assert_eq!(backup.path_generation, 1);
        assert_eq!(backup.min_rtt, Some(Duration::from_millis(40)));
        assert_eq!(backup.loss_rate, None);
    }

    /// A redial is a new path even when the QUIC path id repeats.
    #[test]
    fn a_new_session_bumps_the_generation() {
        let mut sampler = LinkSampler::default();
        let t0 = Instant::now();
        run(&mut sampler, t0, 3, (1, 0, 0));
        assert_eq!(sampler.generation(), 0);
        sampler.new_session();
        let first = sampler.sample(DIRECT, &stats(20, 0, 0, 0), None, t0 + SAMPLE_INTERVAL * 4);
        assert_eq!(first.path_generation, 1);
        // A session that never sampled adds nothing.
        sampler.new_session();
        sampler.new_session();
        assert_eq!(sampler.generation(), 2);
    }

    #[test]
    fn the_minimum_forgets_an_old_path_delay() {
        let mut sampler = LinkSampler::default();
        let t0 = Instant::now();
        sampler.sample(DIRECT, &stats(2, 0, 0, 0), None, t0);
        let within = sampler.sample(DIRECT, &stats(60, 0, 0, 0), None, t0 + MIN_RTT_WINDOW);
        assert_eq!(within.min_rtt, Some(Duration::from_millis(2)));
        let past = sampler.sample(DIRECT, &stats(60, 0, 0, 0), None, t0 + MIN_RTT_WINDOW * 2);
        assert_eq!(past.min_rtt, Some(Duration::from_millis(60)));
    }
}
