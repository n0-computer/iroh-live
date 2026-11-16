//! When the node asks its policy again about a session: expiry, triggers and retries.
//!
//! The actor owns one [`Reauth`] and drives it with the current time. Nothing
//! here awaits or reads a clock, so the logic is tested without a runtime.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    time::{Duration, Instant, SystemTime},
};

use iroh::EndpointId;

use crate::{LinkId, policy::Authorization};

/// How many re-decides run at once, across all sessions.
pub(crate) const MAX_CONCURRENT: usize = 16;

/// The shortest time between two re-decides of one session that its expiry causes.
///
/// Keeps a policy that returns an authorization already expired from asking
/// itself in a loop.
pub(crate) const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// The re-decides the application asked for, until the actor takes them.
#[derive(Debug, Default)]
pub(crate) struct Requests {
    /// Whether every session is to be decided again.
    pub(crate) all: bool,
    /// The peers whose sessions are to be decided again.
    pub(crate) peers: BTreeSet<EndpointId>,
}

/// The shortest wait before a decision that failed to decide runs again.
pub(crate) const RETRY_MIN: Duration = Duration::from_millis(500);

/// The longest wait before a decision that failed to decide runs again.
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(30);

/// The re-decide state of every live session.
///
/// At most one decision per session runs at a time, and it applies when it
/// finishes. A trigger that arrives while one runs marks the session, which
/// runs once more after it, so no trigger is lost and none starves a
/// decision.
#[derive(Debug, Default)]
pub(crate) struct Reauth {
    sessions: HashMap<LinkId, Entry>,
    /// Sessions waiting for a free slot, oldest trigger first.
    queue: VecDeque<LinkId>,
    /// How many started decisions have not finished.
    running: usize,
}

/// One session's re-decide state.
#[derive(Debug, Default)]
struct Entry {
    /// When the session is next due: at expiry, or for a retry.
    deadline: Option<Instant>,
    /// When the session's authorization lapses.
    expires: Option<Instant>,
    /// Where the session's next decision stands.
    phase: Phase,
    /// How many decisions in a row ended without a decision.
    failures: u32,
}

/// Where a session's next decision stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    /// No decision runs or waits.
    #[default]
    Idle,
    /// The session waits in the queue for a free slot.
    Queued,
    /// A decision runs.
    Running {
        /// Whether a trigger arrived meanwhile, so another decision follows.
        again: bool,
    },
}

/// Returns when `authorization` needs a new decision, if ever.
///
/// That is its expiry, but no earlier than `now` plus [`MIN_INTERVAL`].
/// `wall` is the wall-clock time at `now`, since an expiry is a wall-clock
/// time. A time beyond the clock's range is never.
pub(crate) fn deadline_of(
    authorization: &Authorization,
    now: Instant,
    wall: SystemTime,
) -> Option<Instant> {
    let expires = expiry_of(authorization, now, wall)?;
    Some(expires.max(now + MIN_INTERVAL))
}

/// Returns when `authorization` expires on the monotonic clock, or `None` if beyond its range.
fn expiry_of(authorization: &Authorization, now: Instant, wall: SystemTime) -> Option<Instant> {
    authorization
        .expires
        .and_then(|expires| now.checked_add(expires.duration_since(wall).unwrap_or_default()))
}

impl Reauth {
    /// Records that the session on `link` registered.
    pub(crate) fn register(&mut self, link: LinkId) {
        self.sessions.entry(link).or_default();
    }

    /// Sets the deadline of `link` from the authorization that just applied.
    pub(crate) fn decided(
        &mut self,
        link: &LinkId,
        authorization: &Authorization,
        now: Instant,
        wall: SystemTime,
    ) {
        if let Some(entry) = self.sessions.get_mut(link) {
            entry.deadline = deadline_of(authorization, now, wall);
            entry.expires = expiry_of(authorization, now, wall);
            entry.failures = 0;
        }
    }

    /// Forgets `link`, whose session ended or whose policy rejected it.
    pub(crate) fn remove(&mut self, link: &LinkId) {
        if self.sessions.remove(link).is_some() {
            self.queue.retain(|queued| queued != link);
        }
    }

    /// Reports whether `link` is a live session to re-decide.
    #[cfg(test)]
    pub(crate) fn contains(&self, link: &LinkId) -> bool {
        self.sessions.contains_key(link)
    }

    /// Asks for a new decision about `link`.
    ///
    /// While a decision for the session runs, one more runs after it, however
    /// many requests arrive. Returns `false` for a session that is not live.
    pub(crate) fn request(&mut self, link: &LinkId) -> bool {
        let Some(entry) = self.sessions.get_mut(link) else {
            return false;
        };
        match &mut entry.phase {
            Phase::Running { again } => *again = true,
            Phase::Queued => {}
            Phase::Idle => {
                entry.phase = Phase::Queued;
                self.queue.push_back(*link);
            }
        }
        true
    }

    /// Asks for a new decision about every session.
    pub(crate) fn request_all(&mut self) {
        let links: Vec<LinkId> = self.sessions.keys().copied().collect();
        for link in links {
            self.request(&link);
        }
    }

    /// Returns the earliest deadline of any session.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.sessions
            .values()
            .filter_map(|entry| entry.deadline)
            .min()
    }

    /// Asks for a new decision about every session whose deadline is at or before `now`.
    ///
    /// Returns the sessions whose authorization expired, which lose their
    /// rights until a new decision applies. A retry deadline before the
    /// expiry arms the expiry next, so a slow decision cannot hold an expired
    /// authorization in force.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<LinkId> {
        let due: Vec<(LinkId, bool)> = self
            .sessions
            .iter_mut()
            .filter(|(_, entry)| entry.deadline.is_some_and(|deadline| deadline <= now))
            .map(|(link, entry)| {
                let expired = entry.expires.is_some_and(|expires| expires <= now);
                entry.deadline = entry.expires.filter(|_| !expired);
                (*link, expired)
            })
            .collect();
        let mut expired = Vec::new();
        for (link, lapsed) in due {
            self.request(&link);
            if lapsed {
                expired.push(link);
            }
        }
        expired
    }

    /// Returns the next decision to start, while fewer than [`MAX_CONCURRENT`] run.
    pub(crate) fn start(&mut self) -> Option<LinkId> {
        while self.running < MAX_CONCURRENT {
            let link = self.queue.pop_front()?;
            let Some(entry) = self.sessions.get_mut(&link) else {
                continue;
            };
            entry.phase = Phase::Running { again: false };
            self.running += 1;
            return Some(link);
        }
        None
    }

    /// Records that the decision for `link` finished, and queues it again if it was triggered meanwhile.
    ///
    /// Returns whether the session is still live.
    pub(crate) fn finish(&mut self, link: &LinkId) -> bool {
        self.running = self.running.saturating_sub(1);
        let Some(entry) = self.sessions.get_mut(link) else {
            return false;
        };
        match entry.phase {
            Phase::Running { again: true } => {
                entry.phase = Phase::Queued;
                self.queue.push_back(*link);
            }
            Phase::Running { again: false } => entry.phase = Phase::Idle,
            Phase::Idle | Phase::Queued => {}
        }
        true
    }

    /// Schedules another decision for `link`, whose last one ended without a decision.
    ///
    /// The wait doubles with each failure in a row, from [`RETRY_MIN`] up
    /// to [`RETRY_MAX`]. Returns the wait.
    pub(crate) fn retry(&mut self, link: &LinkId, now: Instant) -> Duration {
        let Some(entry) = self.sessions.get_mut(link) else {
            return Duration::ZERO;
        };
        let wait = RETRY_MIN
            .saturating_mul(1 << entry.failures.min(16))
            .min(RETRY_MAX);
        entry.failures = entry.failures.saturating_add(1);
        let at = now + wait;
        entry.deadline = Some(entry.deadline.map_or(at, |deadline| deadline.min(at)));
        wait
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::Grant;

    /// Returns a link id no other call returns.
    fn link() -> LinkId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        LinkId(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Registers the session `link` and applies `authorization` as its first decision.
    fn admit(reauth: &mut Reauth, link: LinkId, authorization: &Authorization, now: Instant) {
        reauth.register(link);
        reauth.decided(&link, authorization, now, SystemTime::UNIX_EPOCH);
    }

    fn expiring(after: Duration) -> Authorization {
        Authorization {
            expires: Some(SystemTime::UNIX_EPOCH + after),
            ..Grant::everything().into()
        }
    }

    #[test]
    fn the_deadline_is_the_expiry() {
        let now = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let secs = Duration::from_secs;
        let deadline = |authorization| deadline_of(&authorization, now, wall);

        assert_eq!(deadline(Grant::everything().into()), None);
        assert_eq!(deadline(expiring(secs(5))), Some(now + secs(5)));
        // An expiry in the past still waits a little.
        let past = Authorization {
            expires: Some(wall - secs(10)),
            ..Default::default()
        };
        assert_eq!(deadline(past), Some(now + MIN_INTERVAL));
    }

    #[test]
    fn the_earliest_deadline_fires_first() {
        let now = Instant::now();
        let (early, late) = (link(), link());
        let mut reauth = Reauth::default();
        admit(&mut reauth, late, &expiring(Duration::from_secs(9)), now);
        admit(&mut reauth, early, &expiring(Duration::from_secs(3)), now);
        admit(&mut reauth, link(), &Grant::everything().into(), now);
        assert_eq!(reauth.next_deadline(), Some(now + Duration::from_secs(3)));

        assert_eq!(reauth.expire(now + Duration::from_secs(4)), [early]);
        assert_eq!(reauth.start(), Some(early));
        assert!(reauth.start().is_none(), "the late session is not due yet");
        assert_eq!(reauth.next_deadline(), Some(now + Duration::from_secs(9)));
    }

    #[test]
    fn the_expiry_stays_armed_while_a_decision_runs() {
        let now = Instant::now();
        let bob = link();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &expiring(Duration::from_secs(10)), now);
        assert!(reauth.request(&bob));
        assert_eq!(reauth.start(), Some(bob));
        assert_eq!(reauth.next_deadline(), Some(now + Duration::from_secs(10)));

        // The decision is still running when the authorization expires.
        assert_eq!(reauth.expire(now + Duration::from_secs(10)), [bob]);
        assert!(
            reauth.start().is_none(),
            "one decision per session at a time"
        );
        assert!(reauth.finish(&bob));
        assert_eq!(reauth.start(), Some(bob), "the expiry decides again");
    }

    #[test]
    fn triggers_while_a_decision_runs_coalesce_into_one_rerun() {
        let now = Instant::now();
        let bob = link();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &Grant::everything().into(), now);

        assert!(reauth.request(&bob));
        for _round in 0..5 {
            assert_eq!(reauth.start(), Some(bob));
            for _ in 0..3 {
                assert!(reauth.request(&bob));
            }
            assert!(
                reauth.start().is_none(),
                "one decision per session at a time"
            );
            assert!(reauth.finish(&bob));
        }
        assert_eq!(reauth.start(), Some(bob), "the last trigger runs once more");
        assert!(reauth.finish(&bob));
        assert!(reauth.start().is_none());
    }

    #[test]
    fn a_failed_decision_retries_with_a_growing_wait() {
        let now = Instant::now();
        let bob = link();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &Grant::everything().into(), now);
        reauth.request(&bob);
        reauth.start().expect("a decision");
        assert!(reauth.finish(&bob));
        assert_eq!(reauth.retry(&bob, now), RETRY_MIN);
        assert!(reauth.start().is_none(), "not before the wait");
        assert!(reauth.expire(now + RETRY_MIN).is_empty());
        assert_eq!(reauth.start(), Some(bob), "the retry");
        assert!(reauth.finish(&bob));
        assert_eq!(reauth.retry(&bob, now), RETRY_MIN * 2);
        for _ in 0..10 {
            reauth.retry(&bob, now);
        }
        assert_eq!(reauth.retry(&bob, now), RETRY_MAX);

        reauth.decided(
            &bob,
            &Grant::everything().into(),
            now,
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(reauth.retry(&bob, now), RETRY_MIN, "a decision resets it");
    }

    #[test]
    fn a_removed_session_clears_its_deadline_and_its_decision() {
        let now = Instant::now();
        let bob = link();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &expiring(Duration::from_secs(1)), now);
        reauth.request(&bob);
        reauth.start().expect("a decision");

        reauth.remove(&bob);
        assert!(!reauth.contains(&bob));
        assert_eq!(reauth.next_deadline(), None);
        assert!(!reauth.finish(&bob));
        assert!(!reauth.request(&bob), "an unknown session is not queued");
        reauth.expire(now + Duration::from_secs(5));
        assert!(reauth.start().is_none());
    }

    #[test]
    fn at_most_a_bounded_number_of_decisions_run_at_once() {
        let now = Instant::now();
        let mut reauth = Reauth::default();
        let links: Vec<_> = (0..MAX_CONCURRENT + 4).map(|_| link()).collect();
        for link in &links {
            admit(&mut reauth, *link, &Grant::everything().into(), now);
        }
        reauth.request_all();
        reauth.request_all();
        let started: Vec<LinkId> = std::iter::from_fn(|| reauth.start()).collect();
        assert_eq!(started.len(), MAX_CONCURRENT);

        assert!(reauth.finish(&started[0]));
        assert!(reauth.start().is_some(), "a finished decision frees a slot");
        assert!(reauth.start().is_none());
    }

    #[test]
    fn a_queued_session_runs_once_for_many_triggers() {
        let now = Instant::now();
        let bob = link();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &Grant::everything().into(), now);
        for _ in 0..5 {
            reauth.request(&bob);
        }
        reauth.start().expect("a decision");
        assert!(reauth.start().is_none());
        assert!(reauth.finish(&bob));
        assert!(reauth.start().is_none());
    }
}
