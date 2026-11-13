//! Rate control: the allocator a node's publications follow, and the estimate it divides.
//!
//! With [`NodeConfig::rate_control`](crate::NodeConfig::rate_control) set, the
//! node builds every transport it dials and accepts with the configured
//! [`SendEstimate`], and divides one estimate among the tracks of all its
//! publications through one [`Allocator`]. Every session shares that
//! allocator's registry, so each one takes the sum of the reservations as its
//! probe target, and the reservations count as probed. A ladder's upper
//! renditions can pause under it.
//!
//! The estimate the allocator divides is the [`FanOut`] policy's reading of
//! the node's links, refreshed every [`INTERVAL`].

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use moq_net::bandwidth::{self, Allocator, Rate};
use moq_transport_iroh::rate::{RateConfig, RateHandle};
pub use moq_transport_iroh::rate::{RateSample, SendEstimate, UnknownSendEstimate};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};

/// How often the node reads its links' estimates.
pub(crate) const INTERVAL: Duration = Duration::from_millis(100);

/// The delivered rate below which a link counts as carrying no media.
///
/// A link that only subscribes sends control messages, a few kbit/s.
const MEDIA_GOODPUT: Rate = Rate::from_kbps(32);

/// How a node's encoders follow what its links carry.
///
/// Off unless set: [`NodeConfig::rate_control`](crate::NodeConfig::rate_control)
/// is `None` by default, and every encoder then runs at its configured bitrate.
///
/// ```
/// use iroh_live_node::{NodeConfig, RateControl, SendEstimate};
///
/// let config = NodeConfig::default().with_rate_control(RateControl::new(SendEstimate::App));
/// assert!(config.rate_control.is_some());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RateControl {
    /// Where each link's send-rate estimate comes from.
    ///
    /// The default is [`SendEstimate::Controller`]: BBR3's model, capped by
    /// the application's estimator.
    pub estimate: SendEstimate,
    /// How a publication that several links read follows them.
    pub fan_out: FanOut,
    /// How long a link's estimate holds after its selected path changes.
    ///
    /// The default is two seconds.
    pub settle: Duration,
}

impl Default for RateControl {
    fn default() -> Self {
        Self::new(SendEstimate::Controller)
    }
}

impl RateControl {
    /// Returns rate control on `estimate`, with the default fan-out and settle window.
    #[must_use]
    pub fn new(estimate: SendEstimate) -> Self {
        Self {
            estimate,
            fan_out: FanOut::default(),
            settle: RateConfig::default().settle,
        }
    }

    /// Returns this rate control with `fan_out` dividing the links' estimates.
    ///
    /// See [`fan_out`](Self::fan_out).
    #[must_use]
    pub fn with_fan_out(mut self, fan_out: FanOut) -> Self {
        self.fan_out = fan_out;
        self
    }

    /// Returns this rate control with each link's estimate holding for `settle` after a path change.
    ///
    /// See [`settle`](Self::settle).
    #[must_use]
    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle = settle;
        self
    }

    /// Returns the transport config each link is built with.
    fn transport(&self) -> RateConfig {
        RateConfig::new(self.estimate).with_settle(self.settle)
    }
}

/// How one encoder follows the several links that read its broadcast.
///
/// A node's publications are served on every link whose peer may see them:
/// direct sessions and relay uplinks. Each link has its own estimate, and one
/// encoder per rendition has to pick one rate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FanOut {
    /// Treats the node's uplink as one pipe that every copy crosses.
    ///
    /// The estimate is the mean of the estimates of the links that carry
    /// media: on a shared uplink, the rate each copy can have. With one link,
    /// it is that link's. A peer behind a slow downlink pulls the mean down
    /// only in part, and its own player steps down a rendition for it.
    #[default]
    Shared,
}

impl FanOut {
    /// Returns the estimate the node's encoders follow, from its links' readings.
    ///
    /// `None` while no link has an estimate, which tells the encoders to hold.
    fn estimate(self, links: &[RateSample]) -> Option<Rate> {
        match self {
            Self::Shared => {
                let estimates = |media: bool| {
                    links.iter().filter_map(move |link| {
                        let carries = link.goodput.is_some_and(|rate| rate >= MEDIA_GOODPUT);
                        link.estimate.filter(|_| carries || !media)
                    })
                };
                // Links without media say nothing of what a copy can have,
                // unless no link carries any.
                let media: Vec<Rate> = estimates(true).collect();
                let counted = match media.is_empty() {
                    true => estimates(false).collect(),
                    false => media,
                };
                let count = counted.len() as u64;
                (count > 0).then(|| {
                    let sum: u128 = counted.iter().map(|rate| u128::from(rate.as_bps())).sum();
                    Rate::from_bps((sum / u128::from(count)) as u64)
                })
            }
        }
    }
}

/// A node's rate control: its allocator, and the links whose estimates it divides.
#[derive(derive_more::Debug)]
pub(crate) struct NodeRate {
    config: RateControl,
    #[debug(skip)]
    uplink: bandwidth::Producer,
    #[debug(skip)]
    allocator: Allocator,
    #[debug(skip)]
    links: Mutex<Vec<RateHandle>>,
}

impl NodeRate {
    /// Creates the rate control of a node, configured by `config`.
    pub(crate) fn new(config: RateControl) -> Self {
        let uplink = bandwidth::Producer::new();
        let allocator = Allocator::new(uplink.consume());
        Self {
            config,
            uplink,
            allocator,
            links: Mutex::new(Vec::new()),
        }
    }

    /// Returns the config the node builds each transport with.
    pub(crate) fn transport(&self) -> RateConfig {
        self.config.transport()
    }

    /// Returns the allocator the publications reserve on and every session shares.
    pub(crate) fn allocator(&self) -> Allocator {
        self.allocator.clone()
    }

    /// Counts `link`'s estimate from now on, until its connection closes.
    pub(crate) fn register(&self, link: Option<RateHandle>) {
        if let Some(link) = link {
            self.links.lock().expect("poisoned").push(link);
        }
    }

    /// Reads every link, and hands the fan-out's estimate to the allocator.
    fn update(&self) -> Option<Rate> {
        let samples: Vec<RateSample> = {
            let mut links = self.links.lock().expect("poisoned");
            links.retain(|link| !link.is_closed());
            links.iter().map(RateHandle::sample).collect()
        };
        let estimate = self.config.fan_out.estimate(&samples);
        trace!(
            links = samples.len(),
            estimate_kbps = estimate.map(|rate| rate.as_bps() / 1000),
            "node send estimate"
        );
        // Fails only once every consumer is gone, and the allocator holds one.
        let _ = self.uplink.set(estimate);
        estimate
    }
}

/// Reads the links of `rate` every [`INTERVAL`] until `shutdown`.
pub(crate) async fn run(rate: Arc<NodeRate>, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last = None;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            () = shutdown.cancelled() => return,
        }
        let estimate = rate.update();
        // Logs moves of a tenth or more, so a trace shows the estimate's course.
        let moved = match (last, estimate) {
            (Some(last), Some(now)) => {
                let (last, now): (Rate, Rate) = (last, now);
                last.as_bps().abs_diff(now.as_bps()) * 10 >= last.as_bps()
            }
            (last, now) => last.is_some() != now.is_some(),
        };
        if moved {
            debug!(
                estimate_kbps = estimate.map(|rate| rate.as_bps() / 1000),
                "node send estimate moved"
            );
            last = estimate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(estimate_kbps: u64, goodput_kbps: u64) -> RateSample {
        let mut sample = RateSample::default();
        sample.estimate = Some(Rate::from_kbps(estimate_kbps));
        sample.goodput = Some(Rate::from_kbps(goodput_kbps));
        sample
    }

    #[test]
    fn shared_is_the_mean_of_the_links_that_carry_media() {
        let shared = FanOut::Shared;
        assert_eq!(shared.estimate(&[]), None);
        assert_eq!(
            shared.estimate(&[link(2_000, 900)]),
            Some(Rate::from_kbps(2_000)),
            "one link is that link's"
        );
        assert_eq!(
            shared.estimate(&[link(2_000, 900), link(1_000, 900), link(9_000, 4)]),
            Some(Rate::from_kbps(1_500)),
            "a link without media does not count"
        );
        assert_eq!(
            shared.estimate(&[link(2_000, 4), link(1_000, 0)]),
            Some(Rate::from_kbps(1_500)),
            "every link counts while none carries media"
        );
    }
}
