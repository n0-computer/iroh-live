//! The decode supervisor's state machine: one incumbent, at most one
//! replacement.
//!
//! A rendition switch or a decoder change opens a replacement decoder beside
//! the one playing and hands the picture over once the replacement has caught
//! up. Everything about that handover that can go wrong in an ordering lives
//! here, as plain transitions on plain data, so it can be driven step by step
//! from a test with no decoder, no network and no clock behind it.
//!
//! The rules, each of which fixes a way the previous supervisor went wrong:
//!
//! - There is at most one replacement. A new request supersedes it as a whole,
//!   open task and warm decoder alike, so a switch to C while B is warming never
//!   lands on B first.
//! - A replacement carries its target, a generation and a deadline from the
//!   moment it is requested until it takes over. The deadline covers the open as
//!   well as the first picture, so neither can wait forever.
//! - A replacement takes over once its playhead has caught up with the
//!   incumbent's, which is `@moq/watch`'s rule: the picture does not step
//!   backwards across a switch by more than [`CATCH_UP_SLACK`]. It waits for
//!   that only for [`CATCH_UP_PATIENCE`] after its first picture, though:
//!   both tracks share the link while it does, and a replacement asked for
//!   because the link cannot carry the incumbent may never catch up on it.
//!   Past the patience it takes over where it is, a step back in time being
//!   the lesser evil against a picture starved to a frame a second.
//! - A replacement takes over on a picture it decoded, never on opening alone:
//!   a decoder that opens and then stays silent keeps its deadline and is given
//!   up, rather than taking over a screen it never draws on. With nothing
//!   playing, including after the incumbent ended, its first picture is enough.
//! - A replacement that is given up is reported as given up, whether or not
//!   anything is playing, so the caller can tell a failed first open from the
//!   end of the video and try something else.

use std::time::Duration;

use tokio::time::Instant;

/// How far behind the incumbent's playhead a replacement may still be when it
/// takes over.
///
/// The same slack `@moq/watch` uses: it absorbs scheduling noise, so a switch
/// does not hinge on a picture landing inside one frame interval, and it is the
/// largest step backwards a switch can show.
pub(crate) const CATCH_UP_SLACK: Duration = Duration::from_millis(100);

/// How long after its first picture a replacement waits to catch up with the
/// incumbent before it takes over regardless.
///
/// Long enough for a replacement opened on a link with room, which catches up
/// within a group, and short against the switch deadline: under a saturated
/// link the two tracks starve each other for as long as they overlap, so the
/// overlap is what has to end.
pub(crate) const CATCH_UP_PATIENCE: Duration = Duration::from_secs(1);

/// What a decoder is built for: a rendition, under one decoder configuration.
///
/// The configuration is a generation rather than the settings themselves, so
/// that a decoder change for the rendition already playing is a different
/// target, and asking for the playing rendition again is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// The rendition's track name.
    pub rendition: String,
    /// Bumped whenever the decoder settings change.
    pub config: u64,
}

impl Target {
    /// Creates a target for `rendition` under decoder configuration `config`.
    pub(crate) fn new(rendition: impl Into<String>, config: u64) -> Self {
        Self {
            rendition: rendition.into(),
            config,
        }
    }
}

/// Why a replacement did not take over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Abandoned<E> {
    /// A newer request replaced it before it took over.
    Superseded,
    /// The request was withdrawn: the rendition asked for is the one playing.
    Withdrawn,
    /// Its decoder did not open.
    OpenFailed(E),
    /// Its track ended before it produced a picture it could take over with.
    Ended,
    /// It did not take over within the deadline.
    TimedOut,
}

/// What a transition did, for the caller to act on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome<E> {
    /// Nothing the caller has to act on.
    Idle,
    /// A replacement took over; the caller reports the new target as playing.
    Promoted(Target),
    /// A replacement was given up, and why.
    Abandoned(Target, Abandoned<E>),
    /// The incumbent ended with nothing on its way: the video has ended.
    Ended,
}

/// What to do with a picture the replacement decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Drop it: it is from a superseded replacement, or it is still behind the
    /// incumbent's playhead.
    Discard,
    /// Show it: the replacement has just taken over.
    Promote,
}

/// A replacement's progress.
#[derive(Debug)]
enum Phase<R, O> {
    /// Its decoder is opening, in the task `O`.
    Opening(O),
    /// Its decoder is open and decoding, but has not caught up yet.
    Warming(R),
}

/// The decoder waiting to take over.
#[derive(Debug)]
struct Replacement<R, O> {
    generation: u64,
    target: Target,
    deadline: Instant,
    phase: Phase<R, O>,
    /// When its first picture arrived, which its catch-up patience runs from.
    first_picture: Option<Instant>,
}

/// The decoder whose pictures are on screen.
#[derive(Debug)]
struct Playing<R> {
    target: Target,
    reader: R,
    /// The presentation time of the last picture shown, or `None` before the
    /// first.
    playhead: Option<Duration>,
}

/// One incumbent and at most one replacement.
///
/// `R` is a running decoder and `O` the task opening one, so a test drives the
/// transitions with stand-ins for both. Dropping a replacement drops its `O`
/// and its `R`, which is what cancels the work behind it.
#[derive(Debug)]
pub(crate) struct Switcher<R, O> {
    incumbent: Option<Playing<R>>,
    replacement: Option<Replacement<R, O>>,
    generations: u64,
    /// How long a replacement has, from its request to taking over.
    patience: Duration,
}

impl<R, O> Switcher<R, O> {
    /// Creates a switcher with nothing playing.
    ///
    /// `patience` is how long a replacement may take, from its request to
    /// taking over, before it is given up.
    pub(crate) fn new(patience: Duration) -> Self {
        Self {
            incumbent: None,
            replacement: None,
            generations: 0,
            patience,
        }
    }

    /// Returns the target playing, if any.
    pub(crate) fn current(&self) -> Option<&Target> {
        self.incumbent.as_ref().map(|playing| &playing.target)
    }

    /// Returns the target on its way in, if any.
    pub(crate) fn switching_to(&self) -> Option<&Target> {
        self.replacement
            .as_ref()
            .map(|replacement| &replacement.target)
    }

    /// Returns the moment the replacement is given up, if there is one.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.replacement
            .as_ref()
            .map(|replacement| replacement.deadline)
    }

    /// Returns the generation of the replacement, if there is one.
    pub(crate) fn replacement_generation(&self) -> Option<u64> {
        self.replacement
            .as_ref()
            .map(|replacement| replacement.generation)
    }

    /// Returns the running incumbent decoder.
    pub(crate) fn incumbent_mut(&mut self) -> Option<&mut R> {
        self.incumbent.as_mut().map(|playing| &mut playing.reader)
    }

    /// Returns the open task of a replacement that is still opening.
    pub(crate) fn opening_mut(&mut self) -> Option<&mut O> {
        match &mut self.replacement {
            Some(Replacement {
                phase: Phase::Opening(task),
                ..
            }) => Some(task),
            _ => None,
        }
    }

    /// Returns the decoder of a replacement that has opened.
    pub(crate) fn warming_mut(&mut self) -> Option<&mut R> {
        match &mut self.replacement {
            Some(Replacement {
                phase: Phase::Warming(reader),
                ..
            }) => Some(reader),
            _ => None,
        }
    }

    /// Reports whether nothing is playing and nothing is on its way.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        self.incumbent.is_none() && self.replacement.is_none()
    }

    /// Asks for `target`, starting at `now`.
    ///
    /// Asking for the target already playing withdraws a replacement, and
    /// asking for the one already on its way changes nothing. Anything else
    /// starts a replacement, calling `open` with its generation and target to
    /// build the task that opens its decoder, and supersedes the replacement
    /// before it.
    ///
    /// Returns the replacement this request withdrew or superseded, if any.
    pub(crate) fn request<E>(
        &mut self,
        target: Target,
        now: Instant,
        open: impl FnOnce(u64, &Target) -> O,
    ) -> Outcome<E> {
        if self.current() == Some(&target) {
            return match self.replacement.take() {
                Some(replacement) => Outcome::Abandoned(replacement.target, Abandoned::Withdrawn),
                None => Outcome::Idle,
            };
        }
        if self.switching_to() == Some(&target) {
            return Outcome::Idle;
        }
        self.generations += 1;
        let generation = self.generations;
        let task = open(generation, &target);
        let previous = self.replacement.replace(Replacement {
            generation,
            target,
            deadline: now + self.patience,
            phase: Phase::Opening(task),
            first_picture: None,
        });
        match previous {
            Some(previous) => Outcome::Abandoned(previous.target, Abandoned::Superseded),
            None => Outcome::Idle,
        }
    }

    /// Records that the replacement of `generation` finished opening.
    ///
    /// An opened replacement starts warming. It takes over on a picture, in
    /// [`replacement_frame`](Self::replacement_frame), and keeps its deadline
    /// until then.
    pub(crate) fn opened<E>(&mut self, generation: u64, result: Result<R, E>) -> Outcome<E> {
        let Some(replacement) = self
            .replacement
            .as_mut()
            .filter(|replacement| replacement.generation == generation)
        else {
            // A superseded open that finished anyway; dropping its decoder is
            // all there is to do.
            return Outcome::Idle;
        };
        match result {
            Ok(reader) => {
                replacement.phase = Phase::Warming(reader);
                Outcome::Idle
            }
            Err(err) => {
                let target = self.replacement.take().expect("matched above").target;
                Outcome::Abandoned(target, Abandoned::OpenFailed(err))
            }
        }
    }

    /// Records a picture the incumbent decoded at `pts`, which the caller
    /// shows.
    pub(crate) fn incumbent_frame(&mut self, pts: Duration) {
        if let Some(playing) = &mut self.incumbent {
            playing.playhead = Some(pts);
        }
    }

    /// Decides what to do with a picture the replacement decoded at `pts`,
    /// arriving at `now`.
    ///
    /// Promotes the replacement once `pts` has caught up with the incumbent's
    /// playhead, or once [`CATCH_UP_PATIENCE`] has passed since its first
    /// picture; the caller then shows this picture and drops whatever of the
    /// incumbent's it was about to show.
    pub(crate) fn replacement_frame<E>(
        &mut self,
        pts: Duration,
        now: Instant,
    ) -> (Verdict, Outcome<E>) {
        let Some(replacement) = &mut self.replacement else {
            return (Verdict::Discard, Outcome::Idle);
        };
        if !matches!(replacement.phase, Phase::Warming(_)) {
            return (Verdict::Discard, Outcome::Idle);
        }
        let first = *replacement.first_picture.get_or_insert(now);
        let caught_up = match self.incumbent.as_ref().and_then(|playing| playing.playhead) {
            Some(playhead) => pts + CATCH_UP_SLACK >= playhead,
            None => true,
        };
        if !caught_up {
            if now.duration_since(first) < CATCH_UP_PATIENCE {
                return (Verdict::Discard, Outcome::Idle);
            }
            let behind = self
                .incumbent
                .as_ref()
                .and_then(|playing| playing.playhead)
                .map(|playhead| playhead.saturating_sub(pts));
            tracing::info!(
                rendition = %replacement.target.rendition,
                ?behind,
                "replacement did not catch up in time, taking over where it is"
            );
        }
        let outcome = self.promote();
        if let Some(playing) = &mut self.incumbent {
            playing.playhead = Some(pts);
        }
        (Verdict::Promote, outcome)
    }

    /// Lets go of the incumbent, so a replacement that could not otherwise
    /// arrive takes over on its first picture.
    ///
    /// For a step down on a link that cannot carry the incumbent: the two
    /// would share the link while they overlap, and the replacement's groups
    /// age out before they arrive. Dropping the incumbent's reader drops its
    /// subscription. The caller keeps its last picture up.
    pub(crate) fn release_incumbent(&mut self) {
        self.incumbent = None;
    }

    /// Records that the incumbent's track ended.
    ///
    /// A replacement on its way keeps its deadline and takes over on its first
    /// picture, which no longer has anything to catch up with. Without one,
    /// the video has ended.
    pub(crate) fn incumbent_ended<E>(&mut self) -> Outcome<E> {
        self.incumbent = None;
        match &self.replacement {
            Some(_) => Outcome::Idle,
            None => Outcome::Ended,
        }
    }

    /// Records that the replacement's track ended before it took over.
    pub(crate) fn replacement_ended<E>(&mut self) -> Outcome<E> {
        match self.replacement.take() {
            Some(replacement) => Outcome::Abandoned(replacement.target, Abandoned::Ended),
            None => Outcome::Idle,
        }
    }

    /// Gives the replacement up if its deadline has passed at `now`.
    pub(crate) fn expire<E>(&mut self, now: Instant) -> Outcome<E> {
        match &self.replacement {
            Some(replacement) if replacement.deadline <= now => {
                let target = self.replacement.take().expect("matched above").target;
                Outcome::Abandoned(target, Abandoned::TimedOut)
            }
            _ => Outcome::Idle,
        }
    }

    /// Makes the warm replacement the incumbent.
    fn promote<E>(&mut self) -> Outcome<E> {
        let Some(Replacement {
            target,
            phase: Phase::Warming(reader),
            ..
        }) = self.replacement.take()
        else {
            unreachable!("only a warm replacement is promoted");
        };
        self.incumbent = Some(Playing {
            target: target.clone(),
            reader,
            playhead: None,
        });
        Outcome::Promoted(target)
    }
}

#[cfg(test)]
mod tests {
    //! The supervisor's transitions, driven one event at a time.
    //!
    //! Readers and open tasks are stand-ins: a reader is its name, and an open
    //! task is the generation it opens, so a test can see which of them the
    //! switcher kept and which it dropped.

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(15);

    type Test = Switcher<&'static str, u64>;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn target(rendition: &str) -> Target {
        Target::new(rendition, 0)
    }

    /// A switcher playing `high` with its playhead at `playhead`.
    fn playing(playhead: Duration) -> (Test, Instant) {
        let now = Instant::now();
        let mut switcher = Test::new(PATIENCE);
        let _: Outcome<()> = switcher.request(target("high"), now, |generation, _| generation);
        let outcome: Outcome<()> = switcher.opened(1, Ok("high"));
        assert_eq!(outcome, Outcome::Idle);
        assert_eq!(
            switcher.replacement_frame::<()>(playhead, Instant::now()),
            (Verdict::Promote, Outcome::Promoted(target("high")))
        );
        (switcher, now)
    }

    /// Asks `switcher` for `rendition` and returns the generation it opens.
    fn ask(switcher: &mut Test, rendition: &str, now: Instant) -> Outcome<()> {
        switcher.request(target(rendition), now, |generation, _| generation)
    }

    /// A first decoder takes over on its first picture, not on opening: one
    /// that opens and stays silent must not reach `Running` on a black screen.
    #[test]
    fn a_first_decoder_takes_over_on_its_first_picture() {
        let now = Instant::now();
        let mut switcher = Test::new(PATIENCE);
        assert_eq!(ask(&mut switcher, "high", now), Outcome::Idle);
        assert_eq!(switcher.opening_mut(), Some(&mut 1));
        assert_eq!(switcher.opened::<()>(1, Ok("high")), Outcome::Idle);
        assert_eq!(switcher.current(), None);
        assert_eq!(switcher.deadline(), Some(now + PATIENCE));
        assert_eq!(
            switcher.replacement_frame::<()>(ms(0), Instant::now()),
            (Verdict::Promote, Outcome::Promoted(target("high")))
        );
        assert_eq!(switcher.current(), Some(&target("high")));
        assert_eq!(switcher.switching_to(), None);
        assert_eq!(switcher.deadline(), None);
    }

    /// A first decoder that opens and never decodes is given up at its
    /// deadline, and reported as a failed switch rather than as the end.
    #[test]
    fn a_silent_first_decoder_times_out() {
        let now = Instant::now();
        let mut switcher = Test::new(PATIENCE);
        ask(&mut switcher, "high", now);
        switcher.opened::<()>(1, Ok("high"));
        assert_eq!(
            switcher.expire::<()>(now + PATIENCE),
            Outcome::Abandoned(target("high"), Abandoned::TimedOut)
        );
        assert!(switcher.is_idle());
    }

    /// R10: a request for C while B is still opening used to leave B in place,
    /// so B landed on its first frame and C replaced it afterwards. B is
    /// dropped with its open task, and its late result changes nothing.
    #[test]
    fn a_newer_request_supersedes_an_opening_replacement() {
        let (mut switcher, now) = playing(ms(1000));
        assert_eq!(ask(&mut switcher, "mid", now), Outcome::Idle);
        let mid = switcher.replacement_generation().expect("mid is opening");

        assert_eq!(
            ask(&mut switcher, "low", now),
            Outcome::Abandoned(target("mid"), Abandoned::Superseded)
        );
        let low = switcher.replacement_generation().expect("low is opening");
        assert_ne!(mid, low);

        // The superseded open finishes anyway.
        assert_eq!(switcher.opened::<()>(mid, Ok("mid")), Outcome::Idle);
        assert_eq!(switcher.switching_to(), Some(&target("low")));
        assert!(switcher.warming_mut().is_none(), "mid must not warm up");
    }

    /// The same for a replacement that has opened and is decoding: it goes
    /// whole, decoder included.
    #[test]
    fn a_newer_request_supersedes_a_warming_replacement() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "mid", now);
        switcher.opened::<()>(2, Ok("mid"));
        assert_eq!(switcher.warming_mut(), Some(&mut "mid"));

        assert_eq!(
            ask(&mut switcher, "low", now),
            Outcome::Abandoned(target("mid"), Abandoned::Superseded)
        );
        assert!(switcher.warming_mut().is_none());
        assert_eq!(switcher.opening_mut(), Some(&mut 3));
    }

    #[test]
    fn asking_for_the_replacement_again_changes_nothing() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        assert_eq!(ask(&mut switcher, "low", now), Outcome::Idle);
        assert_eq!(
            switcher.opening_mut(),
            Some(&mut 2),
            "the open was restarted"
        );
    }

    /// Asking for what is playing is how a pin or an un-pin withdraws a switch
    /// that has not landed.
    #[test]
    fn asking_for_the_playing_rendition_withdraws_the_replacement() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        assert_eq!(
            ask(&mut switcher, "high", now),
            Outcome::Abandoned(target("low"), Abandoned::Withdrawn)
        );
        assert_eq!(switcher.switching_to(), None);
        assert_eq!(switcher.current(), Some(&target("high")));
    }

    /// Regression, carried over from the old supervisor: a decoder change is a
    /// replacement for the rendition already playing, and pinning that
    /// rendition while the new decoder comes up must not throw the change away.
    #[test]
    fn a_decoder_change_survives_repinning_the_playing_rendition() {
        let (mut switcher, now) = playing(ms(1000));
        let rebuild = Target::new("high", 1);
        let _: Outcome<()> = switcher.request(rebuild.clone(), now, |generation, _| generation);
        assert_eq!(switcher.switching_to(), Some(&rebuild));

        // The pin names the rendition under the configuration now asked for.
        let _: Outcome<()> = switcher.request(rebuild.clone(), now, |generation, _| generation);
        assert_eq!(switcher.switching_to(), Some(&rebuild));
    }

    /// The `@moq/watch` rule: a replacement that opened at an older keyframe
    /// decodes its way forward in silence, and takes over only once it has
    /// caught up with the picture on screen.
    #[test]
    fn a_replacement_behind_the_playhead_is_held_back() {
        let (mut switcher, now) = playing(ms(10_000));
        ask(&mut switcher, "low", now);
        switcher.opened::<()>(2, Ok("low"));

        for behind in [ms(8000), ms(9000), ms(9899)] {
            assert_eq!(
                switcher.replacement_frame::<()>(behind, Instant::now()),
                (Verdict::Discard, Outcome::Idle),
                "a picture at {behind:?} would step back from 10s"
            );
        }
        assert_eq!(switcher.current(), Some(&target("high")));

        // Within the slack of the incumbent's playhead: close enough.
        assert_eq!(
            switcher.replacement_frame::<()>(ms(9900), Instant::now()),
            (Verdict::Promote, Outcome::Promoted(target("low")))
        );
        assert_eq!(switcher.current(), Some(&target("low")));
        assert_eq!(switcher.incumbent_mut(), Some(&mut "low"));
    }

    /// Under a saturated link a replacement behind the playhead may never
    /// catch up, and both tracks starve while they overlap. Past its patience
    /// it takes over where it is.
    #[test]
    fn a_replacement_that_cannot_catch_up_takes_over_after_its_patience() {
        let (mut switcher, now) = playing(ms(10_000));
        ask(&mut switcher, "low", now);
        switcher.opened::<()>(2, Ok("low"));
        let first = now + ms(500);
        assert_eq!(
            switcher.replacement_frame::<()>(ms(8000), first).0,
            Verdict::Discard
        );
        assert_eq!(
            switcher
                .replacement_frame::<()>(ms(8100), first + CATCH_UP_PATIENCE - ms(1))
                .0,
            Verdict::Discard
        );
        assert_eq!(
            switcher.replacement_frame::<()>(ms(8200), first + CATCH_UP_PATIENCE),
            (Verdict::Promote, Outcome::Promoted(target("low")))
        );
        assert_eq!(switcher.current(), Some(&target("low")));
    }

    /// A step down on a starved link lets go of the incumbent: the
    /// replacement takes over on its first picture, with nothing to catch up
    /// with, and keeps its deadline until then.
    #[test]
    fn a_released_incumbent_leaves_the_replacement_to_take_over() {
        let (mut switcher, now) = playing(ms(10_000));
        ask(&mut switcher, "low", now);
        switcher.release_incumbent();
        assert_eq!(switcher.current(), None);
        assert_eq!(switcher.deadline(), Some(now + PATIENCE));
        switcher.opened::<()>(2, Ok("low"));
        assert_eq!(
            switcher.replacement_frame::<()>(ms(2000), now),
            (Verdict::Promote, Outcome::Promoted(target("low")))
        );
    }

    /// The playhead the replacement is held to moves with the incumbent.
    #[test]
    fn the_catch_up_bar_follows_the_incumbent() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        switcher.opened::<()>(2, Ok("low"));
        switcher.incumbent_frame(ms(2000));
        assert_eq!(
            switcher.replacement_frame::<()>(ms(1500), Instant::now()).0,
            Verdict::Discard
        );
        assert_eq!(
            switcher.replacement_frame::<()>(ms(2000), Instant::now()).0,
            Verdict::Promote
        );
    }

    /// A replacement ahead of the incumbent takes over on its first picture.
    #[test]
    fn a_replacement_ahead_of_the_playhead_takes_over_at_once() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        switcher.opened::<()>(2, Ok("low"));
        assert_eq!(
            switcher.replacement_frame::<()>(ms(1200), Instant::now()),
            (Verdict::Promote, Outcome::Promoted(target("low")))
        );
    }

    /// Pictures from a replacement that is still opening, or from none at all,
    /// are dropped rather than shown.
    #[test]
    fn only_a_warm_replacement_can_take_over() {
        let (mut switcher, now) = playing(ms(1000));
        assert_eq!(
            switcher.replacement_frame::<()>(ms(5000), Instant::now()).0,
            Verdict::Discard
        );
        ask(&mut switcher, "low", now);
        assert_eq!(
            switcher.replacement_frame::<()>(ms(5000), Instant::now()).0,
            Verdict::Discard
        );
    }

    /// R10: the incumbent ending used to promote a replacement without a
    /// frame and drop its deadline. A warm one now takes over on its next
    /// picture, whatever its playhead, since nothing is left to step back from.
    #[test]
    fn the_incumbent_ending_hands_over_on_the_next_picture() {
        let (mut switcher, now) = playing(ms(10_000));
        ask(&mut switcher, "low", now);
        switcher.opened::<()>(2, Ok("low"));
        assert_eq!(switcher.incumbent_ended::<()>(), Outcome::Idle);
        assert_eq!(switcher.current(), None);
        assert_eq!(switcher.deadline(), Some(now + PATIENCE));
        assert_eq!(
            switcher.replacement_frame::<()>(ms(1000), Instant::now()),
            (Verdict::Promote, Outcome::Promoted(target("low")))
        );
        assert_eq!(switcher.current(), Some(&target("low")));
        assert_eq!(switcher.deadline(), None);
    }

    #[test]
    fn the_incumbent_ending_keeps_an_opening_replacement_on_its_deadline() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        assert_eq!(switcher.incumbent_ended::<()>(), Outcome::Idle);
        assert_eq!(switcher.current(), None);
        assert_eq!(switcher.deadline(), Some(now + PATIENCE));

        // With nothing playing, the replacement takes over on its first
        // picture.
        assert_eq!(switcher.opened::<()>(2, Ok("low")), Outcome::Idle);
        assert_eq!(
            switcher.replacement_frame::<()>(ms(0), Instant::now()).1,
            Outcome::Promoted(target("low"))
        );
    }

    #[test]
    fn the_incumbent_ending_alone_ends_the_video() {
        let (mut switcher, _) = playing(ms(1000));
        assert_eq!(switcher.incumbent_ended::<()>(), Outcome::Ended);
        assert!(switcher.is_idle());
    }

    /// R10: the open phase had no deadline, so an open that never returned was
    /// waited on for the rest of the session. The deadline runs from the
    /// request and covers the open.
    #[test]
    fn a_replacement_that_never_opens_times_out() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        assert_eq!(switcher.expire::<()>(now + PATIENCE - ms(1)), Outcome::Idle);
        assert_eq!(
            switcher.expire::<()>(now + PATIENCE),
            Outcome::Abandoned(target("low"), Abandoned::TimedOut)
        );
        assert_eq!(switcher.opening_mut(), None, "the open task was dropped");
        assert_eq!(switcher.current(), Some(&target("high")));
    }

    /// And the same deadline covers catching up, which a replacement stuck
    /// behind the playhead would otherwise do forever.
    #[test]
    fn a_replacement_that_never_catches_up_times_out() {
        let (mut switcher, now) = playing(ms(60_000));
        ask(&mut switcher, "low", now);
        switcher.opened::<()>(2, Ok("low"));
        switcher.replacement_frame::<()>(ms(1000), Instant::now());
        assert_eq!(
            switcher.expire::<()>(now + PATIENCE),
            Outcome::Abandoned(target("low"), Abandoned::TimedOut)
        );
        assert!(switcher.warming_mut().is_none());
    }

    /// A deadline carried over from a superseded replacement would give the
    /// newer one less time than it is owed.
    #[test]
    fn a_superseding_request_starts_its_own_deadline() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "mid", now);
        let later = now + ms(10_000);
        ask(&mut switcher, "low", later);
        assert_eq!(switcher.deadline(), Some(later + PATIENCE));
    }

    #[test]
    fn a_failed_open_is_reported_and_leaves_the_incumbent_playing() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        assert_eq!(
            switcher.opened(2, Err("no such track")),
            Outcome::Abandoned(target("low"), Abandoned::OpenFailed("no such track"))
        );
        assert_eq!(switcher.current(), Some(&target("high")));
        assert_eq!(switcher.switching_to(), None);
    }

    #[test]
    fn a_replacement_whose_track_ends_is_reported() {
        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        switcher.opened::<()>(2, Ok("low"));
        assert_eq!(
            switcher.replacement_ended::<()>(),
            Outcome::Abandoned(target("low"), Abandoned::Ended)
        );
        assert_eq!(switcher.current(), Some(&target("high")));
    }

    /// C2: a replacement that fails with nothing playing used to report the
    /// end of the video, so a first decoder that would not open left the
    /// player `Ended` with no error and no fallback. It is a failed switch.
    #[test]
    fn a_failure_with_nothing_playing_is_a_failed_switch() {
        let now = Instant::now();
        let mut switcher = Test::new(PATIENCE);
        ask(&mut switcher, "high", now);
        assert_eq!(
            switcher.opened(1, Err("no decoder")),
            Outcome::Abandoned(target("high"), Abandoned::OpenFailed("no decoder"))
        );
        assert!(switcher.is_idle());

        let (mut switcher, now) = playing(ms(1000));
        ask(&mut switcher, "low", now);
        switcher.incumbent_ended::<()>();
        assert_eq!(
            switcher.opened(2, Err("no such track")),
            Outcome::Abandoned(target("low"), Abandoned::OpenFailed("no such track"))
        );
        assert!(switcher.is_idle());
    }
}
