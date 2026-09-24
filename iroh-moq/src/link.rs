//! What a session's connection says about the link under it.
//!
//! Every direct session runs one monitor that reads the selected path's QUIC
//! statistics a few times a second and keeps the latest [`LinkSample`], which
//! [`Session::link`](crate::Session::link) returns. The sample is plain data: the
//! media crate adapts it into whatever its rendition choice reads, and a user
//! interface draws it.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use iroh::endpoint::{Connection, PathId, PathStats};
use tokio_util::sync::CancellationToken;
use tracing::trace;

/// How often the monitor reads the path.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// The span the downlink goodput estimate is taken across.
///
/// A second, because one 200ms sample at video frame rates holds only a handful
/// of frames, and a keyframe landing in one of them doubles the reading.
const GOODPUT_WINDOW: Duration = Duration::from_secs(1);

/// The rate below which arriving traffic counts as control, not media.
///
/// Below it goodput is reported as unmeasured. A subscriber receiving nothing still receives something: acknowledgements of
/// its own acknowledgements, keep-alives and path probes, which measured on an
/// idle connection come to single-digit kbit/s. Kept low all the same, because
/// the smallest rung on a ladder is small: 320x240 video comes in under 100
/// kbit/s.
const GOODPUT_FLOOR_BPS: u64 = 16_000;

/// The span the loss rate is measured across, and its fewest packets.
///
/// A subscriber sends acknowledgements and little else: two to five packets in
/// a 200 ms tick at video rates. A loss rate over one tick is then a fraction
/// with a denominator of three, and one lost acknowledgement reads as a third
/// of everything lost. Two seconds holds enough packets for a single loss to
/// stay small, and the minimum keeps a quiet window from reporting a rate it
/// cannot resolve: below it the rate reads as zero.
const LOSS_WINDOW: Duration = Duration::from_secs(2);
const LOSS_MIN_PACKETS: u64 = 20;

/// The span the round trip minimum is taken across.
///
/// Long enough to outlast a real queue, which drains in round trips rather than
/// in minutes, and short enough that a baseline which moved for a reason other
/// than a path change is forgotten inside a minute.
const MIN_RTT_WINDOW: Duration = Duration::from_secs(15);

/// One reading of a session's link.
///
/// Every figure is measured by this endpoint about the path its connection has
/// selected, so most of them describe what this endpoint sends: QUIC reports a
/// loss count and a congestion event count for its own packets, and a
/// subscriber sends little but acknowledgements. The figures that describe the
/// downlink are [`goodput_bps`](Self::goodput_bps), which says what arrived,
/// and [`delivery_bps`](Self::delivery_bps), the publisher's own estimate.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct LinkSample {
    /// The smoothed round trip time.
    ///
    /// Sampled sparsely on a subscriber, because QUIC takes a sample only from a
    /// packet that asks to be acknowledged. Read it against
    /// [`min_rtt`](Self::min_rtt), and use [`rtt_samples`](Self::rtt_samples)
    /// to tell a fresh reading from a repeat.
    pub rtt: Duration,
    /// The number of distinct round trip readings since the path was selected.
    pub rtt_samples: u64,
    /// The smallest round trip seen recently on the selected path.
    ///
    /// The path's delay with nothing queued. Zero while unmeasured.
    pub min_rtt: Duration,
    /// Recent loss among the packets this endpoint sent, in `0.0..=1.0`.
    ///
    /// On a subscriber these are its acknowledgements, so this stands in for
    /// downlink loss only as far as both directions are impaired alike.
    pub loss_rate: f64,
    /// The rate bytes arrived at over the last second, in bits per second.
    ///
    /// `None` while too little arrives to be media.
    pub goodput_bps: Option<u64>,
    /// The rate bytes left at over the last second, in bits per second.
    pub upload_bps: Option<u64>,
    /// The peer's estimate of the path to this endpoint, in bits per second.
    ///
    /// `None` while it has not sent one.
    pub delivery_bps: Option<u64>,
    /// How many congestion events this endpoint's sending ran into.
    pub congestion_events: u64,
    /// Bumped whenever the connection selects another path.
    ///
    /// A reader compares figures only within one generation.
    pub path_generation: u64,
    /// Whether the selected path goes through an iroh relay.
    pub relayed: bool,
    /// The remote address of the selected path, for display.
    pub remote_addr: Option<String>,
    /// How many paths the connection holds open.
    pub paths: usize,
    /// Bytes received on the selected path since it was opened.
    pub bytes_received: u64,
    /// Bytes sent on the selected path since it was opened.
    pub bytes_sent: u64,
}

/// The latest sample of a session's link, shared with every session clone.
#[derive(Debug, Clone, Default)]
pub(crate) struct LinkState(Arc<Mutex<LinkSample>>);

impl LinkState {
    pub(crate) fn get(&self) -> LinkSample {
        self.0.lock().expect("poisoned").clone()
    }

    fn set(&self, sample: LinkSample) {
        *self.0.lock().expect("poisoned") = sample;
    }
}

/// Reads `connection`'s selected path into `state` every [`SAMPLE_INTERVAL`].
///
/// Runs until the connection closes or `cancel` fires. `delivery` is the peer's bandwidth estimate as moq-net delivers it, `None`
/// for a peer whose version sends none.
pub(crate) async fn monitor(
    connection: Connection,
    delivery: Option<moq_net::bandwidth::Consumer>,
    state: LinkState,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sampler = Sampler::default();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancel.cancelled() => break,
            _ = connection.closed() => break,
        }
        let paths = connection.paths();
        let count = paths.iter().count();
        let Some(selected) = paths.iter().find(|path| path.is_selected()) else {
            continue;
        };
        let delivery_bps = delivery
            .as_ref()
            .and_then(|estimate| estimate.peek())
            .map(moq_net::bandwidth::Rate::as_bps);
        let mut sample = sampler.sample(
            selected.id(),
            &selected.stats(),
            delivery_bps,
            Instant::now(),
        );
        sample.relayed = selected.is_relay();
        sample.remote_addr = Some(format!("{:?}", selected.remote_addr()));
        sample.paths = count;
        // Five a second is too much for anything but a trace, and it is exactly
        // what is wanted when an adaptation decision has to be explained after
        // the fact.
        trace!(
            path_generation = sample.path_generation,
            relayed = sample.relayed,
            rtt_ms = sample.rtt.as_millis() as u64,
            min_rtt_ms = sample.min_rtt.as_millis() as u64,
            loss_rate = sample.loss_rate,
            goodput_kbps = ?sample.goodput_bps.map(|bps| bps / 1000),
            delivery_kbps = ?sample.delivery_bps.map(|bps| bps / 1000),
            "link sample",
        );
        state.set(sample);
    }
}

/// A minimum over a sliding span, kept as two tumbling halves.
///
/// Holds the minimum of the current half and of the half before it, retiring
/// the older one wholesale when the span elapses. That covers between one and
/// two spans, and errs on the right side: a minimum that expired early would
/// read the path's own delay as a queue.
#[derive(Debug)]
struct WindowedMin {
    span: Duration,
    current: Duration,
    previous: Option<Duration>,
    since: Instant,
}

impl WindowedMin {
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

/// The cumulative counters one reading of the path carries.
#[derive(Debug, Clone, Copy)]
struct Counters {
    at: Instant,
    received_bytes: u64,
    sent_bytes: u64,
    sent_packets: u64,
    lost_packets: u64,
}

impl Counters {
    fn read(stats: &PathStats, now: Instant) -> Self {
        Self {
            at: now,
            received_bytes: stats.udp_rx.bytes,
            sent_bytes: stats.udp_tx.bytes,
            sent_packets: stats.udp_tx.datagrams,
            lost_packets: stats.lost_packets,
        }
    }
}

/// What one path has shown so far.
///
/// Its round trip baseline, and the window of counter readings the rates are
/// taken across. Belongs to one path. A reading from another path starts a fresh one, so no
/// rate is ever the difference between counters of two paths.
#[derive(Debug)]
struct PathHistory {
    path: PathId,
    min_rtt: WindowedMin,
    prev_rtt: Duration,
    rtt_samples: u64,
    /// Timestamped readings, oldest first.
    ///
    /// They span the longer of [`GOODPUT_WINDOW`] and [`LOSS_WINDOW`].
    readings: VecDeque<Counters>,
}

/// Turns readings of a path's statistics into [`LinkSample`]s.
///
/// Kept apart from the task that drives it so the arithmetic can be tested
/// against readings made up for the purpose, with no connection under it.
#[derive(Debug, Default)]
struct Sampler {
    history: Option<PathHistory>,
    path_generation: u64,
}

impl Sampler {
    /// Folds a reading of `stats`, taken on `path` at `now`, into a sample.
    fn sample(
        &mut self,
        path: PathId,
        stats: &PathStats,
        delivery_bps: Option<u64>,
        now: Instant,
    ) -> LinkSample {
        let rtt = stats.rtt;
        let history = match &mut self.history {
            Some(history) if history.path == path => history,
            _ => {
                if self.history.is_some() {
                    self.path_generation += 1;
                }
                self.history.insert(PathHistory {
                    path,
                    min_rtt: WindowedMin::new(MIN_RTT_WINDOW, rtt, now),
                    prev_rtt: rtt,
                    rtt_samples: 1,
                    readings: VecDeque::new(),
                })
            }
        };
        let min_rtt = history.min_rtt.record(rtt, now);
        // QUIC updates its estimate only when an acknowledgement brings a new
        // sample, so the same figure is read out over and over; counting the
        // changes lets a reader tell a fresh reading from a repeat.
        if history.prev_rtt != rtt {
            history.prev_rtt = rtt;
            history.rtt_samples += 1;
        }

        let latest = Counters::read(stats, now);
        history.readings.push_back(latest);
        // Keep the newest reading from before the longest window opens, so a
        // difference spans the whole window rather than stopping short of it.
        while history.readings.len() > 2
            && now.duration_since(history.readings[1].at) >= LOSS_WINDOW
        {
            history.readings.pop_front();
        }

        LinkSample {
            rtt,
            rtt_samples: history.rtt_samples,
            min_rtt,
            loss_rate: loss_rate(&history.readings, &latest),
            goodput_bps: rate(&history.readings, &latest, |c| c.received_bytes)
                .filter(|bps| *bps > GOODPUT_FLOOR_BPS),
            upload_bps: rate(&history.readings, &latest, |c| c.sent_bytes),
            delivery_bps,
            congestion_events: stats.congestion_events,
            path_generation: self.path_generation,
            bytes_received: stats.udp_rx.bytes,
            bytes_sent: stats.udp_tx.bytes,
            ..LinkSample::default()
        }
    }
}

/// Returns the newest reading taken at least `span` before `latest`.
fn reading_before<'a>(
    readings: &'a VecDeque<Counters>,
    latest: &Counters,
    span: Duration,
) -> Option<&'a Counters> {
    readings
        .iter()
        .rev()
        .find(|counters| latest.at.duration_since(counters.at) >= span)
}

/// Returns the rate of a byte counter across [`GOODPUT_WINDOW`], in bits per second.
///
/// `bytes` reads the counter. `None` until the readings span the window.
fn rate(
    readings: &VecDeque<Counters>,
    latest: &Counters,
    bytes: impl Fn(&Counters) -> u64,
) -> Option<u64> {
    let oldest = reading_before(readings, latest, GOODPUT_WINDOW)?;
    let span = latest.at.duration_since(oldest.at);
    let delta = bytes(latest).saturating_sub(bytes(oldest)) as f64;
    Some((delta * 8.0 / span.as_secs_f64()) as u64)
}

/// Returns the loss rate across [`LOSS_WINDOW`].
///
/// Zero while the readings do not span it or hold fewer than
/// [`LOSS_MIN_PACKETS`] packets.
fn loss_rate(readings: &VecDeque<Counters>, latest: &Counters) -> f64 {
    let Some(oldest) = reading_before(readings, latest, LOSS_WINDOW) else {
        return 0.0;
    };
    let lost = latest.lost_packets.saturating_sub(oldest.lost_packets);
    let sent = latest.sent_packets.saturating_sub(oldest.sent_packets);
    match sent + lost {
        total if total < LOSS_MIN_PACKETS => 0.0,
        total => lost as f64 / total as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a reading like the one each tick of the monitor sees.
    ///
    /// `rtt` in milliseconds, `lost` packets, `sent` datagrams and `received`
    /// bytes on the counters.
    fn reading(rtt_ms: u64, lost: u64, sent: u64, received: u64) -> PathStats {
        let mut stats = PathStats::default();
        stats.rtt = Duration::from_millis(rtt_ms);
        stats.lost_packets = lost;
        stats.udp_tx.datagrams = sent;
        stats.udp_rx.bytes = received;
        stats
    }

    /// Drives `ticks` readings through `sampler` and returns the last sample.
    ///
    /// One reading per interval from `t0`, with `sent` and `lost` packets added
    /// per tick.
    fn run_ticks(
        sampler: &mut Sampler,
        t0: Instant,
        ticks: u32,
        sent_per_tick: u64,
        lost_per_tick: u64,
    ) -> LinkSample {
        let mut last = LinkSample::default();
        for tick in 1..=ticks {
            let n = u64::from(tick);
            last = sampler.sample(
                PathId::ZERO,
                &reading(20, n * lost_per_tick, n * sent_per_tick, 0),
                None,
                t0 + tick * SAMPLE_INTERVAL,
            );
        }
        last
    }

    /// Loss is measured across a window of recent readings.
    ///
    /// Not against the connection's total, which history would dominate.
    #[test]
    fn loss_is_measured_across_the_window() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        // A hundred already lost before the first reading: never counted.
        sampler.sample(PathId::ZERO, &reading(20, 100, 1000, 0), None, t0);
        let steady = run_ticks(&mut sampler, t0, 12, 20, 2);
        assert!(
            (steady.loss_rate - 100.0 / 1100.0 * 1.1).abs() < 0.02,
            "two lost in twenty-two moved per tick is about 9%: {steady:?}"
        );
    }

    /// A lost acknowledgement among a handful is not a loss rate.
    #[test]
    fn a_lost_packet_among_a_handful_is_not_loss() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let clean = run_ticks(&mut sampler, t0, 10, 3, 0);
        assert_eq!(clean.loss_rate, 0.0, "nothing lost: {clean:?}");
        let lost_one = sampler.sample(
            PathId::ZERO,
            &reading(20, 1, 33, 0),
            None,
            t0 + 11 * SAMPLE_INTERVAL,
        );
        assert!(
            lost_one.loss_rate > 0.0 && lost_one.loss_rate < 0.05,
            "one in thirty-four is a small loss, not a third: {lost_one:?}"
        );
    }

    /// A window with too few packets to resolve a rate reports none.
    #[test]
    fn loss_needs_enough_packets_to_be_a_rate() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        run_ticks(&mut sampler, t0, 10, 1, 0);
        let lost_one = sampler.sample(
            PathId::ZERO,
            &reading(20, 1, 11, 0),
            None,
            t0 + 11 * SAMPLE_INTERVAL,
        );
        assert_eq!(lost_one.loss_rate, 0.0, "{lost_one:?}");
    }

    /// Loss that stops is forgotten once the window has moved past it.
    #[test]
    fn loss_clears_once_the_window_moves_past_it() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let bad = run_ticks(&mut sampler, t0, 11, 10, 5);
        assert!(bad.loss_rate > 0.3, "{bad:?}");
        let mut last = bad;
        for tick in 12..=31u32 {
            last = sampler.sample(
                PathId::ZERO,
                &reading(20, 55, 110 + u64::from(tick - 11) * 10, 0),
                None,
                t0 + tick * SAMPLE_INTERVAL,
            );
        }
        assert_eq!(last.loss_rate, 0.0, "{last:?}");
    }

    /// A repeated round trip is one sample, not several.
    #[test]
    fn a_repeated_round_trip_is_one_sample() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let mut samples = 0;
        for tick in 0..5u32 {
            samples = sampler
                .sample(
                    PathId::ZERO,
                    &reading(20, 0, 0, 0),
                    None,
                    t0 + tick * SAMPLE_INTERVAL,
                )
                .rtt_samples;
        }
        assert_eq!(samples, 1, "five ticks of one figure are one sample");
        let fresh = sampler.sample(
            PathId::ZERO,
            &reading(25, 0, 0, 0),
            None,
            t0 + 5 * SAMPLE_INTERVAL,
        );
        assert_eq!(fresh.rtt_samples, 2);
    }

    /// Goodput is bytes across the whole window.
    ///
    /// And nothing until the readings span it.
    #[test]
    fn goodput_needs_a_full_window() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let mut last = None;
        for tick in 0..=5u32 {
            let at = t0 + tick * SAMPLE_INTERVAL;
            last = sampler
                .sample(
                    PathId::ZERO,
                    &reading(20, 0, 0, u64::from(tick) * 100_000),
                    None,
                    at,
                )
                .goodput_bps;
            if at.duration_since(t0) < GOODPUT_WINDOW {
                assert_eq!(last, None, "no figure before the window spans");
            }
        }
        let bps = last.expect("a second of readings has a rate");
        assert!(
            (3_500_000..=4_500_000).contains(&bps),
            "expected about 4 Mbit/s, got {bps}"
        );
    }

    /// A new path starts its whole history over.
    ///
    /// Its baseline, its round trip count, and its rate windows, and the
    /// generation says so. Carried across, the rate windows would difference one
    /// path's counters against another's.
    #[test]
    fn a_new_path_starts_its_history_over() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        let direct = sampler.sample(PathId::ZERO, &reading(2, 0, 0, 0), None, t0);
        assert_eq!(direct.min_rtt, Duration::from_millis(2));
        assert_eq!(direct.path_generation, 0);

        // Heavy loss on the first path.
        let lossy = run_ticks(&mut sampler, t0, 11, 10, 5);
        assert!(lossy.loss_rate > 0.3);
        assert_eq!(lossy.min_rtt, Duration::from_millis(2));

        // The relay path's counters start from its own zero; differencing them
        // against the old path's would read as a huge negative, saturated to
        // nothing, or as a loss rate that belongs to the old path.
        let relayed = sampler.sample(
            PathId::MAX,
            &reading(40, 0, 0, 0),
            None,
            t0 + 12 * SAMPLE_INTERVAL,
        );
        assert_eq!(relayed.path_generation, 1);
        assert_eq!(relayed.min_rtt, Duration::from_millis(40));
        assert_eq!(relayed.loss_rate, 0.0);
        assert_eq!(relayed.goodput_bps, None);
        assert_eq!(relayed.rtt_samples, 1);
    }

    /// A path's counters start from wherever that path's history left them,
    /// so a backup path that lost packets long ago must not read as losing
    /// them now.
    #[test]
    fn loss_starts_over_on_a_new_path() {
        let mut sampler = Sampler::default();
        let t0 = Instant::now();
        // A clean direct path, long enough to fill the window.
        run_ticks(&mut sampler, t0, 12, 20, 0);
        // The backup path's counters carry three hundred old losses.
        let later = t0 + 13 * SAMPLE_INTERVAL;
        let backup = sampler.sample(PathId::MAX, &reading(40, 300, 400, 0), None, later);
        assert_eq!(backup.path_generation, 1);
        assert_eq!(
            backup.loss_rate, 0.0,
            "the backup path's old losses read as new ones: {backup:?}"
        );
    }

    #[test]
    fn a_windowed_minimum_forgets_the_path_it_came_from() {
        let span = Duration::from_secs(15);
        let start = Instant::now();
        let mut min = WindowedMin::new(span, Duration::from_millis(2), start);
        assert_eq!(
            min.record(Duration::from_millis(60), start),
            Duration::from_millis(2)
        );
        assert_eq!(
            min.record(Duration::from_millis(60), start + span / 2),
            Duration::from_millis(2)
        );
        assert_eq!(
            min.record(Duration::from_millis(60), start + span),
            Duration::from_millis(2)
        );
        assert_eq!(
            min.record(Duration::from_millis(60), start + span * 2),
            Duration::from_millis(60)
        );
    }

    #[test]
    fn a_windowed_minimum_takes_a_dip_immediately() {
        let span = Duration::from_secs(15);
        let start = Instant::now();
        let mut min = WindowedMin::new(span, Duration::from_millis(60), start);
        assert_eq!(
            min.record(Duration::from_millis(3), start + span / 4),
            Duration::from_millis(3)
        );
    }
}
