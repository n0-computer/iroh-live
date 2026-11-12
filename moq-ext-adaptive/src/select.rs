//! The pure part of rendition selection: which rendition should play.
//!
//! A player turns its catalog into a ranked `Vec<`[`Rung`]`>` and calls
//! [`choose`] on every pass of its selection loop. The loop itself, with its
//! channels and timers, stays with the caller.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use crate::{
    NetworkSample,
    bound::{Bound, Constraints, Rung},
};

/// How the rendition is chosen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RenditionMode {
    /// Follows the link.
    ///
    /// Without network signals, holds the best rendition that fits.
    #[default]
    Auto,
    /// Holds the named rendition.
    ///
    /// If it cannot be played, plays as [`Auto`](Self::Auto) would, reports
    /// why, and returns to it when it becomes available.
    Pinned(String),
    /// Plays no video, and leaves audio unaffected (a tile scrolled off screen).
    Off,
}

impl RenditionMode {
    /// Returns a mode holding the rendition `name`.
    #[must_use]
    pub fn pinned(name: impl Into<String>) -> Self {
        Self::Pinned(name.into())
    }
}

/// Why a pin could not be honoured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinProblem {
    /// The pinned rendition's decoder failed.
    ///
    /// The rendition is left alone until it is retried.
    Excluded(String),
    /// The ladder has no rendition of that name.
    Unknown(String),
}

/// What [`choose`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The rendition to play, or `None` for no video.
    pub rendition: Option<String>,
    /// Why a pin was not honoured, when one was not.
    pub pin: Option<PinProblem>,
}

/// The renditions whose decoders failed, and until when each is left alone.
#[derive(Debug, Default)]
pub struct Backoffs(BTreeMap<String, Instant>);

impl Backoffs {
    /// Leaves `rendition` alone until `until`.
    pub fn fail(&mut self, rendition: &str, until: Instant) {
        self.0.insert(rendition.to_string(), until);
    }

    /// Returns the renditions left alone at `now`.
    #[must_use]
    pub fn excluded(&self, now: Instant) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Reports whether `rendition` is left alone at `now`.
    #[must_use]
    pub fn is_excluded(&self, rendition: &str, now: Instant) -> bool {
        self.0.get(rendition).is_some_and(|until| *until > now)
    }

    /// Reports whether any rendition is left alone at `now`.
    #[must_use]
    pub fn any_excluded(&self, now: Instant) -> bool {
        self.0.values().any(|until| *until > now)
    }

    /// Forgets every failure, for a new decoder configuration.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Picks the rendition to play, and why a pin could not be honoured.
///
/// `rungs` is the ladder, most expensive first, and `constraints` rules
/// renditions out. `current` is the rendition last asked for and `on_screen`
/// the one playing. `sample` is the link, if the transport measures one.
/// Without it the best allowed rendition plays.
#[must_use]
#[allow(
    clippy::too_many_arguments,
    reason = "the rendition asked for and the one on screen are both weighed"
)]
pub fn choose(
    mode: &RenditionMode,
    rungs: &[Rung],
    constraints: &Constraints,
    current: Option<&str>,
    on_screen: Option<&str>,
    sample: Option<&NetworkSample>,
    bound: &mut Bound,
    now: Instant,
) -> Choice {
    let pin = match mode {
        RenditionMode::Off => {
            return Choice {
                rendition: None,
                pin: None,
            };
        }
        RenditionMode::Pinned(name) => {
            if constraints.excluded.contains(name) {
                Some(PinProblem::Excluded(name.clone()))
            } else if !rungs.iter().any(|rung| rung.name == *name) {
                Some(PinProblem::Unknown(name.clone()))
            } else {
                return Choice {
                    rendition: Some(name.clone()),
                    pin: None,
                };
            }
        }
        RenditionMode::Auto => None,
    };
    let rendition = match sample {
        Some(sample) => bound.decide(rungs, current, on_screen, constraints, sample, now),
        // Nothing to adapt to: the best rendition allowed plays.
        None => best(rungs, constraints),
    };
    Choice { rendition, pin }
}

/// Returns the best rendition the constraints allow, or the smallest if none is.
#[must_use]
pub fn best(rungs: &[Rung], constraints: &Constraints) -> Option<String> {
    rungs
        .iter()
        .find(|rung| constraints.allows(rung))
        .or_else(|| rungs.last())
        .map(|rung| rung.name.clone())
}

/// Reports whether automatic selection is stepping down from `on_screen`.
///
/// `rungs` is the ladder, most expensive first. A switch to a rung further
/// down, in [`RenditionMode::Auto`], is a step down. Such a switch does not
/// overlap: on a saturated link the incumbent's track would keep taking the
/// bandwidth the replacement needs.
#[must_use]
pub fn step_down(
    mode: &RenditionMode,
    rungs: &[Rung],
    choice: Option<&str>,
    on_screen: Option<&str>,
) -> bool {
    let rank = |name: &str| rungs.iter().position(|rung| rung.name == name);
    matches!(mode, RenditionMode::Auto)
        && match (choice, on_screen) {
            (Some(choice), Some(playing)) => rank(choice) > rank(playing),
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Adaptation;

    fn rung(name: &str, bitrate: u64) -> Rung {
        Rung {
            name: name.into(),
            bitrate: Some(bitrate),
            stalled: false,
        }
    }

    fn ladder() -> Vec<Rung> {
        vec![rung("1080p", 4_000_000), rung("360p", 500_000)]
    }

    fn pick(mode: &RenditionMode, excluded: &[&str]) -> Choice {
        let constraints = Constraints {
            excluded: excluded.iter().map(|name| name.to_string()).collect(),
        };
        let mut bound = Bound::new(Adaptation::default());
        choose(
            mode,
            &ladder(),
            &constraints,
            None,
            None,
            None,
            &mut bound,
            Instant::now(),
        )
    }

    #[test]
    fn auto_without_signals_plays_the_best() {
        assert_eq!(
            pick(&RenditionMode::Auto, &[]).rendition.as_deref(),
            Some("1080p")
        );
    }

    #[test]
    fn a_pin_plays_its_rendition() {
        let choice = pick(&RenditionMode::Pinned("360p".into()), &[]);
        assert_eq!(choice.rendition.as_deref(), Some("360p"));
        assert_eq!(choice.pin, None);
    }

    /// A pin to a missing rendition falls back to automatic and says why.
    #[test]
    fn a_pin_to_a_missing_rendition_falls_back_and_says_why() {
        let choice = pick(&RenditionMode::Pinned("4k".into()), &[]);
        assert_eq!(choice.rendition.as_deref(), Some("1080p"));
        assert_eq!(choice.pin, Some(PinProblem::Unknown("4k".into())));
    }

    #[test]
    fn a_pin_to_a_failing_rendition_falls_back_until_it_is_retried() {
        let choice = pick(&RenditionMode::Pinned("1080p".into()), &["1080p"]);
        assert_eq!(choice.rendition.as_deref(), Some("360p"));
        assert_eq!(choice.pin, Some(PinProblem::Excluded("1080p".into())));
    }

    #[test]
    fn off_plays_nothing() {
        assert_eq!(pick(&RenditionMode::Off, &[]).rendition, None);
    }

    #[test]
    fn a_failing_rendition_is_skipped_by_auto() {
        assert_eq!(
            pick(&RenditionMode::Auto, &["1080p"]).rendition.as_deref(),
            Some("360p")
        );
    }

    #[test]
    fn backoffs_expire() {
        let now = Instant::now();
        let mut backoffs = Backoffs::default();
        assert!(!backoffs.any_excluded(now));
        let until = now + std::time::Duration::from_secs(10);
        backoffs.fail("1080p", until);
        assert!(backoffs.is_excluded("1080p", now));
        assert_eq!(backoffs.excluded(now).len(), 1);
        assert!(!backoffs.is_excluded("1080p", until));
        backoffs.clear();
        assert!(!backoffs.any_excluded(now));
    }

    #[test]
    fn step_down_needs_auto_and_a_lower_rung() {
        let rungs = ladder();
        assert!(step_down(
            &RenditionMode::Auto,
            &rungs,
            Some("360p"),
            Some("1080p")
        ));
        assert!(!step_down(
            &RenditionMode::Auto,
            &rungs,
            Some("1080p"),
            Some("360p")
        ));
        assert!(!step_down(&RenditionMode::Auto, &rungs, Some("360p"), None));
        let pinned = RenditionMode::Pinned("360p".into());
        assert!(!step_down(&pinned, &rungs, Some("360p"), Some("1080p")));
    }

    #[test]
    fn the_default_mode_is_auto() {
        assert_eq!(RenditionMode::default(), RenditionMode::Auto);
        assert_eq!(
            pick(&RenditionMode::default(), &[]).rendition.as_deref(),
            Some("1080p")
        );
        assert_eq!(
            RenditionMode::pinned("360p"),
            RenditionMode::Pinned("360p".into())
        );
    }

    #[test]
    fn best_takes_the_first_allowed_rung_or_the_smallest() {
        let ladder = ladder();
        let none = Constraints::default();
        assert_eq!(best(&ladder, &none).as_deref(), Some("1080p"));
        let without_top = Constraints {
            excluded: ["1080p".to_string()].into(),
        };
        assert_eq!(best(&ladder, &without_top).as_deref(), Some("360p"));
        let without_any = Constraints {
            excluded: ["1080p".to_string(), "360p".to_string()].into(),
        };
        assert_eq!(best(&ladder, &without_any).as_deref(), Some("360p"));
        assert_eq!(best(&[], &none), None);
    }
}
