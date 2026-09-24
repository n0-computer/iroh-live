//! Rendition selection as a bound rather than a state machine.
//!
//! Each tick computes which renditions are allowed, and the best allowed one
//! wins:
//!
//! - The publisher's delivery estimate times a margin caps the bitrate. A rung
//!   fits while the estimate covers [`Tuning::fit_ratio`] of its advertised
//!   bitrate, the same figure for staying and for stepping up.
//! - Sustained loss lowers a ceiling one rung at a time, emergency loss drops it
//!   to the bottom, and a clean stretch raises it again one rung at a time.
//! - The caller's constraints exclude the rest: a height limit, a catalog
//!   `stalled` flag, and renditions whose decoders failed.
//! - The smallest eligible rendition is the fallback when nothing fits, so there
//!   is always an answer.
//!
//! Time enters only as hysteresis around that answer: a lower target has to
//! hold for [`Tuning::downgrade_hold`] and a higher one for
//! [`Tuning::upgrade_hold`], and no step up comes within
//! [`Tuning::post_downgrade_cooldown`] of a step down.
//!
//! This replaces the probe-and-headroom rule of the old `adaptive` module,
//! whose upgrade gate asked the estimate to cover one and a half times the next
//! rung's bitrate. Parked on a low rung, a publisher sends only that rung's bytes, so
//! its estimate is application-limited at a few times that rate and never
//! reached the gate: `adaptation_follows_a_real_link` sat at 380 to 490 kbit/s
//! against a 1.2 Mbit/s gate for a full minute with nothing wrong with the link.
//! One ratio for both directions, with the asymmetry moved into the timers, is
//! what lets the ladder climb back on a clear link.
//!
//! A change of network path resets everything learned so far, since history
//! from the old path says nothing about the new one.

use std::{
    collections::{BTreeSet, VecDeque},
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

/// The thresholds and timers. Internal, so they can be retuned in a patch.
#[derive(Debug, Clone)]
pub(crate) struct Tuning {
    /// The share of a rung's advertised bitrate the estimate has to cover for
    /// the rung to fit.
    ///
    /// Below one, and by this much, for two reasons that stack. An advertised
    /// bitrate is a ceiling handed to the encoder, and openh264 was measured
    /// sending about 40% of it. And the estimate an iroh publisher sends is its
    /// congestion window over the round trip, which under BBR3 sits a gain
    /// above the delivery rate: in the patchbay lab a 100 kbit/s cap read as
    /// 108 to 164. Both errors point the same way, so the threshold sits under
    /// them.
    pub fit_ratio: f64,
    /// How long the estimate is remembered, as a sliding maximum.
    ///
    /// The estimate is refreshed ten times a second and moves with every
    /// acknowledgement, so a single low reading is not a smaller link. A
    /// maximum over a second is what a sustained shortfall has to beat.
    pub estimate_window: Duration,
    /// Loss above which the ceiling steps down one rung, once held for
    /// [`downgrade_hold`](Self::downgrade_hold).
    pub loss_step_down: f64,
    /// Loss above which the ceiling drops to the bottom rung at once.
    pub loss_emergency: f64,
    /// How long a lower target has to hold before the switch.
    pub downgrade_hold: Duration,
    /// How long a higher target has to hold before the switch, and how long
    /// loss has to stay clear before the loss ceiling rises a rung.
    pub upgrade_hold: Duration,
    /// How long after a step down no step up is taken.
    pub post_downgrade_cooldown: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            fit_ratio: 0.5,
            estimate_window: Duration::from_secs(1),
            loss_step_down: 0.10,
            loss_emergency: 0.20,
            downgrade_hold: Duration::from_millis(500),
            upgrade_hold: Duration::from_secs(4),
            post_downgrade_cooldown: Duration::from_secs(4),
        }
    }
}

/// The selection state carried from one tick to the next.
#[derive(Debug)]
pub(crate) struct Bound {
    tuning: Tuning,
    /// The path the history below was gathered on.
    path_generation: Option<u64>,
    /// Recent estimates, oldest first, for the sliding maximum.
    estimates: VecDeque<(Instant, u64)>,
    /// The best rung loss allows, as an index into the ranked renditions, or
    /// `None` when loss allows everything.
    loss_ceiling: Option<usize>,
    /// When loss above the step-down threshold began, if it is above it.
    lossy_since: Option<Instant>,
    /// When loss last went clear, if it is clear.
    clean_since: Option<Instant>,
    /// Since when the target has been below the rung playing.
    ///
    /// Held for "any lower rung" rather than for one of them, so a target that
    /// wavers between two lower rungs under a noisy shortfall still reaches the
    /// hold; the switch goes to whichever is the target when it does.
    lower: Option<Instant>,
    /// Since when the target has been above the rung playing, likewise.
    higher: Option<Instant>,
    /// When the last step down landed, or the last time it was seen still on
    /// its way.
    ///
    /// The cooldown runs from the landing rather than the decision: a switch
    /// takes a decoder open and a keyframe to land, seconds over an impaired
    /// link, and a cooldown counted from the decision could be over before the
    /// lower rung ever played.
    last_downgrade: Option<Instant>,
    /// Whether the last decision was a step down that has not landed yet.
    downgrading: bool,
    /// Whether a switch was on its way at the last decision.
    in_flight: bool,
}

impl Bound {
    /// Creates a bound with nothing learned yet.
    pub(crate) fn new(tuning: Tuning) -> Self {
        Self {
            tuning,
            path_generation: None,
            estimates: VecDeque::new(),
            loss_ceiling: None,
            lossy_since: None,
            clean_since: None,
            lower: None,
            higher: None,
            last_downgrade: None,
            downgrading: false,
            in_flight: false,
        }
    }

    /// Forgets everything learned about the network.
    fn reset(&mut self) {
        self.estimates.clear();
        self.loss_ceiling = None;
        self.lossy_since = None;
        self.clean_since = None;
        self.lower = None;
        self.higher = None;
        self.last_downgrade = None;
        self.downgrading = false;
        self.in_flight = false;
    }

    /// Returns the sliding maximum of the estimate, if there is one.
    fn estimate(&mut self, reading: &Reading, now: Instant) -> Option<u64> {
        if let Some(delivery) = reading.delivery {
            self.estimates.push_back((now, delivery));
        }
        while let Some((at, _)) = self.estimates.front()
            && now.duration_since(*at) > self.tuning.estimate_window
        {
            self.estimates.pop_front();
        }
        self.estimates.iter().map(|(_, bps)| *bps).max()
    }

    /// Moves the loss ceiling on this reading.
    ///
    /// `current` is the index of the rung playing, which a sustained loss
    /// steps one below; `lowest` is the index of the bottom eligible rung.
    fn follow_loss(&mut self, loss: f64, current: usize, lowest: usize, now: Instant) {
        if loss >= self.tuning.loss_emergency {
            self.loss_ceiling = Some(lowest);
            self.lossy_since = None;
            self.clean_since = None;
            return;
        }
        if loss >= self.tuning.loss_step_down {
            self.clean_since = None;
            let since = *self.lossy_since.get_or_insert(now);
            if now.duration_since(since) >= self.tuning.downgrade_hold {
                let below = (current + 1).min(lowest);
                self.loss_ceiling = Some(
                    self.loss_ceiling
                        .map_or(below, |ceiling| ceiling.max(below)),
                );
                // A fresh hold for the next step, so a lasting loss walks the
                // ladder down one rung per hold rather than all at once.
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
        if now.duration_since(since) >= self.tuning.upgrade_hold {
            // One rung at a time, and gone once it reaches the top.
            self.loss_ceiling = ceiling.checked_sub(1);
            self.clean_since = Some(now);
        }
    }

    /// Picks the rendition to play.
    ///
    /// `ranked` is the catalog's video renditions, best first. `current` is the
    /// one last asked for, which is on screen or on its way, and the hold
    /// timers weigh the target against it. `on_screen` is the one actually
    /// playing: while the two differ a switch is in flight, and loss is not
    /// followed, since the loss measured then is the old rendition's and a
    /// step taken on it would pile a second step onto the first before the
    /// first could help. Returns `None` only when `ranked` is empty.
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
            self.reset();
            self.path_generation = Some(reading.path_generation);
        }

        let eligible: Vec<usize> = ranked
            .iter()
            .enumerate()
            .filter(|(_, rung)| {
                !rung.stalled
                    && !constraints.excluded.contains(&rung.name)
                    && match (constraints.max_height, rung.height) {
                        (Some(max), Some(height)) => height <= max,
                        _ => true,
                    }
            })
            .map(|(index, _)| index)
            .collect();
        // The guaranteed fallback: with every rung ruled out, the smallest
        // still plays rather than nothing.
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
            // An emergency still drops to the bottom at once: it overrides
            // whatever is on its way rather than stacking on it.
            if reading
                .loss
                .is_some_and(|loss| loss >= self.tuning.loss_emergency)
            {
                self.loss_ceiling = Some(lowest);
                self.lossy_since = None;
                self.clean_since = None;
            }
        } else {
            if std::mem::take(&mut self.in_flight) {
                // Landed: the loss seen from here on is the new rendition's,
                // and a lasting loss earns its next step one hold from now.
                self.downgrading = false;
                if self.lossy_since.is_some() {
                    self.lossy_since = Some(now);
                }
            }
            if let Some(loss) = reading.loss {
                self.follow_loss(loss, current_index.unwrap_or(0), lowest, now);
            }
        }
        self.in_flight = in_flight;

        let fits = |rung: &Rung| match (estimate, rung.bitrate) {
            (Some(estimate), Some(bitrate)) => {
                bitrate as f64 * self.tuning.fit_ratio <= estimate as f64
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

        // A rendition that is gone, or no longer allowed at all, is left at
        // once: there is nothing to wait for.
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
                    .is_some_and(|loss| loss >= self.tuning.loss_emergency);
                let since = *self.lower.get_or_insert(now);
                if emergency || now.duration_since(since) >= self.tuning.downgrade_hold {
                    self.lower = None;
                    self.last_downgrade = Some(now);
                    self.downgrading = true;
                    return Some(target.clone());
                }
                Some(ranked[current_index].name.clone())
            }
            std::cmp::Ordering::Less => {
                self.lower = None;
                let cooling = self
                    .last_downgrade
                    .is_some_and(|at| now.duration_since(at) < self.tuning.post_downgrade_cooldown);
                let since = *self.higher.get_or_insert(now);
                if !cooling && now.duration_since(since) >= self.tuning.upgrade_hold {
                    self.higher = None;
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

    fn ladder() -> Vec<Rung> {
        vec![
            Rung {
                name: "1080p".into(),
                bitrate: Some(4_000_000),
                height: Some(1080),
                stalled: false,
            },
            Rung {
                name: "720p".into(),
                bitrate: Some(2_000_000),
                height: Some(720),
                stalled: false,
            },
            Rung {
                name: "360p".into(),
                bitrate: Some(500_000),
                height: Some(360),
                stalled: false,
            },
        ]
    }

    fn estimate(bps: u64) -> Reading {
        Reading {
            loss: Some(0.0),
            delivery: Some(bps),
            path_generation: 0,
        }
    }

    /// Runs `reading` every 100ms from `start` for `span`, feeding each
    /// decision back as the rendition playing, and returns it with the time.
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
            playing = bound
                .decide(
                    &ranked,
                    Some(&playing),
                    Some(&playing),
                    &Constraints::default(),
                    &reading,
                    now,
                )
                .expect("the ladder is not empty");
            now += Duration::from_millis(100);
        }
        (playing, now)
    }

    #[test]
    fn the_best_fitting_rung_is_chosen_from_nothing() {
        let mut bound = Bound::new(Tuning::default());
        let chosen = bound.decide(
            &ladder(),
            None,
            None,
            &Constraints::default(),
            &estimate(1_200_000),
            Instant::now(),
        );
        assert_eq!(chosen.as_deref(), Some("720p"));
    }

    #[test]
    fn a_shortfall_steps_down_after_the_hold() {
        let mut bound = Bound::new(Tuning::default());
        let start = Instant::now();
        // Covers half of 720p but not half of 1080p.
        let (playing, _) = run(&mut bound, "1080p", estimate(1_500_000), start, ms(400));
        assert_eq!(playing, "1080p", "held for less than the downgrade hold");
        let (playing, _) = run(&mut bound, "1080p", estimate(1_500_000), start, ms(700));
        assert_eq!(playing, "720p");
    }

    /// The fix for the stalled upgrade gate. Parked on `low`, a publisher
    /// sends only `low`'s bytes, so its estimate is application-limited at a
    /// few times that. Measured in the patchbay lab, 380 to 490 kbit/s: under
    /// the old rule's 1.5 times the 800 kbit/s rung above, and over this rule's
    /// half of it.
    #[test]
    fn an_application_limited_estimate_still_climbs_back() {
        let ranked = vec![
            Rung {
                name: "high".into(),
                bitrate: Some(800_000),
                height: Some(480),
                stalled: false,
            },
            Rung {
                name: "low".into(),
                bitrate: Some(200_000),
                height: Some(240),
                stalled: false,
            },
        ];
        let mut bound = Bound::new(Tuning::default());
        let start = Instant::now();
        let mut playing = "low".to_string();
        let mut now = start;
        // Readings that wander across the range the lab showed.
        let readings = [380_000, 430_000, 490_000, 395_000, 450_000];
        for tick in 0..60 {
            let reading = estimate(readings[tick % readings.len()]);
            playing = bound
                .decide(
                    &ranked,
                    Some(&playing),
                    Some(&playing),
                    &Constraints::default(),
                    &reading,
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(playing, "high");
    }

    #[test]
    fn a_step_up_waits_out_the_upgrade_hold() {
        let mut bound = Bound::new(Tuning::default());
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
        let tuning = Tuning {
            post_downgrade_cooldown: Duration::from_secs(10),
            ..Tuning::default()
        };
        let mut bound = Bound::new(tuning);
        let start = Instant::now();
        let (playing, now) = run(&mut bound, "1080p", estimate(1_500_000), start, ms(1000));
        assert_eq!(playing, "720p");
        let (playing, _) = run(&mut bound, &playing, estimate(10_000_000), now, ms(6000));
        assert_eq!(playing, "720p", "inside the cooldown");
    }

    #[test]
    fn a_single_low_reading_inside_the_window_is_not_a_shortfall() {
        let mut bound = Bound::new(Tuning::default());
        let ranked = ladder();
        let start = Instant::now();
        let mut playing = "1080p".to_string();
        for tick in 0..30u32 {
            // Nine readings that fit, then one that does not.
            let bps = match tick % 10 {
                9 => 500_000,
                _ => 3_000_000,
            };
            playing = bound
                .decide(
                    &ranked,
                    Some(&playing),
                    Some(&playing),
                    &Constraints::default(),
                    &estimate(bps),
                    start + ms(100) * tick,
                )
                .expect("the ladder is not empty");
        }
        assert_eq!(playing, "1080p");
    }

    #[test]
    fn emergency_loss_drops_to_the_bottom_at_once() {
        let mut bound = Bound::new(Tuning::default());
        let reading = Reading {
            loss: Some(0.25),
            delivery: None,
            path_generation: 0,
        };
        let chosen = bound.decide(
            &ladder(),
            Some("1080p"),
            Some("1080p"),
            &Constraints::default(),
            &reading,
            Instant::now(),
        );
        assert_eq!(chosen.as_deref(), Some("360p"));
    }

    #[test]
    fn sustained_loss_steps_down_one_rung_per_hold_and_recovers() {
        let mut bound = Bound::new(Tuning::default());
        let start = Instant::now();
        let lossy = Reading {
            loss: Some(0.12),
            delivery: None,
            path_generation: 0,
        };
        let (playing, now) = run(&mut bound, "1080p", lossy, start, ms(1100));
        assert_eq!(playing, "720p");
        let (playing, now) = run(&mut bound, &playing, lossy, now, ms(1200));
        assert_eq!(playing, "360p");

        let clean = Reading {
            loss: Some(0.0),
            ..lossy
        };
        let (playing, _) = run(&mut bound, &playing, clean, now, ms(20_000));
        assert_eq!(playing, "1080p", "clean loss lifts the ceiling again");
    }

    /// Without an estimate there is no bandwidth bound, so a publisher that
    /// sends none is held back only by loss.
    #[test]
    fn no_estimate_leaves_only_loss() {
        let mut bound = Bound::new(Tuning::default());
        let reading = Reading {
            loss: Some(0.0),
            delivery: None,
            path_generation: 0,
        };
        let (playing, _) = run(&mut bound, "1080p", reading, Instant::now(), ms(10_000));
        assert_eq!(playing, "1080p");
    }

    #[test]
    fn the_height_limit_caps_the_choice() {
        let mut bound = Bound::new(Tuning::default());
        let constraints = Constraints {
            max_height: Some(720),
            ..Constraints::default()
        };
        let chosen = bound.decide(
            &ladder(),
            None,
            None,
            &constraints,
            &estimate(100_000_000),
            Instant::now(),
        );
        assert_eq!(chosen.as_deref(), Some("720p"));
    }

    /// A rendition ruled out while it plays is left at once, without a hold.
    #[test]
    fn an_excluded_rendition_is_left_at_once() {
        let mut bound = Bound::new(Tuning::default());
        let constraints = Constraints {
            excluded: BTreeSet::from(["1080p".to_string()]),
            ..Constraints::default()
        };
        let chosen = bound.decide(
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
        let mut bound = Bound::new(Tuning::default());
        let chosen = bound.decide(
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
        let mut bound = Bound::new(Tuning::default());
        let constraints = Constraints {
            max_height: Some(100),
            ..Constraints::default()
        };
        let chosen = bound.decide(
            &ladder(),
            None,
            None,
            &constraints,
            &estimate(100_000_000),
            Instant::now(),
        );
        assert_eq!(chosen.as_deref(), Some("360p"));
    }

    /// R16's fix made visible: a new path starts with no history. The old
    /// path's estimate would otherwise sit in the sliding maximum for a whole
    /// window and hold the top rung on a path that cannot carry it.
    #[test]
    fn a_new_path_forgets_the_old_one() {
        let mut bound = Bound::new(Tuning::default());
        let start = Instant::now();
        // Plenty on path 0.
        let (playing, now) = run(&mut bound, "1080p", estimate(100_000_000), start, ms(2000));
        assert_eq!(playing, "1080p");
        // The path changes, and the new one carries half of 720p's bitrate:
        // the downgrade is due after one hold, not after the old maximum ages.
        let fresh = Reading {
            path_generation: 1,
            ..estimate(1_000_000)
        };
        let (playing, _) = run(&mut bound, "1080p", fresh, now, ms(600));
        assert_eq!(playing, "720p", "the old path's estimate held the top rung");
    }

    /// S6: a shortfall whose target wavers between two lower rungs still
    /// steps down after one hold; timing each target separately never did.
    #[test]
    fn a_wavering_lower_target_still_steps_down() {
        // Each reading stands alone, so the sliding maximum does not smooth
        // the waver away before the hold sees it.
        let mut bound = Bound::new(Tuning {
            estimate_window: ms(1),
            ..Tuning::default()
        });
        let ranked = ladder();
        let start = Instant::now();
        let mut playing = "1080p".to_string();
        for tick in 0..8u32 {
            // Alternately fits 720p and only 360p: 1.2 and 0.6 Mbit/s.
            let bps = if tick % 2 == 0 { 1_200_000 } else { 600_000 };
            let now = start + ms(100) * tick;
            playing = bound
                .decide(
                    &ranked,
                    Some(&playing),
                    Some(&playing),
                    &Constraints::default(),
                    &estimate(bps),
                    now,
                )
                .expect("the ladder is not empty");
        }
        assert_ne!(playing, "1080p", "the hold restarted with every waver");
    }

    /// N3: with the rendition asked for fed back as current, sustained loss
    /// used to step the ceiling below the replacement still on its way every
    /// hold, superseding it before it could land. Loss is not followed while a
    /// switch is in flight, and the next step comes a hold after the landing.
    #[test]
    fn loss_does_not_stack_steps_while_a_switch_is_in_flight() {
        let mut bound = Bound::new(Tuning::default());
        let ranked = ladder();
        let lossy = Reading {
            loss: Some(0.12),
            delivery: None,
            path_generation: 0,
        };
        let start = Instant::now();
        let mut asked = "1080p".to_string();
        let mut now = start;
        // The loss holds; the first step's replacement takes three seconds
        // to land, and 1080p stays on screen meanwhile.
        while now < start + ms(3500) {
            asked = bound
                .decide(
                    &ranked,
                    Some(&asked),
                    Some("1080p"),
                    &Constraints::default(),
                    &lossy,
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(asked, "720p", "a second step was stacked on the first");
        // It lands, the loss goes on, and the next step comes after a hold.
        let landed = now;
        while now < landed + ms(400) {
            asked = bound
                .decide(
                    &ranked,
                    Some(&asked),
                    Some("720p"),
                    &Constraints::default(),
                    &lossy,
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(asked, "720p", "stepped again before a hold on the new rung");
        while now < landed + ms(1200) {
            asked = bound
                .decide(
                    &ranked,
                    Some(&asked),
                    Some("720p"),
                    &Constraints::default(),
                    &lossy,
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(asked, "360p");
    }

    /// N3: the cooldown after a step down used to run from the decision, so a
    /// switch that took longer than the cooldown to land could be taken back
    /// almost as soon as it played. It runs from the landing.
    #[test]
    fn the_cooldown_runs_from_the_landing() {
        let tuning = Tuning::default();
        let mut bound = Bound::new(tuning.clone());
        let ranked = ladder();
        let start = Instant::now();
        let mut asked = "1080p".to_string();
        let mut on_screen = "1080p".to_string();
        let mut now = start;
        // A shortfall steps down after the hold.
        while asked == "1080p" {
            asked = bound
                .decide(
                    &ranked,
                    Some(&asked),
                    Some(&on_screen),
                    &Constraints::default(),
                    &estimate(1_500_000),
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(asked, "720p");
        // The link recovers at once, but the switch takes five seconds to land.
        let decided = now;
        while now < decided + ms(5000) {
            asked = bound
                .decide(
                    &ranked,
                    Some(&asked),
                    Some(&on_screen),
                    &Constraints::default(),
                    &estimate(100_000_000),
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(asked, "720p", "stepped back up before the step down landed");
        on_screen = "720p".to_string();
        let landed = now;
        while now < landed + tuning.post_downgrade_cooldown - ms(200) {
            asked = bound
                .decide(
                    &ranked,
                    Some(&asked),
                    Some(&on_screen),
                    &Constraints::default(),
                    &estimate(100_000_000),
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(
            asked, "720p",
            "stepped up inside the cooldown after the landing"
        );
        while now < landed + tuning.post_downgrade_cooldown + tuning.upgrade_hold + ms(500) {
            asked = bound
                .decide(
                    &ranked,
                    Some(&asked),
                    Some(&on_screen),
                    &Constraints::default(),
                    &estimate(100_000_000),
                    now,
                )
                .expect("the ladder is not empty");
            now += ms(100);
        }
        assert_eq!(asked, "1080p", "never came back up");
    }

    /// The loss ceiling is part of what a path taught: it goes with it.
    #[test]
    fn a_new_path_lifts_the_loss_ceiling() {
        let mut bound = Bound::new(Tuning::default());
        let reading = Reading {
            loss: Some(0.25),
            delivery: None,
            path_generation: 0,
        };
        let now = Instant::now();
        bound.decide(
            &ladder(),
            Some("1080p"),
            Some("1080p"),
            &Constraints::default(),
            &reading,
            now,
        );
        assert!(bound.loss_ceiling.is_some());
        let fresh = Reading {
            loss: Some(0.0),
            delivery: None,
            path_generation: 1,
        };
        let chosen = bound.decide(&ladder(), None, None, &Constraints::default(), &fresh, now);
        assert_eq!(chosen.as_deref(), Some("1080p"));
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }
}
