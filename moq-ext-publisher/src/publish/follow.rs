//! A video rendition's rate control: its reservation and the grant it follows.
//!
//! A broadcast with an allocator, set by
//! [`LocalBroadcast::set_bandwidth`](crate::LocalBroadcast::set_bandwidth),
//! reserves each rendition's configured bitrate on it and follows the grant
//! through upstream's [`rate::Grant`]. Drops apply at once, rises ramp at 25%
//! a second, and a rendition above a lower one of the ladder pauses when its
//! grant falls below the band boundary while a session probes for it. Without
//! an allocator nothing is reserved, and every encoder runs at its configured
//! bitrate.

use moq_mux::rate::{self, Policy, Target};
use moq_net::bandwidth::{Allocator, Rate, Reservation};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::debug;

/// The allocator a broadcast's renditions reserve on, `None` while rate control is off.
pub(super) type Bandwidth = watch::Receiver<Option<Allocator>>;

/// The catalog producer of a broadcast, with the sections this crate adds.
pub(super) type CatalogProducer = moq_mux::catalog::Producer<Sections>;

/// The root sections this crate adds to a broadcast's catalog.
///
/// A catalog reader that does not know them ignores them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Sections {
    /// Present once the broadcast follows an allocator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate: Option<RateSection>,
}

impl moq_mux::catalog::hang::CatalogExt for Sections {}

/// The catalog's `rate` section: `{"probe": true}`.
///
/// It tells a player that the publisher's estimate is probed, and that it
/// pauses a rendition whose grant falls below its band boundary. The player
/// then keeps a rendition down to that boundary, computed from the catalog's
/// bitrates, instead of at its usual margin over the estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct RateSection {
    probe: bool,
}

/// Adds the `rate` section to `catalog`.
pub(super) fn advertise(catalog: &mut CatalogProducer) -> Result<(), moq_mux::Error> {
    let section = Some(RateSection { probe: true });
    if catalog.snapshot().ext.rate != section {
        catalog.modify()?.ext.rate = section;
    }
    Ok(())
}

/// Returns each ceiling's policy, with the next lower ceiling of the ladder as its `lower`.
///
/// A rendition without a known ceiling gets `None` and never follows.
pub(super) fn policies(ceilings: &[Option<Rate>]) -> Vec<Option<Policy>> {
    ceilings
        .iter()
        .map(|ceiling| {
            let max = (*ceiling)?;
            let mut policy = Policy::new(max);
            policy.lower = ceilings
                .iter()
                .flatten()
                .copied()
                .filter(|other| *other < max)
                .max();
            Some(policy)
        })
        .collect()
}

/// One rendition's reservation and the grant it follows.
pub(super) struct Follower {
    bandwidth: Bandwidth,
    policy: Option<Policy>,
    /// The reservation, and the track it was taken for.
    reservation: Option<(String, Reservation)>,
    /// The grant, followed while the encoder runs or the rendition is paused.
    grant: Option<rate::Grant>,
    /// What the encoder was last set to.
    target: Option<Target>,
    /// Whether the encoder takes a new bitrate. One that does not still pauses.
    retunable: bool,
}

impl std::fmt::Debug for Follower {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Follower")
            .field("policy", &self.policy)
            .field("following", &self.grant.is_some())
            .field("target", &self.target)
            .field("retunable", &self.retunable)
            .finish_non_exhaustive()
    }
}

impl Follower {
    /// Creates the follower of a rendition under `policy`, on the broadcast's `bandwidth`.
    pub(super) fn new(bandwidth: Bandwidth, policy: Option<Policy>) -> Self {
        Self {
            bandwidth,
            policy,
            reservation: None,
            grant: None,
            target: None,
            retunable: true,
        }
    }

    /// Returns the configured bitrate the rendition reserves.
    pub(super) fn ceiling(&self) -> Option<Rate> {
        self.policy.as_ref().map(|policy| policy.max)
    }

    /// Reports whether the grant paused the rendition.
    pub(super) fn paused(&self) -> bool {
        self.target == Some(Target::Disabled)
    }

    /// Returns the rendition's current slice of the estimate, `None` without one.
    pub(super) fn grant(&self) -> Option<Rate> {
        self.reservation.as_ref()?.1.rate()
    }

    /// Returns the bitrate the encoder was last set to, `None` while paused or not following.
    pub(super) fn target(&self) -> Option<Rate> {
        match self.target? {
            Target::Enabled(rate) => Some(rate),
            Target::Disabled => None,
        }
    }

    /// Reserves the rendition's ceiling for the track `demand` reads, unless it already has.
    pub(super) fn reserve(&mut self, demand: &moq_net::track::Demand) {
        if self
            .reservation
            .as_ref()
            .is_some_and(|(track, _)| track == demand.name())
        {
            return;
        }
        self.rebind(demand);
    }

    /// Takes a new reservation on the current allocator, and follows on from the current target.
    fn rebind(&mut self, demand: &moq_net::track::Demand) {
        let (Some(policy), Some(allocator)) = (&self.policy, self.bandwidth.borrow().clone())
        else {
            self.reservation = None;
            self.grant = None;
            return;
        };
        let reservation = allocator.reserve(demand, policy.max);
        if let Some(target) = self.target {
            let mut grant = rate::Grant::new(reservation.consume(), policy.clone());
            grant.assume(target);
            self.grant = Some(grant);
        }
        debug!(
            track = demand.name(),
            ceiling_kbps = policy.max.as_bps() / 1000,
            "reserved the rendition's bitrate"
        );
        self.reservation = Some((demand.name().to_string(), reservation));
    }

    /// Starts following the grant from the ceiling, as a freshly opened encoder runs.
    pub(super) fn start(&mut self) {
        let (Some(policy), Some((_, reservation))) = (&self.policy, &self.reservation) else {
            return;
        };
        self.grant = Some(rate::Grant::new(reservation.consume(), policy.clone()));
        self.target = Some(Target::Enabled(policy.max));
    }

    /// Stops following while nobody watches, unless the rendition is paused.
    ///
    /// A past viewer's grant is stale by the time the next viewer arrives, so
    /// the next encoder starts over at the ceiling. A paused rendition keeps
    /// following, since a player skips it and no viewer would come to resume it.
    pub(super) fn rest(&mut self) {
        if !self.paused() {
            self.grant = None;
            self.target = None;
        }
    }

    /// Waits for the next target, taking a new reservation when the allocator changes.
    ///
    /// Never returns while nothing is followed. Cancel safe.
    pub(super) async fn next(&mut self, demand: &moq_net::track::Demand) -> Target {
        loop {
            tokio::select! {
                changed = self.bandwidth.changed() => match changed {
                    Ok(()) => self.rebind(demand),
                    // The broadcast is gone, and the slot stops with it.
                    Err(_) => std::future::pending().await,
                },
                target = next_target(&mut self.grant) => match target {
                    Ok(target) => return target,
                    Err(err) => {
                        debug!(error = %err, "the grant ended, holding the bitrate");
                        self.grant = None;
                    }
                },
            }
        }
    }

    /// Records that the encoder runs at `target` now.
    pub(super) fn applied(&mut self, target: Target) {
        self.target = Some(target);
    }

    /// Takes back the last target, which the encoder could not apply.
    pub(super) fn failed(&mut self) {
        if let Some(grant) = &mut self.grant {
            grant.failed();
        }
    }

    /// Reports whether the encoder takes a new bitrate.
    pub(super) fn retunable(&self) -> bool {
        self.retunable
    }

    /// Records that the encoder cannot retune. The rendition still pauses and resumes.
    pub(super) fn unsupported(&mut self) {
        self.retunable = false;
    }
}

/// Waits for `grant`'s next target, or forever without one.
async fn next_target(grant: &mut Option<rate::Grant>) -> Result<Target, moq_net::Error> {
    match grant {
        Some(grant) => grant.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kbps(kbps: u64) -> Rate {
        Rate::from_kbps(kbps)
    }

    /// The wire form a player reads, which `moq-ext-player` pins too.
    #[test]
    fn the_rate_section_reads_probe_true() {
        let sections = Sections {
            rate: Some(RateSection { probe: true }),
        };
        assert_eq!(
            serde_json::to_string(&sections).expect("serializes"),
            r#"{"rate":{"probe":true}}"#
        );
        assert_eq!(
            serde_json::to_string(&Sections::default()).expect("serializes"),
            "{}",
            "a broadcast without rate control adds nothing"
        );
    }

    #[test]
    fn each_rung_pauses_above_the_next_lower_ceiling() {
        let policies = policies(&[Some(kbps(1_800)), None, Some(kbps(500)), Some(kbps(900))]);
        let lowers: Vec<_> = policies
            .iter()
            .map(|policy| policy.as_ref().map(|policy| policy.lower))
            .collect();
        assert_eq!(
            lowers,
            [
                Some(Some(kbps(900))),
                None,
                Some(None),
                Some(Some(kbps(500)))
            ]
        );
    }
}
