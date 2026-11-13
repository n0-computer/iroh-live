//! The send-rate estimate a [`Transport`](crate::Transport) reports for rate control.
//!
//! moq-net reads a transport's send-rate estimate through
//! [`Metrics::estimated_send_rate`](moq_net::transport::Metrics::estimated_send_rate).
//! The session divides it among its senders and reports it to the peer, whose
//! players pick a rendition by it. [`SendEstimate`] chooses where the number
//! comes from, and [`RateConfig`] carries the choice to
//! [`Transport::with_rate`](crate::Transport::with_rate).
//!
//! The default, [`SendEstimate::Window`], is the congestion window over the
//! round trip. Under BBR3 it reads up to 1.6 times what a capped path carries,
//! because the window carries BBR3's gain over the delivery rate. The other
//! two run an estimator in the application, which reads the selected path's
//! statistics:
//!
//! - It lowers the estimate to 0.85 times the delivered rate once the round
//!   trip shows a queue on two readings in a row, and by half the loss rate
//!   once loss passes 10%, never under 50 kbit/s.
//! - While the path stays clean it lets the estimate climb up to 1.5 times
//!   the session's probe target, which is what the session's senders
//!   reserved. It climbs 10% a second just above the rate at which the path
//!   last queued, and 25% a second below and well past it. A long climb
//!   speeds up. The climb is the probe. A sender that follows the estimate up
//!   finds out whether the path carries it, and the next queue brings the
//!   estimate back. No padding is sent.
//! - A change of the selected path starts the round trip baseline over, and
//!   the estimate holds through a settle window. A round trip that rises and
//!   stays up on the same path reads as a queue at first, and the estimate
//!   falls. A queue drains once the sender backs off, and a longer path does
//!   not. So a round trip that stays flat for a second after the sender backed
//!   off, with no loss, becomes the baseline, and the fall is undone.
//!
//! Until the path first shows congestion, the estimate is the window estimate
//! ([`SendEstimate::App`]) or the controller's ([`SendEstimate::Controller`]),
//! so a sender starts at the rate it was configured with.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use iroh::endpoint::{Connection, PathId};
use moq_net::bandwidth::Rate;
use tracing::{debug, trace};

/// Where a transport's send-rate estimate comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SendEstimate {
    /// The congestion window over the round trip of the selected path.
    ///
    /// The estimate every transport reported before rate control, and still
    /// the default. Under BBR3 it reads up to 1.6 times what a capped path
    /// carries, and it never falls while a sender is limited by its media.
    #[default]
    Window,
    /// The application's estimator over the selected path's statistics.
    ///
    /// It reports the window estimate until the path first shows congestion,
    /// and its own estimate from then on.
    App,
    /// The congestion controller's estimate, capped by the application's estimator.
    ///
    /// BBR3 reports its bandwidth model here. CUBIC and NewReno report none,
    /// and the application's estimator stands in. noq 1.4's BBR3 never lowers
    /// its model while a sender is limited by its media, so the cap is what
    /// brings the estimate down after a drop. With a controller that ages its
    /// model, the estimate can only climb as far as the controller has seen.
    Controller,
}

impl SendEstimate {
    /// Every source, the default first.
    pub const ALL: [Self; 3] = [Self::Window, Self::App, Self::Controller];

    /// Returns the name it displays as and parses from.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::App => "app",
            Self::Controller => "controller",
        }
    }
}

impl std::fmt::Display for SendEstimate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for SendEstimate {
    type Err = UnknownSendEstimate;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|estimate| estimate.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| UnknownSendEstimate(name.to_string()))
    }
}

/// An error returned when a name is not that of a [`SendEstimate`].
#[n0_error::stack_error(derive)]
#[derive(Clone, PartialEq, Eq)]
#[error("unknown send estimate {_0:?}: expected window, app, or controller")]
pub struct UnknownSendEstimate(String);

/// How a transport estimates its send rate.
///
/// The default reports the window estimate, as a transport from
/// [`Transport::new`](crate::Transport::new) does.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RateConfig {
    /// Where the estimate comes from.
    pub estimate: SendEstimate,
    /// How long the estimate holds after the selected path changes.
    ///
    /// A new path has no round trip baseline, so a queue cannot show in it
    /// yet. The default is two seconds.
    pub settle: Duration,
}

impl Default for RateConfig {
    fn default() -> Self {
        Self {
            estimate: SendEstimate::Window,
            settle: DEFAULT_SETTLE,
        }
    }
}

impl RateConfig {
    /// Returns a config that estimates from `estimate`, with the default settle window.
    #[must_use]
    pub fn new(estimate: SendEstimate) -> Self {
        Self {
            estimate,
            ..Self::default()
        }
    }

    /// Returns this config with the estimate holding for `settle` after a path change.
    ///
    /// See [`settle`](Self::settle).
    #[must_use]
    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle = settle;
        self
    }
}

/// The default [`RateConfig::settle`].
const DEFAULT_SETTLE: Duration = Duration::from_secs(2);

/// One reading of a transport's estimator.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[non_exhaustive]
pub struct RateSample {
    /// The send-rate estimate, as the transport reports it to moq-net.
    pub estimate: Option<Rate>,
    /// What the selected path delivered over the last window.
    ///
    /// Bytes sent less bytes lost, so it counts what is still in flight.
    pub goodput: Option<Rate>,
    /// The smoothed round trip less its minimum over the last ten seconds.
    pub queue_delay: Option<Duration>,
    /// Lost over sent packets in the last two seconds.
    pub loss: f64,
    /// Whether the estimate follows the path rather than the window or the controller.
    ///
    /// It turns true when the path first shows congestion.
    pub capped: bool,
    /// Bumped on every change of the selected path.
    pub path_generation: u64,
}

/// Reads one transport's estimator.
///
/// Cheap to clone. Every read refreshes the estimate from the selected path,
/// at most every 50 ms, so a caller that reads on a timer drives the
/// estimator as moq-net's own sampling does.
#[derive(Debug, Clone)]
pub struct RateHandle {
    shared: Arc<RateShared>,
}

impl RateHandle {
    /// Returns the latest reading, refreshed from the selected path if it is due.
    #[must_use]
    pub fn sample(&self) -> RateSample {
        self.shared.sample(Instant::now())
    }

    /// Reports whether the connection has closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.connection.close_reason().is_some()
    }

    /// Returns the target the session asks the transport to probe up to.
    ///
    /// The estimate climbs toward it while the path stays clean. Zero when the
    /// session has none.
    #[must_use]
    pub fn target(&self) -> Rate {
        self.shared.probe.target()
    }
}

/// What a transport and its [`RateHandle`]s share.
pub(crate) struct RateShared {
    config: RateConfig,
    connection: Connection,
    pub(crate) probe: moq_net::transport::Probe,
    state: Mutex<Estimating>,
}

impl std::fmt::Debug for RateShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateShared")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// The estimator and its last reading.
#[derive(Debug)]
struct Estimating {
    estimator: Estimator,
    last: Option<(Instant, RateSample)>,
}

/// The least time between two readings of the path.
///
/// moq-net samples every 100 ms while anyone reads the estimate, and a node
/// reads its links on its own timer. Readings closer than this return the
/// last one, so two readers never halve the estimator's step.
const MIN_INTERVAL: Duration = Duration::from_millis(50);

impl RateShared {
    /// Creates the estimator of `connection`, as `config` says.
    pub(crate) fn new(connection: Connection, config: RateConfig) -> Self {
        Self {
            state: Mutex::new(Estimating {
                estimator: Estimator::new(config.estimate, config.settle),
                last: None,
            }),
            config,
            connection,
            probe: moq_net::transport::Probe::new(),
        }
    }

    /// Returns a handle on this estimator.
    pub(crate) fn handle(self: &Arc<Self>) -> RateHandle {
        RateHandle {
            shared: self.clone(),
        }
    }

    /// Returns the latest reading at `now`, refreshing it from the selected path if due.
    pub(crate) fn sample(&self, now: Instant) -> RateSample {
        let mut state = self.state.lock().expect("poisoned");
        if let Some((at, sample)) = state.last
            && now.saturating_duration_since(at) < MIN_INTERVAL
        {
            return sample;
        }
        let paths = self.connection.paths();
        let Some(selected) = paths.iter().find(|path| path.is_selected()) else {
            return state.last.map(|(_, sample)| sample).unwrap_or_default();
        };
        let stats = selected.stats();
        let path = PathSample {
            path: selected.id(),
            rtt: stats.rtt,
            cwnd: stats.cwnd,
            sent_bytes: stats.sent_bytes,
            lost_bytes: stats.lost_bytes,
            sent_packets: stats.sent_packets,
            lost_packets: stats.lost_packets,
            controller: stats.bandwidth_estimate,
        };
        let target = self.probe.target();
        let sample = state.estimator.on_sample(path, target, now);
        state.last = Some((now, sample));
        sample
    }
}

/// The counters one reading of the selected path carries, cumulative where they count.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PathSample {
    /// The path the reading is of.
    pub(crate) path: PathId,
    /// The smoothed round trip.
    pub(crate) rtt: Duration,
    /// The congestion window in bytes.
    pub(crate) cwnd: u64,
    pub(crate) sent_bytes: u64,
    pub(crate) lost_bytes: u64,
    pub(crate) sent_packets: u64,
    pub(crate) lost_packets: u64,
    /// The congestion controller's estimate in bits per second, if it has one.
    pub(crate) controller: Option<u64>,
}

/// The span the round trip minimum is taken across.
///
/// Long enough to outlast a queue, short enough to forget a baseline that
/// moved without a path change.
const MIN_RTT_WINDOW: Duration = Duration::from_secs(10);

/// The shortest span the delivered rate is measured across.
///
/// Four round trips when that is longer, so bytes in flight weigh little.
const GOODPUT_WINDOW: Duration = Duration::from_millis(500);

/// The span the loss rate is measured across.
const LOSS_WINDOW: Duration = Duration::from_secs(2);

/// The fewest packets sent in [`LOSS_WINDOW`] for a loss rate. Below it the rate is zero.
const MIN_PACKETS: u64 = 20;

/// The bounds of the queueing delay that counts as overuse.
///
/// The threshold is half the minimum round trip within them. A smoothed round
/// trip on a wired path moves by a few milliseconds. One on Wi-Fi moves by
/// more, so the floor keeps it from reading as a queue.
const QUEUE_FLOOR: Duration = Duration::from_millis(15);
const QUEUE_CEILING: Duration = Duration::from_millis(60);

/// How many readings in a row have to show a queue before the estimate falls.
const OVERUSE_READINGS: u32 = 2;

/// The share of the delivered rate the estimate falls to on overuse, GCC's beta.
const BACKOFF: f64 = 0.85;

/// The lowest the estimate falls to, in bits per second.
///
/// The delivered rate reads low during a burst of loss, since lost bytes are
/// counted when they are declared lost. An estimate that fell to it would take
/// long to climb back from a few kbit/s.
const FLOOR: u64 = 50_000;

/// Loss above which the estimate falls by half the loss rate, GCC's rule.
const LOSS_DECREASE: f64 = 0.10;

/// Loss under which the estimate may climb.
const LOSS_CLEAN: f64 = 0.02;

/// How fast the estimate climbs while the path is clean, per second, near where it last queued.
///
/// A little over GCC's 8%, since the climb is also the only probe.
const CLIMB_NEAR: f64 = 0.10;

/// How fast the estimate climbs away from where the path last queued, per second.
///
/// The ramp of upstream's rate policy, so a sender recovers from a lifted cap
/// as fast as it may raise its bitrate.
const CLIMB_FAR: f64 = 0.25;

/// The fastest the far climb gets, per second.
///
/// The far climb speeds up by [`CLIMB_FAR`] for each second it goes on, up to
/// this, which about doubles the estimate each second. A sender follows the
/// estimate up no faster than its own ramp. So a faster climb only shortens
/// how long a paused rendition waits to resume.
const CLIMB_FAR_MAX: f64 = 0.7;

/// How far past the rate the path last congested at the near climb reaches.
///
/// GCC climbs additively near its last decrease and multiplicatively away
/// from it. This is the same split. Below that rate the estimate climbs at
/// [`CLIMB_FAR`]: the path carried it before it queued.
const NEAR: f64 = 1.25;

/// How far above the session's probe target the estimate may climb.
///
/// A player keeps a rendition while the estimate covers 1.25 times its
/// bitrate, so an estimate that stopped at the target would never let it
/// climb to the top rendition.
const TARGET_HEADROOM: f64 = 1.5;

/// How far above the delivered rate the estimate may climb when the session has no target.
///
/// The cap of GCC's increase, plus a little so a quiet path can start.
const GOODPUT_HEADROOM: f64 = 1.5;
const GOODPUT_SLACK: u64 = 10_000;

/// How long the estimate holds after a fall before it climbs again.
///
/// It doubles for each fall within [`HOLD_RESET`] of the last that ends a
/// climb, up to [`HOLD_MAX`], so a sender that keeps overshooting tries less
/// often. Further falls on one congestion, with no climb between them, keep
/// the hold as it is.
const HOLD_MIN: Duration = Duration::from_secs(1);
const HOLD_MAX: Duration = Duration::from_secs(4);
const HOLD_RESET: Duration = Duration::from_secs(10);

/// How long a queue may persist after a fall before the estimate falls again.
///
/// The sender needs a moment to follow the fall, and the queue drains only
/// after it has. It falls again only while the queue has not shrunk below
/// [`DRAINING`] of its peak since the fall: a deep queue on a slow link
/// takes seconds to drain, and every fall during that would cut the
/// estimate under the link.
const REDECREASE: Duration = Duration::from_secs(2);

/// The share of its peak a queue has to shrink to before it counts as draining.
const DRAINING: f64 = 0.9;

/// How far a queue has to grow to count as growing.
const GROWING: f64 = 1.1;

/// How long a queue has to stay under its peak to count as flat.
const FLAT: Duration = Duration::from_secs(1);

/// The share of the delivered rate at the last queue a sender has to fall under to count as backed off.
///
/// A queue drains once the sender sends less than the path carries. A round
/// trip that stays flat for [`FLAT`] while the sender sends less than this,
/// with no loss, is a longer path. A route change on the same QUIC path looks
/// like that. The longer round trip becomes the baseline, and the fall it
/// caused is undone.
const BACKED_OFF: f64 = 0.9;

/// The counters of one reading, and when it was taken.
#[derive(Debug, Clone, Copy)]
struct Reading {
    at: Instant,
    rtt: Duration,
    delivered: u64,
    sent_packets: u64,
    lost_packets: u64,
}

/// The application's send-rate estimator over the readings of one connection.
///
/// It never reads a clock, so its tests run on made-up instants.
#[derive(Debug)]
pub(crate) struct Estimator {
    source: SendEstimate,
    settle: Duration,
    path: Option<PathId>,
    path_generation: u64,
    /// No queue counts as overuse before this, after a path change.
    settled_at: Option<Instant>,
    /// This path's readings over the last [`MIN_RTT_WINDOW`], oldest first.
    readings: VecDeque<Reading>,
    /// Readings before this are left out of the round trip minimum.
    ///
    /// Set when a round trip that rose and stayed flat turned out to be a
    /// longer path.
    baseline_since: Option<Instant>,
    /// The estimate once the path showed congestion, `None` before.
    cap: Option<u64>,
    /// The state before the first fall on the current queue, restored if the
    /// queue turns out to be a longer path.
    before_queue: Option<Fallen>,
    /// Readings in a row that showed a queue.
    overused: u32,
    /// Whether the queue of the last fall has yet to drain.
    draining: bool,
    /// The largest queue seen since the last fall.
    peak_queue: Duration,
    /// When the queue last grew by a tenth, and how long it was then.
    grew_at: Option<Instant>,
    grew_from: Duration,
    /// The delivered rate when the estimate last fell.
    queued_at: Option<u64>,
    /// The rate the path last congested at, near which the climb slows.
    congested_at: Option<u64>,
    /// When the estimate last fell.
    fell_at: Option<Instant>,
    /// Since when the estimate has climbed at the far rate without a break.
    climbing_since: Option<Instant>,
    /// How long after [`fell_at`](Self::fell_at) the estimate holds.
    hold: Duration,
    /// Whether the estimate climbed since it last fell.
    climbed: bool,
}

impl Estimator {
    /// Creates an estimator that reports from `source`.
    pub(crate) fn new(source: SendEstimate, settle: Duration) -> Self {
        Self {
            source,
            settle,
            path: None,
            path_generation: 0,
            settled_at: None,
            readings: VecDeque::new(),
            baseline_since: None,
            cap: None,
            before_queue: None,
            overused: 0,
            draining: false,
            peak_queue: Duration::ZERO,
            grew_at: None,
            grew_from: Duration::ZERO,
            queued_at: None,
            congested_at: None,
            fell_at: None,
            climbing_since: None,
            hold: HOLD_MIN,
            climbed: false,
        }
    }

    /// Folds `sample`, read at `now`, into the estimate, climbing toward `target`.
    pub(crate) fn on_sample(
        &mut self,
        sample: PathSample,
        target: Rate,
        now: Instant,
    ) -> RateSample {
        if self.path != Some(sample.path) {
            if self.path.is_some() {
                self.path_generation += 1;
                self.settled_at = Some(now + self.settle);
                debug!(
                    path_generation = self.path_generation,
                    "the selected path changed, holding the estimate"
                );
            }
            self.path = Some(sample.path);
            self.readings.clear();
            self.baseline_since = None;
            self.before_queue = None;
            self.overused = 0;
            self.draining = false;
        }
        let elapsed = self
            .readings
            .back()
            .map(|last| now.saturating_duration_since(last.at));
        self.readings.push_back(Reading {
            at: now,
            rtt: sample.rtt,
            delivered: sample.sent_bytes.saturating_sub(sample.lost_bytes),
            sent_packets: sample.sent_packets,
            lost_packets: sample.lost_packets,
        });
        while self
            .readings
            .front()
            .is_some_and(|first| now.saturating_duration_since(first.at) > MIN_RTT_WINDOW)
        {
            self.readings.pop_front();
        }

        let min_rtt = self
            .readings
            .iter()
            .filter(|reading| self.baseline_since.is_none_or(|since| reading.at >= since))
            .map(|reading| reading.rtt)
            .filter(|rtt| !rtt.is_zero())
            .min();
        let queue = min_rtt.map(|min| sample.rtt.saturating_sub(min));
        let goodput = min_rtt.and_then(|min| self.goodput(now, GOODPUT_WINDOW.max(min * 4)));
        let loss = self.loss(now, None);
        // Loss counted once: what the last fall already answered for does
        // not lower the estimate again.
        let fresh_loss = self.loss(now, self.fell_at);
        let window = (sample.cwnd > 0 && !sample.rtt.is_zero())
            .then(|| (sample.cwnd as f64 * 8.0 / sample.rtt.as_secs_f64()) as u64);
        let before = self.estimate(window, sample.controller);
        let climbing_since = self.climbing_since.take();

        let settling = self.settled_at.is_some_and(|until| now < until);
        let threshold = min_rtt
            .map(|min| (min / 2).clamp(QUEUE_FLOOR, QUEUE_CEILING))
            .unwrap_or(QUEUE_CEILING);
        let queued = queue.is_some_and(|queue| queue > threshold);
        let drained = queue.is_none_or(|queue| queue < threshold / 2);
        self.overused = match queued && !settling {
            true => self.overused + 1,
            false => 0,
        };
        let since_fall = self.fell_at.map(|at| now.saturating_duration_since(at));
        let queue_now = queue.unwrap_or_default();
        let shrinking = queue_now.as_secs_f64() < self.peak_queue.as_secs_f64() * DRAINING;
        if self.draining {
            if queue_now.as_secs_f64() > self.grew_from.as_secs_f64() * GROWING {
                self.grew_at = Some(now);
                self.grew_from = queue_now;
            }
            self.peak_queue = self.peak_queue.max(queue_now);
        }
        let growing = self
            .grew_at
            .is_some_and(|at| now.saturating_duration_since(at) < FLAT);

        let backed_off = goodput
            .zip(self.queued_at)
            .is_some_and(|(goodput, queued)| (goodput as f64) < queued as f64 * BACKED_OFF);
        let longer_path = self.draining
            && queued
            && !shrinking
            && !growing
            && backed_off
            && loss < LOSS_CLEAN
            && self.before_queue.is_some();
        if longer_path {
            // A queue would have drained by now. This one is a longer path.
            self.baseline_since = Some(now);
            self.draining = false;
            self.overused = 0;
            if let Some(fallen) = self.before_queue.take() {
                fallen.restore(self);
            }
            debug!(
                rtt_ms = sample.rtt.as_millis() as u64,
                estimate_kbps = self.cap.map(|cap| cap / 1000),
                "the round trip rose and stayed flat, taking it as the new baseline"
            );
        } else if self.overused >= OVERUSE_READINGS
            && let Some(goodput) = goodput
            && (!self.draining
                || (since_fall.is_none_or(|since| since >= REDECREASE)
                    && !shrinking
                    && (growing || !backed_off || self.cap.is_some_and(|cap| cap > goodput))))
        {
            if !self.draining {
                self.before_queue = Some(Fallen::of(self));
            }
            let lowered = (goodput as f64 * BACKOFF) as u64;
            self.fall(
                self.cap.unwrap_or(u64::MAX).min(lowered),
                now,
                queue_now,
                Some(goodput),
            );
            debug!(
                estimate_kbps = self.cap.unwrap_or(0) / 1000,
                goodput_kbps = goodput / 1000,
                queue_ms = queue.unwrap_or_default().as_millis() as u64,
                "a queue built, lowering the estimate"
            );
        } else if fresh_loss > LOSS_DECREASE && since_fall.is_none_or(|since| since >= HOLD_MIN) {
            // GCC lowers its own estimate, which it paces to. This one can
            // sit far above what the path delivers, before the first
            // congestion or while the sender lags it, so the climb's cap
            // bounds it too.
            let ceiling = goodput.map_or(u64::MAX, headroom);
            let base = self.cap.or(before).unwrap_or(u64::MAX).min(ceiling);
            if base != u64::MAX {
                // A longer path loses nothing, so no fall here is undone.
                self.before_queue = None;
                self.fall(
                    (base as f64 * (1.0 - fresh_loss / 2.0)) as u64,
                    now,
                    queue_now,
                    goodput,
                );
                debug!(
                    estimate_kbps = self.cap.unwrap_or(0) / 1000,
                    loss = fresh_loss,
                    "loss is high, lowering the estimate"
                );
            }
        } else if self.draining {
            self.draining = !drained;
            if drained {
                // It drained, so it was a queue.
                self.before_queue = None;
            }
        } else if let Some(cap) = self.cap
            && drained
            && !settling
            && loss < LOSS_CLEAN
            && since_fall.is_none_or(|since| since >= self.hold)
        {
            let ceiling = match target.as_bps() {
                0 => goodput.map_or(cap, headroom),
                target => {
                    ((target as f64 * TARGET_HEADROOM) as u64).max(goodput.map_or(0, headroom))
                }
            };
            if cap < ceiling {
                let near = self.congested_at.is_some_and(|congested| {
                    (congested..((congested as f64 * NEAR) as u64)).contains(&cap)
                });
                let climb = match near {
                    true => CLIMB_NEAR,
                    false => {
                        let since = *self.climbing_since.insert(climbing_since.unwrap_or(now));
                        let speedup = now.saturating_duration_since(since).as_secs_f64();
                        (CLIMB_FAR * (1.0 + speedup)).min(CLIMB_FAR_MAX)
                    }
                };
                let step = elapsed.unwrap_or_default().min(Duration::from_secs(1));
                let grown = (cap as f64 * (1.0 + climb * step.as_secs_f64())).ceil() as u64;
                self.cap = Some(grown.min(ceiling));
                self.climbed = true;
            }
        }

        let estimate = self.estimate(window, sample.controller);
        let sample = RateSample {
            estimate: estimate.map(Rate::from_bps),
            goodput: goodput.map(Rate::from_bps),
            queue_delay: queue,
            loss,
            capped: self.source != SendEstimate::Window && self.cap.is_some(),
            path_generation: self.path_generation,
        };
        trace!(
            estimate_kbps = estimate.map(|bps| bps / 1000),
            goodput_kbps = goodput.map(|bps| bps / 1000),
            queue_ms = queue.map(|queue| queue.as_millis() as u64),
            loss,
            target_kbps = target.as_bps() / 1000,
            "send estimate"
        );
        sample
    }

    /// Lowers the estimate to `cap` at `now`, and lengthens the hold if it fell recently.
    ///
    /// `queue` and `goodput` are the queueing delay and the delivered rate
    /// when it fell, which later readings compare against.
    fn fall(&mut self, cap: u64, now: Instant, queue: Duration, goodput: Option<u64>) {
        self.hold = match self.fell_at {
            Some(at) if now.saturating_duration_since(at) < HOLD_RESET => match self.climbed {
                true => (self.hold * 2).min(HOLD_MAX),
                false => self.hold,
            },
            _ => HOLD_MIN,
        };
        self.climbed = false;
        let cap = cap.max(FLOOR);
        self.cap = Some(cap);
        self.congested_at = Some((cap as f64 / BACKOFF) as u64);
        self.fell_at = Some(now);
        self.draining = true;
        self.overused = 0;
        self.peak_queue = queue;
        self.grew_at = Some(now);
        self.grew_from = queue;
        if goodput.is_some() {
            self.queued_at = goodput;
        }
    }

    /// Returns the estimate to report, from the window and controller estimates of this reading.
    fn estimate(&self, window: Option<u64>, controller: Option<u64>) -> Option<u64> {
        match self.source {
            SendEstimate::Window => window,
            SendEstimate::App => self.cap.or(window),
            SendEstimate::Controller => match (controller, self.cap) {
                (Some(controller), Some(cap)) => Some(controller.min(cap)),
                (Some(controller), None) => Some(controller),
                (None, cap) => cap.or(window),
            },
        }
    }

    /// Returns the delivered rate over at least `window`, or `None` before the readings span it.
    fn goodput(&self, now: Instant, window: Duration) -> Option<u64> {
        let latest = self.readings.back()?;
        let base = self
            .readings
            .iter()
            .rev()
            .find(|reading| now.saturating_duration_since(reading.at) >= window)?;
        let span = latest.at.saturating_duration_since(base.at).as_secs_f64();
        let bytes = latest.delivered.saturating_sub(base.delivered);
        (span > 0.0).then(|| (bytes as f64 * 8.0 / span) as u64)
    }

    /// Returns the loss rate over [`LOSS_WINDOW`], or since `since` if that is later.
    ///
    /// Zero before enough packets.
    fn loss(&self, now: Instant, since: Option<Instant>) -> f64 {
        let start = now.checked_sub(LOSS_WINDOW);
        let start = match (start, since) {
            (Some(start), Some(since)) => Some(start.max(since)),
            (start, since) => start.or(since),
        };
        let (Some(latest), Some(base)) = (
            self.readings.back(),
            self.readings
                .iter()
                .rev()
                .find(|reading| start.is_some_and(|start| reading.at <= start))
                .or_else(|| self.readings.front()),
        ) else {
            return 0.0;
        };
        let sent = latest.sent_packets.saturating_sub(base.sent_packets);
        let lost = latest.lost_packets.saturating_sub(base.lost_packets);
        match sent {
            sent if sent < MIN_PACKETS => 0.0,
            sent => (lost as f64 / sent as f64).min(1.0),
        }
    }
}

/// What a fall changed, so a fall on a longer path can be undone.
#[derive(Debug, Clone, Copy)]
struct Fallen {
    cap: Option<u64>,
    congested_at: Option<u64>,
    fell_at: Option<Instant>,
    hold: Duration,
    climbed: bool,
}

impl Fallen {
    /// Records the state `estimator` is in before a fall.
    fn of(estimator: &Estimator) -> Self {
        Self {
            cap: estimator.cap,
            congested_at: estimator.congested_at,
            fell_at: estimator.fell_at,
            hold: estimator.hold,
            climbed: estimator.climbed,
        }
    }

    /// Puts `estimator` back as it was before the fall.
    fn restore(self, estimator: &mut Estimator) {
        estimator.cap = self.cap;
        estimator.congested_at = self.congested_at;
        estimator.fell_at = self.fell_at;
        estimator.hold = self.hold;
        estimator.climbed = self.climbed;
    }
}

/// Returns how far the estimate may climb above `goodput` without a target.
fn headroom(goodput: u64) -> u64 {
    (goodput as f64 * GOODPUT_HEADROOM) as u64 + GOODPUT_SLACK
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(100);
    const BASE_RTT: Duration = Duration::from_millis(20);

    /// A path driven on a made-up clock, sending at a set rate.
    struct Path {
        estimator: Estimator,
        now: Instant,
        id: PathId,
        rtt: Duration,
        /// Bits per second the sender delivers.
        rate: u64,
        sent_bytes: u64,
        lost_bytes: u64,
        sent_packets: u64,
        lost_packets: u64,
        /// Lost packets per hundred sent.
        loss_percent: u64,
        /// Hundredths of a lost packet not yet counted.
        loss_debt: u64,
        controller: Option<u64>,
        target: Rate,
    }

    impl Path {
        fn new(source: SendEstimate) -> Self {
            Self {
                estimator: Estimator::new(source, DEFAULT_SETTLE),
                now: Instant::now(),
                id: PathId::ZERO,
                rtt: BASE_RTT,
                rate: 1_000_000,
                sent_bytes: 0,
                lost_bytes: 0,
                sent_packets: 0,
                lost_packets: 0,
                loss_percent: 0,
                loss_debt: 0,
                controller: None,
                target: Rate::ZERO,
            }
        }

        /// Advances one tick and returns the estimator's reading.
        fn tick(&mut self) -> RateSample {
            self.now += TICK;
            let bytes = self.rate / 8 / 10;
            let packets = (bytes / 1200).max(1);
            self.sent_bytes += bytes;
            self.sent_packets += packets;
            self.loss_debt += packets * self.loss_percent;
            let lost = self.loss_debt / 100;
            self.loss_debt %= 100;
            self.lost_packets += lost;
            self.lost_bytes += lost * 1200;
            let sample = PathSample {
                path: self.id,
                rtt: self.rtt,
                // A window at twice the bandwidth-delay product of 4 Mbit/s.
                cwnd: 2 * 4_000_000 / 8 * self.rtt.as_millis() as u64 / 1000,
                sent_bytes: self.sent_bytes,
                lost_bytes: self.lost_bytes,
                sent_packets: self.sent_packets,
                lost_packets: self.lost_packets,
                controller: self.controller,
            };
            self.estimator.on_sample(sample, self.target, self.now)
        }

        /// Advances `duration` and returns the last reading.
        fn run(&mut self, duration: Duration) -> RateSample {
            let mut last = RateSample::default();
            for _ in 0..duration.as_millis() / TICK.as_millis() {
                last = self.tick();
            }
            last
        }
    }

    fn kbps(sample: RateSample) -> u64 {
        sample.estimate.map_or(0, |rate| rate.as_bps() / 1000)
    }

    #[test]
    fn the_window_source_reports_the_window() {
        let mut path = Path::new(SendEstimate::Window);
        let sample = path.run(Duration::from_secs(1));
        assert_eq!(kbps(sample), 8_000, "twice 4 Mbit/s");
        path.rtt = BASE_RTT * 10;
        let sample = path.run(Duration::from_secs(1));
        assert!(!sample.capped, "the window source never caps");
    }

    #[test]
    fn before_congestion_the_app_source_reports_the_window() {
        let mut path = Path::new(SendEstimate::App);
        let sample = path.run(Duration::from_secs(3));
        assert!(!sample.capped);
        assert_eq!(kbps(sample), 8_000);
    }

    #[test]
    fn a_queue_lowers_the_estimate_to_the_delivered_rate() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        let first = path.tick();
        assert!(!first.capped, "one reading is not a queue");
        let second = path.tick();
        assert!(second.capped);
        assert_eq!(kbps(second), 850, "0.85 times the 1 Mbit/s delivered");
    }

    #[test]
    fn a_growing_queue_lowers_the_estimate_again_only_after_a_while() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        let fell = kbps(path.run(TICK * 2));
        path.rate = 600_000;
        // The link slowed down further, so the queue still grows.
        let mut grow = || {
            path.rtt += Duration::from_millis(15);
            path.tick()
        };
        let mut draining = RateSample::default();
        for _ in 0..15 {
            draining = grow();
        }
        assert_eq!(kbps(draining), fell, "one fall per queue");
        let mut again = draining;
        for _ in 0..10 {
            again = grow();
        }
        assert!(
            kbps(again) < fell,
            "a queue that outgrows the sender's fall"
        );
    }

    #[test]
    fn a_draining_queue_does_not_lower_the_estimate_again() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 10;
        let fell = kbps(path.run(TICK * 3));
        // A deep queue that drains for seconds, above the threshold all along.
        for _ in 0..40 {
            path.rtt = path.rtt.saturating_sub(Duration::from_millis(3));
            path.tick();
        }
        assert!(path.rtt > BASE_RTT * 2, "the queue is still there");
        assert_eq!(kbps(path.tick()), fell, "one fall for a queue that drains");
    }

    #[test]
    fn a_longer_path_becomes_the_baseline_and_undoes_its_fall() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(2_000_000);
        path.run(Duration::from_secs(3));
        // The same path gets 60 ms longer, with no queue.
        path.rtt = BASE_RTT + Duration::from_millis(60);
        let fell = path.run(TICK * 2);
        assert!(fell.capped, "a step reads as a queue at first");
        assert_eq!(kbps(fell), 850);
        // The sender follows the fall, and the round trip stays where it is.
        path.rate = 850_000;
        let undone = path.run(Duration::from_secs(2));
        assert!(!undone.capped, "the fall was undone: {undone:?}");
        assert_eq!(kbps(undone), 8_000, "back to the window estimate");
        assert_eq!(undone.queue_delay, Some(Duration::ZERO));
        // A queue on top of the longer path still reads as one.
        path.rtt += BASE_RTT * 4;
        let queued = path.run(TICK * 2);
        assert!(queued.capped);
        assert_eq!(kbps(queued), 722, "0.85 times the 850 kbit/s delivered");
    }

    /// A step after the path congested once restores the estimate it had before the step.
    #[test]
    fn a_longer_path_restores_the_estimate_before_it() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(2_000_000);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        path.run(TICK * 2);
        path.rtt = BASE_RTT;
        let before = kbps(path.run(Duration::from_secs(12)));
        assert!(before > 850, "climbed after the queue: {before}");
        path.rtt = BASE_RTT + Duration::from_millis(60);
        let fell = kbps(path.run(TICK * 2));
        assert!(fell < before, "{fell} from {before}");
        path.rate = 800_000;
        let undone = path.run(Duration::from_secs(2));
        assert!(undone.capped);
        assert!(
            kbps(undone) >= before,
            "{} from {before} before the step",
            kbps(undone)
        );
    }

    /// A flat queue the sender has not backed off from is a queue, not a longer path.
    #[test]
    fn a_flat_queue_without_a_back_off_is_no_longer_path() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT + Duration::from_millis(60);
        let fell = path.run(TICK * 2);
        // The sender ignores the fall, and the queue holds.
        let held = path.run(Duration::from_secs(3));
        assert!(held.capped);
        assert!(
            kbps(held) <= kbps(fell),
            "{} from {}",
            kbps(held),
            kbps(fell)
        );
        assert!(held.queue_delay > Some(Duration::from_millis(50)));
    }

    #[test]
    fn a_flat_queue_that_drops_packets_lowers_the_estimate_further() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 20;
        let fell = path.run(TICK * 2);
        path.rate = 600_000;
        path.loss_percent = 15;
        let full = path.run(Duration::from_secs(4));
        assert!(kbps(full) < kbps(fell), "loss lowers it under a flat queue");
    }

    #[test]
    fn the_climb_speeds_up_well_past_the_last_queue() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(20_000_000);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        path.run(TICK * 2);
        path.rtt = BASE_RTT;
        let held = kbps(path.run(Duration::from_secs(1)));
        // Back to the 1 Mbit/s the path queued at, at 25% a second.
        let back = kbps(path.run(Duration::from_millis(800)));
        assert!(back >= 1_000 && held < 1_000, "{back} from {held}");
        // Near it, 10% a second.
        let near = kbps(path.tick());
        let near_next = kbps(path.run(Duration::from_secs(1)));
        assert!(near_next < near * 112 / 100, "{near_next} from {near}");
        // Past 1.25 times it, 25% a second again.
        path.run(Duration::from_secs(3));
        let far = kbps(path.tick());
        let far_next = kbps(path.run(Duration::from_secs(1)));
        assert!(far > 1_250, "{far}");
        assert!(far_next > far * 120 / 100, "{far_next} from {far}");
    }

    /// The far climb speeds up while it goes on, to about doubling each second.
    #[test]
    fn a_long_climb_speeds_up() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(1_000_000_000);
        path.rate = 100_000;
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        path.run(TICK * 2);
        path.rtt = BASE_RTT;
        // Past the hold and the near band.
        path.run(Duration::from_secs(4));
        let mut last = kbps(path.tick());
        let mut gains = Vec::new();
        for _ in 0..6 {
            let next = kbps(path.run(Duration::from_secs(1)));
            gains.push(next as f64 / last as f64);
            last = next;
        }
        assert!(gains[1] > gains[0] * 1.1, "{gains:?}");
        assert!(gains.iter().all(|gain| *gain <= 2.0), "{gains:?}");
        assert!(gains[5] > 1.9, "{gains:?}");
    }

    #[test]
    fn a_clean_path_climbs_toward_the_target_after_the_hold() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(2_000_000);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        let fell = kbps(path.run(TICK * 2));
        path.rtt = BASE_RTT;
        let held = kbps(path.run(Duration::from_millis(900)));
        assert_eq!(held, fell, "the estimate holds for a second after a fall");
        let climbed = kbps(path.run(Duration::from_secs(2)));
        // 25% a second back to the 1 Mbit/s it queued at, then 10% a second.
        assert!(climbed > 1_100, "{climbed} from {fell}");
        assert!(climbed < 1_200, "{climbed} from {fell}");
        let top = kbps(path.run(Duration::from_secs(60)));
        assert_eq!(top, 3_000, "1.5 times the target");
    }

    #[test]
    fn without_a_target_the_climb_stops_above_the_delivered_rate() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        path.run(TICK * 2);
        path.rtt = BASE_RTT;
        let top = kbps(path.run(Duration::from_secs(60)));
        assert_eq!(top, 1_510, "1.5 times 1 Mbit/s, and 10 kbit/s");
    }

    #[test]
    fn an_idle_sender_keeps_its_estimate() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        path.run(TICK * 2);
        path.rtt = BASE_RTT;
        let before = kbps(path.run(Duration::from_secs(1)));
        path.rate = 0;
        let after = kbps(path.run(Duration::from_secs(10)));
        assert!(after >= before, "{after} below {before}");
    }

    #[test]
    fn falls_in_a_row_lengthen_the_hold() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        let mut holds = Vec::new();
        for _ in 0..4 {
            path.rtt = BASE_RTT * 5;
            path.run(TICK * 2);
            path.rtt = BASE_RTT;
            let fell = kbps(path.tick());
            let mut held = TICK;
            while kbps(path.tick()) == fell {
                held += TICK;
            }
            holds.push(held);
        }
        assert!(holds.windows(2).all(|pair| pair[1] >= pair[0]), "{holds:?}");
        assert!(holds[2] > holds[1] && holds[1] > holds[0], "{holds:?}");
        assert!(holds[3] <= HOLD_MAX, "{holds:?}");
    }

    /// Falls on one congestion keep the hold at a second. Only a climb that ends in a fall doubles it.
    #[test]
    fn falls_without_a_climb_between_keep_the_hold() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(2_000_000);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        let first = kbps(path.run(TICK * 2));
        // The queue keeps growing past the sender's fall, so it falls again.
        path.rate = 600_000;
        let mut again = first;
        for _ in 0..30 {
            path.rtt += Duration::from_millis(15);
            again = kbps(path.tick());
        }
        assert!(again < first, "fell only once");
        assert_eq!(
            path.estimator.hold, HOLD_MIN,
            "no climb came between the falls"
        );
        path.rtt = BASE_RTT;
        let fell = kbps(path.tick());
        let mut held = TICK;
        while kbps(path.tick()) == fell {
            held += TICK;
        }
        assert!(held <= HOLD_MIN + TICK * 2, "held for {held:?}");
    }

    #[test]
    fn high_loss_lowers_the_estimate_and_moderate_loss_holds_it() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(2_000_000);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        path.run(TICK * 2);
        path.rtt = BASE_RTT;
        path.run(Duration::from_secs(2));
        path.loss_percent = 5;
        let start = kbps(path.run(Duration::from_secs(3)));
        let held = kbps(path.run(Duration::from_secs(5)));
        assert_eq!(held, start, "5% loss neither lowers nor raises");
        path.loss_percent = 20;
        let lowered = path.run(Duration::from_secs(3));
        assert!(
            kbps(lowered) < start * 95 / 100,
            "{} from {start}",
            kbps(lowered)
        );
        path.loss_percent = 60;
        let floor = path.run(Duration::from_secs(20));
        assert_eq!(kbps(floor), FLOOR / 1000, "a long burst stops at the floor");
    }

    /// Loss that stops with the fall it caused does not lower the estimate again.
    ///
    /// The loss rate spans two seconds, so it still reads high a second after
    /// the fall.
    #[test]
    fn a_burst_of_loss_lowers_the_estimate_once() {
        let mut path = Path::new(SendEstimate::App);
        path.target = Rate::from_bps(2_000_000);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        path.run(TICK * 2);
        path.rtt = BASE_RTT;
        let mut last = kbps(path.run(Duration::from_secs(3)));
        path.loss_percent = 60;
        let fell = loop {
            let now = kbps(path.tick());
            if now < last {
                break now;
            }
            last = now;
        };
        path.loss_percent = 0;
        let after = path.run(HOLD_MIN + TICK);
        assert!(
            after.loss > LOSS_DECREASE,
            "the window still holds the burst"
        );
        assert_eq!(kbps(after), fell, "the burst lowered it twice");
    }

    #[test]
    fn a_path_change_holds_the_estimate_through_the_settle_window() {
        let mut path = Path::new(SendEstimate::App);
        path.run(Duration::from_secs(3));
        path.rtt = BASE_RTT * 5;
        let fell = kbps(path.run(TICK * 2));
        // A longer path: its round trip has no baseline yet.
        path.id = PathId::MAX;
        path.rtt = BASE_RTT * 8;
        let moved = path.tick();
        assert_eq!(moved.path_generation, 1);
        let settled = path.run(DEFAULT_SETTLE - TICK * 2);
        assert_eq!(kbps(settled), fell, "no queue reads on a new path");
        // The longer round trip is the new baseline, not a queue.
        let after = path.run(Duration::from_secs(3));
        assert!(kbps(after) >= fell, "{} below {fell}", kbps(after));
    }

    #[test]
    fn the_controller_source_is_capped_by_the_estimator() {
        let mut path = Path::new(SendEstimate::Controller);
        path.controller = Some(5_000_000);
        let sample = path.run(Duration::from_secs(3));
        assert_eq!(
            kbps(sample),
            5_000,
            "the controller's estimate before congestion"
        );
        path.rtt = BASE_RTT * 5;
        let capped = path.run(TICK * 2);
        assert_eq!(kbps(capped), 850, "a stuck controller estimate is capped");
        path.controller = Some(500_000);
        assert_eq!(kbps(path.tick()), 500, "a lower controller estimate wins");
        path.controller = None;
        assert_eq!(kbps(path.tick()), 850, "the estimator stands in for none");
    }

    #[test]
    fn send_estimates_parse_from_their_names() {
        for estimate in SendEstimate::ALL {
            assert_eq!(estimate.to_string().parse(), Ok(estimate));
            assert_eq!(estimate.name().to_uppercase().parse(), Ok(estimate));
        }
        assert!("bbr".parse::<SendEstimate>().is_err());
    }
}
