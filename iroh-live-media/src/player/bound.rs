//! Rendition selection as a bound rather than a state machine.
//!
//! Each tick computes which renditions are allowed, and the best allowed one
//! wins:
//!
//! - The publisher's delivery estimate caps the bitrate. A rung fits while the
//!   estimate covers [`Adaptation::fit_ratio`] of its advertised bitrate.
//! - Sustained loss lowers a ceiling one rung at a time, and emergency loss
//!   drops it to the bottom. A clean stretch raises it one rung at a time.
//! - The caller's constraints exclude the rest: a height limit, a catalog
//!   `stalled` flag, and renditions whose decoders failed.
//! - When nothing fits, the smallest eligible rendition plays.
//!
//! Time enters only as hysteresis around that answer. A lower target has to
//! hold for [`Adaptation::downgrade_hold`] and a higher one for
//! [`Adaptation::upgrade_hold`]. No step up comes within
//! [`Adaptation::post_downgrade_cooldown`] of a step down.
//!
//! One ratio serves both directions. A publisher parked on a low rung has an
//! estimate limited by what it sends, and it can still climb back.
//!
//! A change of network path resets everything learned on the old one.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Duration,
};

use tokio::time::Instant;

/// One rendition, as the bound sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rung {
    /// The rendition's track name.
    pub name: String,
    /// The advertised bitrate in bits per second, if the catalog gives one.
    pub bitrate: Option<u64>,
    /// The coded height, if the catalog gives one.
    pub height: Option<u32>,
    /// Whether the publisher flagged the rendition to be avoided.
    pub stalled: bool,
}

/// What the caller rules out, whatever the network says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Constraints {
    /// No rendition taller than this, if set.
    pub max_height: Option<u32>,
    /// Renditions whose decoders recently failed.
    pub excluded: BTreeSet<String>,
}

impl Constraints {
    /// Reports whether `rung` is not stalled, not excluded and not too tall.
    pub(crate) fn allows(&self, rung: &Rung) -> bool {
        !rung.stalled
            && !self.excluded.contains(&rung.name)
            && match (self.max_height, rung.height) {
                (Some(max), Some(height)) => height <= max,
                _ => true,
            }
    }
}

/// One reading of the network, as the bound needs it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Reading {
    /// The fraction of packets lost, if measured.
    pub loss: Option<f64>,
    /// The publisher's delivery estimate in bits per second, if it sent one.
    pub delivery: Option<u64>,
    /// Bumped whenever the network path changes.
    pub path_generation: u64,
}

/// The player's adaptation thresholds and timers.
///
/// The defaults are tuned together on real and simulated links.
#[derive(Debug, Clone, Copy)]
pub struct Adaptation {
    /// The multiple of a rung's advertised bitrate the estimate has to cover.
    ///
    /// openh264 and VA-API were measured sending 82% of the advertised bitrate
    /// on the patchbay suite's picture. The estimate an iroh publisher sends is
    /// its congestion window over the round trip, and a capped link read at 0.8
    /// to 1.6 times its cap in the patchbay lab. The sliding maximum keeps the
    /// top of that range. At 1.25, a rung fits while the maximum covers 1.5
    /// times what the rung sends.
    pub fit_ratio: f64,
    /// How long the estimate is remembered, as a sliding maximum.
    ///
    /// The estimate refreshes ten times a second and moves with every
    /// acknowledgement. A single low reading does not mean a smaller link, so
    /// only a shortfall that lasts the whole window counts.
    pub estimate_window: Duration,
    /// Loss above which the ceiling steps down one rung, once held for
    /// [`downgrade_hold`](Self::downgrade_hold).
    pub loss_step_down: f64,
    /// Loss above which the ceiling drops to the bottom rung at once.
    pub loss_emergency: f64,
    /// How long a lower target has to hold before the switch.
    pub downgrade_hold: Duration,
    /// How long a higher target has to hold before the switch.
    ///
    /// Loss also has to stay clear this long before the loss ceiling rises a
    /// rung.
    pub upgrade_hold: Duration,
    /// How long after a step down no step up is taken.
    pub post_downgrade_cooldown: Duration,
    /// How long a rung has to play after a step up before the step counts as held.
    ///
    /// Each step down from a rung multiplies the hold before the next step up
    /// to it by four, up to [`upgrade_hold_max`](Self::upgrade_hold_max). A
    /// step up that plays this long clears the count. The estimate read on the
    /// rung below says little about the rung above, so this lets a marginal
    /// link settle on the rung it can carry.
    pub trial: Duration,
    /// The longest hold before a step up.
    pub upgrade_hold_max: Duration,
    /// How often the network is read while it can change the choice.
    pub tick: Duration,
    /// How long a replacement decoder has to take over before the switch is given up.
    ///
    /// It covers a real handover. The replacement subscribes to the other
    /// rendition, waits for its next keyframe, and decodes until it catches up
    /// with the picture on screen. On a two second GOP over an impaired link,
    /// the keyframe alone takes seconds. The incumbent keeps playing either
    /// way.
    pub switch_deadline: Duration,
}

impl Default for Adaptation {
    fn default() -> Self {
        Self {
            fit_ratio: 1.25,
            estimate_window: Duration::from_secs(1),
            loss_step_down: 0.10,
            loss_emergency: 0.20,
            downgrade_hold: Duration::from_millis(500),
            upgrade_hold: Duration::from_secs(4),
            post_downgrade_cooldown: Duration::from_secs(4),
            trial: Duration::from_secs(20),
            upgrade_hold_max: Duration::from_secs(120),
            tick: Duration::from_millis(200),
            switch_deadline: Duration::from_secs(15),
        }
    }
}

/// The selection state carried from one tick to the next.
#[derive(Debug, Default)]
pub(crate) struct Bound {
    adaptation: Adaptation,
    /// The path the history below was gathered on.
    path_generation: Option<u64>,
    /// Recent estimates, oldest first, for the sliding maximum.
    estimates: VecDeque<(Instant, u64)>,
    /// The index of the best rung loss allows, or `None` when loss allows all.
    loss_ceiling: Option<usize>,
    /// When loss above the step-down threshold began, if it is above it.
    lossy_since: Option<Instant>,
    /// When loss last went clear, if it is clear.
    clean_since: Option<Instant>,
    /// Since when the target has been below the rung playing.
    ///
    /// Timed for any lower rung, so a target that wavers between two lower
    /// rungs still reaches the hold.
    lower: Option<Instant>,
    /// Since when the target has been above the rung playing, likewise.
    higher: Option<Instant>,
    /// When the last step down landed, or was last seen still on its way.
    ///
    /// The cooldown runs from the landing. A switch needs a decoder and a
    /// keyframe to land, which takes seconds on an impaired link. A cooldown
    /// from the decision could end before the lower rung played.
    last_downgrade: Option<Instant>,
    /// Whether the last decision was a step down that has not landed yet.
    downgrading: bool,
    /// Whether a switch was on its way at the last decision.
    in_flight: bool,
    /// Failed tries at each rung, by name. See [`Adaptation::trial`].
    failed_tries: BTreeMap<String, u32>,
    /// The rung last stepped up to while its trial runs, and when it landed.
    trial: Option<(String, Option<Instant>)>,
}

impl Bound {
    /// Creates a bound with nothing learned yet.
    pub(crate) fn new(adaptation: Adaptation) -> Self {
        Self {
            adaptation,
            ..Self::default()
        }
    }

    /// Returns the hold before a step up to `rung`, lengthened by failed tries.
    fn upgrade_hold(&self, rung: &str) -> Duration {
        let tries = self.failed_tries.get(rung).copied().unwrap_or(0).min(8);
        self.adaptation
            .upgrade_hold
            .saturating_mul(4u32.pow(tries))
            .min(self.adaptation.upgrade_hold_max)
    }

    /// Returns the sliding maximum of the estimate, if there is one.
    fn estimate(&mut self, reading: &Reading, now: Instant) -> Option<u64> {
        if let Some(delivery) = reading.delivery {
            self.estimates.push_back((now, delivery));
        }
        while let Some((at, _)) = self.estimates.front()
            && now.duration_since(*at) > self.adaptation.estimate_window
        {
            self.estimates.pop_front();
        }
        self.estimates.iter().map(|(_, bps)| *bps).max()
    }

    /// Moves the loss ceiling on this reading.
    ///
    /// `current` is the index of the rung playing, and `lowest` the index of
    /// the bottom eligible rung. Sustained loss moves the ceiling one below
    /// `current`.
    fn follow_loss(&mut self, loss: f64, current: usize, lowest: usize, now: Instant) {
        if loss >= self.adaptation.loss_emergency {
            self.loss_ceiling = Some(lowest);
            self.lossy_since = None;
            self.clean_since = None;
            return;
        }
        if loss >= self.adaptation.loss_step_down {
            self.clean_since = None;
            let since = *self.lossy_since.get_or_insert(now);
            if now.duration_since(since) >= self.adaptation.downgrade_hold {
                let below = (current + 1).min(lowest);
                self.loss_ceiling = Some(
                    self.loss_ceiling
                        .map_or(below, |ceiling| ceiling.max(below)),
                );
                // A lasting loss steps down one rung per hold.
                self.lossy_since = Some(now);
            }
            return;
        }
        self.lossy_since = None;
        let Some(ceiling) = self.loss_ceiling else {
            self.clean_since = None;
            return;
        };
        let since = *self.clean_since.get_or_insert(now);
        if now.duration_since(since) >= self.adaptation.upgrade_hold {
            // One rung at a time, and gone once it reaches the top.
            self.loss_ceiling = ceiling.checked_sub(1);
            self.clean_since = Some(now);
        }
    }

    /// Picks the rendition to play.
    ///
    /// `ranked` is the catalog's video renditions, best first. `current` is the
    /// one last asked for, on screen or on its way, and the hold timers weigh
    /// the target against it. `on_screen` is the one playing. While the two
    /// differ, a switch is in flight and loss is not followed: the loss then is
    /// the old rendition's, and a step on it would stack onto the first.
    ///
    /// Returns `None` only when `ranked` is empty.
    pub(crate) fn decide(
        &mut self,
        ranked: &[Rung],
        current: Option<&str>,
        on_screen: Option<&str>,
        constraints: &Constraints,
        reading: &Reading,
        now: Instant,
    ) -> Option<String> {
        if self.path_generation != Some(reading.path_generation) {
            if self.path_generation.is_some() {
                tracing::debug!(
                    generation = reading.path_generation,
                    "the network path changed, forgetting what the old one taught",
                );
            }
            *self = Self {
                path_generation: Some(reading.path_generation),
                ..Self::new(self.adaptation)
            };
        }

        let eligible: Vec<usize> = ranked
            .iter()
            .enumerate()
            .filter(|(_, rung)| constraints.allows(rung))
            .map(|(index, _)| index)
            .collect();
        // With every rung ruled out, the smallest still plays.
        let Some(&lowest) = eligible.last() else {
            return ranked.last().map(|rung| rung.name.clone());
        };

        let current_index = current.and_then(|name| ranked.iter().position(|r| r.name == name));
        let estimate = self.estimate(reading, now);
        let in_flight = current.is_some() && current != on_screen;
        if in_flight {
            if self.downgrading {
                self.last_downgrade = Some(now);
            }
            // An emergency still drops to the bottom at once, overriding the
            // switch on its way.
            if reading
                .loss
                .is_some_and(|loss| loss >= self.adaptation.loss_emergency)
            {
                self.loss_ceiling = Some(lowest);
                self.lossy_since = None;
                self.clean_since = None;
            }
        } else {
            if std::mem::take(&mut self.in_flight) {
                // Landed. Loss from here on is the new rendition's, and a
                // lasting loss steps again one hold from now.
                self.downgrading = false;
                if self.lossy_since.is_some() {
                    self.lossy_since = Some(now);
                }
            }
            if let Some(loss) = reading.loss {
                self.follow_loss(loss, current_index.unwrap_or(0), lowest, now);
            }
            // A step up that landed starts its trial, and one that played
            // through it clears the rung's failed tries.
            if let Some((rung, landed)) = &mut self.trial {
                match landed {
                    None if on_screen == Some(rung.as_str()) => *landed = Some(now),
                    Some(at) if now.duration_since(*at) >= self.adaptation.trial => {
                        self.failed_tries.remove(rung.as_str());
                        self.trial = None;
                    }
                    _ => {}
                }
            }
        }
        self.in_flight = in_flight;

        let fits = |rung: &Rung| match (estimate, rung.bitrate) {
            (Some(estimate), Some(bitrate)) => {
                bitrate as f64 * self.adaptation.fit_ratio <= estimate as f64
            }
            _ => true,
        };
        let target_index = eligible
            .iter()
            .copied()
            .filter(|&index| self.loss_ceiling.is_none_or(|ceiling| index >= ceiling))
            .find(|&index| fits(&ranked[index]))
            .unwrap_or(lowest);
        let target = &ranked[target_index].name;

        // A rendition that is gone or no longer allowed is left at once.
        let Some(current_index) = current_index.filter(|index| eligible.contains(index)) else {
            self.lower = None;
            self.higher = None;
            return Some(target.clone());
        };

        match target_index.cmp(&current_index) {
            std::cmp::Ordering::Equal => {
                self.lower = None;
                self.higher = None;
                Some(ranked[current_index].name.clone())
            }
            std::cmp::Ordering::Greater => {
                self.higher = None;
                let emergency = reading
                    .loss
                    .is_some_and(|loss| loss >= self.adaptation.loss_emergency);
                let since = *self.lower.get_or_insert(now);
                if emergency || now.duration_since(since) >= self.adaptation.downgrade_hold {
                    self.lower = None;
                    self.last_downgrade = Some(now);
                    self.downgrading = true;
                    // The rung stepped down from counts as a failed try, and its
                    // trial ends.
                    let rung = ranked[current_index].name.clone();
                    if self.trial.as_ref().is_some_and(|(trial, _)| *trial == rung) {
                        self.trial = None;
                    }
                    let tries = self.failed_tries.entry(rung).or_default();
                    *tries += 1;
                    tracing::debug!(
                        rung = %ranked[current_index].name,
                        tries = *tries,
                        "stepped down; the next step up to this rung waits longer"
                    );
                    return Some(target.clone());
                }
                Some(ranked[current_index].name.clone())
            }
            std::cmp::Ordering::Less => {
                self.lower = None;
                let cooling = self.last_downgrade.is_some_and(|at| {
                    now.duration_since(at) < self.adaptation.post_downgrade_cooldown
                });
                let since = *self.higher.get_or_insert(now);
                if !cooling && now.duration_since(since) >= self.upgrade_hold(target) {
                    self.higher = None;
                    self.trial = Some((target.clone(), None));
                    return Some(target.clone());
                }
                Some(ranked[current_index].name.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rung(name: &str, bitrate: u64, height: u32) -> Rung {
        Rung {
            name: name.into(),
            bitrate: Some(bitrate),
            height: Some(height),
            stalled: false,
        }
    }

    fn ladder() -> Vec<Rung> {
        vec![
            rung("1080p", 4_000_000, 1080),
            rung("720p", 2_000_000, 720),
            rung("360p", 500_000, 360),
        ]
    }

    fn estimate(bps: u64) -> Reading {
        Reading {
            loss: Some(0.0),
            delivery: Some(bps),
            path_generation: 0,
        }
    }

    fn loss(loss: f64) -> Reading {
        Reading {
            loss: Some(loss),
            delivery: None,
            path_generation: 0,
        }
    }

    /// Decides once over `ranked` with no constraints.
    fn step(
        bound: &mut Bound,
        ranked: &[Rung],
        current: &str,
        on_screen: &str,
        reading: &Reading,
        now: Instant,
    ) -> String {
        bound
            .decide(
                ranked,
                Some(current),
                Some(on_screen),
                &Constraints::default(),
                reading,
                now,
            )
            .expect("the ladder is not empty")
    }

    /// Decides once over the ladder from nothing, under `constraints`.
    fn first(bound: &mut Bound, constraints: &Constraints, reading: &Reading) -> Option<String> {
        bound.decide(&ladder(), None, None, constraints, reading, Instant::now())
    }

    /// Runs `reading` every 100ms for `span`, feeding each decision back.
    ///
    /// Returns the rendition playing at the end, and the time.
    fn run(
        bound: &mut Bound,
        current: &str,
        reading: Reading,
        start: Instant,
        span: Duration,
    ) -> (String, Instant) {
        let ranked = ladder();
        let mut now = start;
        let mut playing = current.to_string();
        while now < start + span {
            playing = step(bound, &ranked, &playing, &playing, &reading, now);
            now += ms(100);
        }
        (playing, now)
    }

    #[test]
    fn the_best_fitting_rung_is_chosen_from_nothing() {
        let mut bound = Bound::default();
        let chosen = first(&mut bound, &Constraints::default(), &estimate(3_000_000));
        assert_eq!(chosen.as_deref(), Some("720p"));
    }

    #[test]
    fn a_shortfall_steps_down_after_the_hold() {
        let mut bound = Bound::default();
        let start = Instant::now();
        // Covers the fit ratio of 720p but not of 1080p.
        let (playing, _) = run(&mut bound, "1080p", estimate(3_750_000), start, ms(400));
        assert_eq!(playing, "1080p", "held for less than the downgrade hold");
        let (playing, _) = run(&mut bound, "1080p", estimate(3_750_000), start, ms(700));
        assert_eq!(playing, "720p");
    }

    /// An estimate that wanders just above the top rung's fit threshold steps up.
    #[test]
    fn an_application_limited_estimate_still_climbs_back() {
        let ranked = vec![rung("high", 800_000, 480), rung("low", 200_000, 240)];
        let mut bound = Bound::default();
        let mut playing = "low".to_string();
        let mut now = Instant::now();
        // Readings just above the top rung's fit threshold of 1 Mbit/s.
        let readings = [1_010_000, 1_075_000, 1_190_000, 1_020_000, 1_125_000];
        for tick in 0..60 {
            let reading = estimate(readings[tick % readings.len()]);
            playing = step(&mut bound, &ranked, &playing, &playing, &reading, now);
            now += ms(100);
        }
        assert_eq!(playing, "high");
    }

    #[test]
    fn a_step_up_waits_out_the_upgrade_hold() {
        let mut bound = Bound::default();
        let start = Instant::now();
        let (playing, _) = run(&mut bound, "360p", estimate(10_000_000), start, ms(3900));
        assert_eq!(playing, "360p");
        let (playing, _) = run(&mut bound, "360p", estimate(10_000_000), start, ms(4200));
        assert_eq!(
            playing, "1080p",
            "the best fitting rung wins, not the next one"
        );
    }

    #[test]
    fn no_step_up_inside_the_cooldown_after_a_step_down() {
        let mut bound = Bound::new(Adaptation {
            post_downgrade_cooldown: Duration::from_secs(10),
            ..Adaptation::default()
        });
        let start = Instant::now();
        let (playing, now) = run(&mut bound, "1080p", estimate(3_750_000), start, ms(1000));
        assert_eq!(playing, "720p");
        let (playing, _) = run(&mut bound, &playing, estimate(10_000_000), now, ms(6000));
        assert_eq!(playing, "720p", "inside the cooldown");
    }

    #[test]
    fn a_single_low_reading_inside_the_window_is_not_a_shortfall() {
        let mut bound = Bound::default();
        let ranked = ladder();
        let start = Instant::now();
        let mut playing = "1080p".to_string();
        for tick in 0..30u32 {
            // Nine readings that fit, then one that does not.
            let bps = match tick % 10 {
                9 => 1_250_000,
                _ => 7_500_000,
            };
            let now = start + ms(100) * tick;
            playing = step(&mut bound, &ranked, &playing, &playing, &estimate(bps), now);
        }
        assert_eq!(playing, "1080p");
    }

    #[test]
    fn emergency_loss_drops_to_the_bottom_at_once() {
        let mut bound = Bound::default();
        let chosen = step(
            &mut bound,
            &ladder(),
            "1080p",
            "1080p",
            &loss(0.25),
            Instant::now(),
        );
        assert_eq!(chosen, "360p");
    }

    #[test]
    fn sustained_loss_steps_down_one_rung_per_hold_and_recovers() {
        let mut bound = Bound::default();
        let start = Instant::now();
        let (playing, now) = run(&mut bound, "1080p", loss(0.12), start, ms(1100));
        assert_eq!(playing, "720p");
        let (playing, now) = run(&mut bound, &playing, loss(0.12), now, ms(1200));
        assert_eq!(playing, "360p");

        // Each rung stepped down from waits four times the usual hold before
        // it is tried again, so the climb back takes two of those.
        let (playing, _) = run(&mut bound, &playing, loss(0.0), now, ms(45_000));
        assert_eq!(playing, "1080p", "clean loss lifts the ceiling again");
    }

    /// Without an estimate, only loss holds a rendition back.
    #[test]
    fn no_estimate_leaves_only_loss() {
        let mut bound = Bound::default();
        let (playing, _) = run(&mut bound, "1080p", loss(0.0), Instant::now(), ms(10_000));
        assert_eq!(playing, "1080p");
    }

    #[test]
    fn the_height_limit_caps_the_choice() {
        let constraints = Constraints {
            max_height: Some(720),
            ..Constraints::default()
        };
        let chosen = first(&mut Bound::default(), &constraints, &estimate(100_000_000));
        assert_eq!(chosen.as_deref(), Some("720p"));
    }

    /// A rendition ruled out while it plays is left at once, without a hold.
    #[test]
    fn an_excluded_rendition_is_left_at_once() {
        let constraints = Constraints {
            excluded: BTreeSet::from(["1080p".to_string()]),
            ..Constraints::default()
        };
        let chosen = Bound::default().decide(
            &ladder(),
            Some("1080p"),
            Some("1080p"),
            &constraints,
            &estimate(100_000_000),
            Instant::now(),
        );
        assert_eq!(chosen.as_deref(), Some("720p"));
    }

    #[test]
    fn a_stalled_rendition_is_avoided() {
        let mut ranked = ladder();
        ranked[0].stalled = true;
        let chosen = Bound::default().decide(
            &ranked,
            None,
            None,
            &Constraints::default(),
            &estimate(100_000_000),
            Instant::now(),
        );
        assert_eq!(chosen.as_deref(), Some("720p"));
    }

    #[test]
    fn with_everything_ruled_out_the_smallest_still_plays() {
        let constraints = Constraints {
            max_height: Some(100),
            ..Constraints::default()
        };
        let chosen = first(&mut Bound::default(), &constraints, &estimate(100_000_000));
        assert_eq!(chosen.as_deref(), Some("360p"));
    }

    /// A new path drops the old path's estimates from the sliding maximum.
    #[test]
    fn a_new_path_forgets_the_old_one() {
        let mut bound = Bound::default();
        let start = Instant::now();
        // Plenty on path 0.
        let (playing, now) = run(&mut bound, "1080p", estimate(100_000_000), start, ms(2000));
        assert_eq!(playing, "1080p");
        // The path changes, and the new one only just carries 720p: the
        // downgrade is due after one hold, not after the old maximum ages.
        let fresh = Reading {
            path_generation: 1,
            ..estimate(2_500_000)
        };
        let (playing, _) = run(&mut bound, "1080p", fresh, now, ms(600));
        assert_eq!(playing, "720p", "the old path's estimate held the top rung");
    }

    /// A shortfall whose target wavers between two lower rungs steps down after one hold.
    #[test]
    fn a_wavering_lower_target_still_steps_down() {
        // Each reading stands alone, so the sliding maximum does not smooth
        // the waver away before the hold sees it.
        let mut bound = Bound::new(Adaptation {
            estimate_window: ms(1),
            ..Adaptation::default()
        });
        let ranked = ladder();
        let start = Instant::now();
        let mut playing = "1080p".to_string();
        for tick in 0..8u32 {
            // Alternately fits 720p and only 360p: 3 and 1.5 Mbit/s.
            let bps = if tick % 2 == 0 { 3_000_000 } else { 1_500_000 };
            let now = start + ms(100) * tick;
            playing = step(&mut bound, &ranked, &playing, &playing, &estimate(bps), now);
        }
        assert_ne!(playing, "1080p", "the hold restarted with every waver");
    }

    /// Sustained loss does not stack a second step while a switch is in flight.
    ///
    /// The next step comes one hold after the landing.
    #[test]
    fn loss_does_not_stack_steps_while_a_switch_is_in_flight() {
        let mut bound = Bound::default();
        let ranked = ladder();
        let lossy = loss(0.12);
        let start = Instant::now();
        let mut asked = "1080p".to_string();
        let mut now = start;
        // The loss holds. The first step's replacement takes three seconds to
        // land, and 1080p stays on screen meanwhile.
        while now < start + ms(3500) {
            asked = step(&mut bound, &ranked, &asked, "1080p", &lossy, now);
            now += ms(100);
        }
        assert_eq!(asked, "720p", "a second step was stacked on the first");
        // It lands, the loss goes on, and the next step comes after a hold.
        let landed = now;
        while now < landed + ms(400) {
            asked = step(&mut bound, &ranked, &asked, "720p", &lossy, now);
            now += ms(100);
        }
        assert_eq!(asked, "720p", "stepped again before a hold on the new rung");
        while now < landed + ms(1200) {
            asked = step(&mut bound, &ranked, &asked, "720p", &lossy, now);
            now += ms(100);
        }
        assert_eq!(asked, "360p");
    }

    /// The cooldown after a step down runs from the landing.
    #[test]
    fn the_cooldown_runs_from_the_landing() {
        let tuning = Adaptation::default();
        let mut bound = Bound::new(tuning);
        let ranked = ladder();
        let mut asked = "1080p".to_string();
        let mut now = Instant::now();
        // A shortfall steps down after the hold.
        while asked == "1080p" {
            asked = step(
                &mut bound,
                &ranked,
                &asked,
                "1080p",
                &estimate(3_750_000),
                now,
            );
            now += ms(100);
        }
        assert_eq!(asked, "720p");
        // The link recovers at once, but the switch takes five seconds to land.
        let decided = now;
        while now < decided + ms(5000) {
            asked = step(
                &mut bound,
                &ranked,
                &asked,
                "1080p",
                &estimate(100_000_000),
                now,
            );
            now += ms(100);
        }
        assert_eq!(asked, "720p", "stepped back up before the step down landed");
        let landed = now;
        while now < landed + tuning.post_downgrade_cooldown - ms(200) {
            asked = step(
                &mut bound,
                &ranked,
                &asked,
                "720p",
                &estimate(100_000_000),
                now,
            );
            now += ms(100);
        }
        assert_eq!(
            asked, "720p",
            "stepped up inside the cooldown after the landing"
        );
        // The step down counts against 1080p, so its next try waits longer
        // than the usual hold.
        let hold = bound.upgrade_hold("1080p");
        assert!(hold > tuning.upgrade_hold);
        while now < landed + tuning.post_downgrade_cooldown.max(hold) + ms(500) {
            asked = step(
                &mut bound,
                &ranked,
                &asked,
                "720p",
                &estimate(100_000_000),
                now,
            );
            now += ms(100);
        }
        assert_eq!(asked, "1080p", "never came back up");
    }

    /// Each step down from a rung multiplies the hold before the next try at it.
    ///
    /// The link carries 720p but not 1080p, and the estimate read on 720p says
    /// 1080p fits.
    #[test]
    fn failed_steps_up_back_off() {
        let mut bound = Bound::default();
        let ranked = ladder();
        let start = Instant::now();
        let mut playing = "720p".to_string();
        let mut now = start;
        let mut ups = Vec::new();
        // Plenty is read while 720p plays; 1080p, once it plays, is short.
        while now < start + Duration::from_secs(200) {
            let reading = match playing.as_str() {
                "1080p" => estimate(3_000_000),
                _ => estimate(100_000_000),
            };
            let next = step(&mut bound, &ranked, &playing, &playing, &reading, now);
            if next == "1080p" && playing != "1080p" {
                ups.push(now.duration_since(start));
            }
            playing = next;
            now += ms(100);
        }
        let gaps: Vec<Duration> = ups.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(ups.len() >= 3, "tried {} times: {ups:?}", ups.len());
        assert!(
            gaps.windows(2).all(|pair| pair[1] > pair[0] * 3),
            "the tries did not back off: {ups:?}"
        );
        assert!(
            ups.len() <= 5,
            "tried {} times in 200 s: {ups:?}",
            ups.len()
        );
    }

    /// A rung that holds through its trial gets the usual hold back.
    #[test]
    fn a_step_up_that_held_clears_its_failures() {
        let mut bound = Bound::default();
        bound.failed_tries.insert("1080p".into(), 3);
        bound.trial = Some(("1080p".into(), None));
        run(
            &mut bound,
            "1080p",
            estimate(100_000_000),
            Instant::now(),
            ms(25_000),
        );
        assert!(bound.failed_tries.is_empty(), "{:?}", bound.failed_tries);
        assert_eq!(
            bound.upgrade_hold("1080p"),
            Adaptation::default().upgrade_hold
        );
    }

    /// The loss ceiling is part of what a path taught: it goes with it.
    #[test]
    fn a_new_path_lifts_the_loss_ceiling() {
        let mut bound = Bound::default();
        step(
            &mut bound,
            &ladder(),
            "1080p",
            "1080p",
            &loss(0.25),
            Instant::now(),
        );
        assert!(bound.loss_ceiling.is_some());
        let fresh = Reading {
            path_generation: 1,
            ..loss(0.0)
        };
        let chosen = first(&mut bound, &Constraints::default(), &fresh);
        assert_eq!(chosen.as_deref(), Some("1080p"));
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }
}
