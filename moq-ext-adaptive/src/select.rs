//! The pure part of rendition selection: which rendition should play.
//!
//! A player turns its catalog into a ranked `Vec<`[`Rung`]`>` and calls
//! [`choose`] on every pass of its selection loop. The loop itself, with its
//! channels and timers, stays with the caller.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use crate::{
    NetworkSample,
    bound::{Bound, Constraints, Rung},
};

/// How long a rendition whose decoder failed is left alone.
pub const EXCLUSION: Duration = Duration::from_secs(10);

/// How long after a track ended with no successor the target is asked for again.
pub const REVIVE_AFTER: Duration = Duration::from_secs(2);

/// How long a catalog may lack video before the video counts as over.
///
/// A publisher replacing its video removes the old renditions before it adds
/// the new ones. A closing publisher empties its catalog before it ends, just
/// before the next route in a route table serves the path. Both gaps are brief.
pub const VANISH_GRACE: Duration = Duration::from_secs(3);

/// How the rendition is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Follow the link, never above `max_height` rows when set (a grid tile).
    ///
    /// Without network signals, holds the best rendition that fits.
    Auto {
        /// The tallest rendition allowed.
        max_height: Option<u32>,
    },
    /// Hold the named rendition.
    ///
    /// If it cannot be played, plays as [`Auto`](Self::Auto) would, reports
    /// why, and returns to it when it becomes available.
    Pinned(String),
    /// Play no video, and leave audio unaffected (a tile scrolled off screen).
    Off,
}

impl Default for Mode {
    /// Automatic selection with no height limit.
    fn default() -> Self {
        Self::auto()
    }
}

impl Mode {
    /// Returns automatic selection with no height limit.
    pub const fn auto() -> Self {
        Self::Auto { max_height: None }
    }

    /// Returns automatic selection that never goes above `max_height`.
    pub const fn auto_below(max_height: u32) -> Self {
        Self::Auto {
            max_height: Some(max_height),
        }
    }

    /// Returns a mode holding the rendition `name`.
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
    /// Leaves `rendition` alone for [`EXCLUSION`] from `now`.
    pub fn fail(&mut self, rendition: &str, now: Instant) {
        self.0.insert(rendition.to_string(), now + EXCLUSION);
    }

    /// Returns the renditions left alone at `now`.
    pub fn excluded(&self, now: Instant) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Reports whether `rendition` is left alone at `now`.
    pub fn is_excluded(&self, rendition: &str, now: Instant) -> bool {
        self.0.get(rendition).is_some_and(|until| *until > now)
    }

    /// Reports whether any rendition is left alone at `now`.
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
/// `rungs` is the ladder, best first. `current` is the rendition last asked
/// for and `on_screen` the one playing. `sample` is the link, if the transport
/// measures one. Without it the best allowed rendition plays.
#[must_use]
#[allow(
    clippy::too_many_arguments,
    reason = "the rendition asked for and the one on screen are both weighed"
)]
pub fn choose(
    mode: &Mode,
    rungs: &[Rung],
    excluded: &BTreeSet<String>,
    current: Option<&str>,
    on_screen: Option<&str>,
    sample: Option<&NetworkSample>,
    bound: &mut Bound,
    now: Instant,
) -> Choice {
    let (max_height, pin) = match mode {
        Mode::Off => {
            return Choice {
                rendition: None,
                pin: None,
            };
        }
        Mode::Pinned(name) => {
            if excluded.contains(name) {
                (None, Some(PinProblem::Excluded(name.clone())))
            } else if !rungs.iter().any(|rung| rung.name == *name) {
                (None, Some(PinProblem::Unknown(name.clone())))
            } else {
                return Choice {
                    rendition: Some(name.clone()),
                    pin: None,
                };
            }
        }
        Mode::Auto { max_height } => (*max_height, None),
    };
    let constraints = Constraints {
        max_height,
        excluded: excluded.clone(),
    };
    let rendition = match sample {
        Some(sample) => bound.decide(rungs, current, on_screen, &constraints, sample, now),
        // Nothing to adapt to: the best rendition allowed plays.
        None => best(rungs, &constraints),
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
/// `rungs` is the ladder, best first. A switch to a rung further down, in
/// [`Mode::Auto`], is a step down. Such a switch does not overlap: on a
/// saturated link the incumbent's track would keep taking the bandwidth the
/// replacement needs.
#[must_use]
pub fn step_down(
    mode: &Mode,
    rungs: &[Rung],
    choice: Option<&str>,
    on_screen: Option<&str>,
) -> bool {
    let rank = |name: &str| rungs.iter().position(|rung| rung.name == name);
    matches!(mode, Mode::Auto { .. })
        && match (choice, on_screen) {
            (Some(choice), Some(playing)) => rank(choice) > rank(playing),
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Adaptation;

    fn rung(name: &str, height: u32, bitrate: u64) -> Rung {
        Rung {
            name: name.into(),
            bitrate: Some(bitrate),
            height: Some(height),
            stalled: false,
        }
    }

    fn ladder() -> Vec<Rung> {
        vec![rung("1080p", 1080, 4_000_000), rung("360p", 360, 500_000)]
    }

    fn pick(mode: &Mode, excluded: &[&str]) -> Choice {
        let excluded = excluded.iter().map(|name| name.to_string()).collect();
        let mut bound = Bound::new(Adaptation::default());
        choose(
            mode,
            &ladder(),
            &excluded,
            None,
            None,
            None,
            &mut bound,
            Instant::now(),
        )
    }

    #[test]
    fn auto_without_signals_plays_the_best() {
        assert_eq!(pick(&Mode::auto(), &[]).rendition.as_deref(), Some("1080p"));
    }

    #[test]
    fn auto_respects_the_height_limit() {
        let mode = Mode::auto_below(720);
        assert_eq!(pick(&mode, &[]).rendition.as_deref(), Some("360p"));
    }

    #[test]
    fn a_pin_plays_its_rendition() {
        let choice = pick(&Mode::Pinned("360p".into()), &[]);
        assert_eq!(choice.rendition.as_deref(), Some("360p"));
        assert_eq!(choice.pin, None);
    }

    /// A pin to a missing rendition falls back to automatic and says why.
    #[test]
    fn a_pin_to_a_missing_rendition_falls_back_and_says_why() {
        let choice = pick(&Mode::Pinned("4k".into()), &[]);
        assert_eq!(choice.rendition.as_deref(), Some("1080p"));
        assert_eq!(choice.pin, Some(PinProblem::Unknown("4k".into())));
    }

    #[test]
    fn a_pin_to_a_failing_rendition_falls_back_until_it_is_retried() {
        let choice = pick(&Mode::Pinned("1080p".into()), &["1080p"]);
        assert_eq!(choice.rendition.as_deref(), Some("360p"));
        assert_eq!(choice.pin, Some(PinProblem::Excluded("1080p".into())));
    }

    #[test]
    fn off_plays_nothing() {
        assert_eq!(pick(&Mode::Off, &[]).rendition, None);
    }

    #[test]
    fn a_failing_rendition_is_skipped_by_auto() {
        assert_eq!(
            pick(&Mode::auto(), &["1080p"]).rendition.as_deref(),
            Some("360p")
        );
    }

    #[test]
    fn backoffs_expire() {
        let now = Instant::now();
        let mut backoffs = Backoffs::default();
        assert!(!backoffs.any_excluded(now));
        backoffs.fail("1080p", now);
        assert!(backoffs.is_excluded("1080p", now));
        assert_eq!(backoffs.excluded(now).len(), 1);
        assert!(!backoffs.is_excluded("1080p", now + EXCLUSION));
        backoffs.clear();
        assert!(!backoffs.any_excluded(now));
    }

    #[test]
    fn step_down_needs_auto_and_a_lower_rung() {
        let rungs = ladder();
        assert!(step_down(
            &Mode::auto(),
            &rungs,
            Some("360p"),
            Some("1080p")
        ));
        assert!(!step_down(
            &Mode::auto(),
            &rungs,
            Some("1080p"),
            Some("360p")
        ));
        assert!(!step_down(&Mode::auto(), &rungs, Some("360p"), None));
        let pinned = Mode::Pinned("360p".into());
        assert!(!step_down(&pinned, &rungs, Some("360p"), Some("1080p")));
    }
}
