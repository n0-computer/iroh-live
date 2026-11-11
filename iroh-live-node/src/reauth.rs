//! When the node asks its policy again about a peer: deadlines and triggers.
//!
//! The actor owns one [`Reauth`] and drives it with the current time. Nothing
//! here awaits or reads a clock, so the logic is tested without a runtime.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    time::{Duration, Instant, SystemTime},
};

use iroh::EndpointId;

use crate::policy::{Authorization, Event};

/// How many re-decides run at once, across all peers.
pub(crate) const MAX_CONCURRENT: usize = 16;

/// The shortest time between two re-decides of one peer that its deadlines cause.
///
/// Keeps a policy that returns an authorization already expired, or a zero
/// revalidation interval, from asking itself in a loop.
pub(crate) const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// The re-decides the application asked for, until the actor takes them.
#[derive(Debug, Default)]
pub(crate) struct Requests {
    /// Whether every peer is to be decided again.
    pub(crate) all: bool,
    /// The peers to decide again.
    pub(crate) peers: BTreeSet<EndpointId>,
}

/// The re-decide state of every peer with a live session.
///
/// At most one decision per peer runs at a time. A trigger that arrives
/// while one runs marks the peer, which runs once more after it. Whether a
/// finished decision applies is the business of [`State::decided`], which
/// compares when decisions started.
///
/// [`State::decided`]: crate::state::State::decided
#[derive(Debug, Default)]
pub(crate) struct Reauth {
    peers: HashMap<EndpointId, Entry>,
    /// Peers waiting for a free slot, oldest trigger first.
    queue: VecDeque<EndpointId>,
    /// How many started decisions have not finished.
    running: usize,
}

/// One peer's re-decide state.
#[derive(Debug, Default)]
struct Entry {
    /// When the peer is next due: for revalidation or at expiry.
    deadline: Option<Instant>,
    /// When the peer's authorization lapses.
    expires: Option<Instant>,
    /// Whether the next decision is more than a revalidation.
    reauthorize: bool,
    /// Whether a decision for the peer runs.
    running: bool,
    /// Whether the peer waits in the queue.
    queued: bool,
    /// Whether a trigger arrived while a decision ran, so it runs again.
    wanted: bool,
}

/// A decision to start, from [`Reauth::start`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Start {
    pub(crate) peer: EndpointId,
    pub(crate) event: Event,
}

/// Returns when `authorization` needs a new decision, if ever.
///
/// That is the earlier of its expiry and `now` plus its revalidation
/// interval, but no earlier than `now` plus [`MIN_INTERVAL`]. `wall` is the
/// wall-clock time at `now`, since an expiry is a wall-clock time. A time
/// beyond the clock's range is never.
pub(crate) fn deadline_of(
    authorization: &Authorization,
    now: Instant,
    wall: SystemTime,
) -> Option<Instant> {
    let expires = expiry_of(authorization, now, wall);
    let revalidate = authorization
        .revalidate
        .and_then(|every| now.checked_add(every));
    let deadline = match (expires, revalidate) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(deadline.max(now + MIN_INTERVAL))
}

/// Returns when `authorization` expires on the monotonic clock, or `None` if beyond its range.
fn expiry_of(authorization: &Authorization, now: Instant, wall: SystemTime) -> Option<Instant> {
    authorization
        .expires
        .and_then(|expires| now.checked_add(expires.duration_since(wall).unwrap_or_default()))
}

impl Reauth {
    /// Records that `peer` has a registered session.
    ///
    /// Leaves a known peer alone: a new session neither moves its deadline
    /// nor touches a decision in flight, so a peer that keeps opening
    /// sessions is still revalidated on time.
    pub(crate) fn register(&mut self, peer: EndpointId) {
        self.peers.entry(peer).or_default();
    }

    /// Sets the deadline of `peer` from the authorization that just applied.
    pub(crate) fn decided(
        &mut self,
        peer: &EndpointId,
        authorization: &Authorization,
        now: Instant,
        wall: SystemTime,
    ) {
        if let Some(entry) = self.peers.get_mut(peer) {
            entry.deadline = deadline_of(authorization, now, wall);
            entry.expires = expiry_of(authorization, now, wall);
        }
    }

    /// Forgets `peer`, whose last session ended or whose policy rejected it.
    pub(crate) fn remove(&mut self, peer: &EndpointId) {
        if self.peers.remove(peer).is_some() {
            self.queue.retain(|queued| queued != peer);
        }
    }

    /// Reports whether `peer` has a live session to re-decide.
    #[cfg(test)]
    pub(crate) fn contains(&self, peer: &EndpointId) -> bool {
        self.peers.contains_key(peer)
    }

    /// Asks for a new decision about `peer`, with [`Event::Reauthorize`].
    ///
    /// Returns `false` for a peer without a live session.
    pub(crate) fn request(&mut self, peer: &EndpointId) -> bool {
        self.trigger(peer, true)
    }

    /// Asks for a new decision about `peer`.
    ///
    /// While a decision for the peer runs, a reauthorization runs once more
    /// after it, however many arrive. A revalidation that falls due then is
    /// dropped, since the running decision is at least as fresh and sets the
    /// next deadline.
    fn trigger(&mut self, peer: &EndpointId, reauthorize: bool) -> bool {
        let Some(entry) = self.peers.get_mut(peer) else {
            return false;
        };
        if entry.running {
            if reauthorize {
                entry.reauthorize = true;
                entry.wanted = true;
            }
            return true;
        }
        entry.reauthorize |= reauthorize;
        if !entry.queued {
            entry.queued = true;
            self.queue.push_back(*peer);
        }
        true
    }

    /// Asks for a new decision about every peer.
    pub(crate) fn request_all(&mut self) {
        let peers: Vec<EndpointId> = self.peers.keys().copied().collect();
        for peer in peers {
            self.request(&peer);
        }
    }

    /// Returns the earliest deadline of any peer.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.peers.values().filter_map(|entry| entry.deadline).min()
    }

    /// Asks for a new decision about every peer whose deadline is at or before `now`.
    ///
    /// A peer whose authorization expired is decided with
    /// [`Event::Reauthorize`], one only due for revalidation with
    /// [`Event::Revalidate`].
    pub(crate) fn expire(&mut self, now: Instant) {
        let due: Vec<(EndpointId, bool)> = self
            .peers
            .iter_mut()
            .filter(|(_, entry)| entry.deadline.is_some_and(|deadline| deadline <= now))
            .map(|(peer, entry)| {
                entry.deadline = None;
                let expired = entry.expires.is_some_and(|expires| expires <= now);
                (*peer, expired)
            })
            .collect();
        for (peer, expired) in due {
            self.trigger(&peer, expired);
        }
    }

    /// Returns the next decision to start, while fewer than [`MAX_CONCURRENT`] run.
    pub(crate) fn start(&mut self) -> Option<Start> {
        while self.running < MAX_CONCURRENT {
            let peer = self.queue.pop_front()?;
            let Some(entry) = self.peers.get_mut(&peer) else {
                continue;
            };
            entry.queued = false;
            entry.wanted = false;
            entry.running = true;
            let event = if std::mem::take(&mut entry.reauthorize) {
                Event::Reauthorize
            } else {
                Event::Revalidate
            };
            self.running += 1;
            return Some(Start { peer, event });
        }
        None
    }

    /// Records that the decision for `peer` finished, and queues it again if it was triggered meanwhile.
    ///
    /// Returns whether the peer still has a live session.
    pub(crate) fn finish(&mut self, peer: &EndpointId) -> bool {
        self.running = self.running.saturating_sub(1);
        let Some(entry) = self.peers.get_mut(peer) else {
            return false;
        };
        entry.running = false;
        if std::mem::take(&mut entry.wanted) && !entry.queued {
            entry.queued = true;
            self.queue.push_back(*peer);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;
    use crate::Grant;

    fn peer() -> EndpointId {
        SecretKey::generate().public()
    }

    /// Registers `peer` and applies `authorization` as its first decision.
    fn admit(reauth: &mut Reauth, peer: EndpointId, authorization: &Authorization, now: Instant) {
        reauth.register(peer);
        reauth.decided(&peer, authorization, now, SystemTime::UNIX_EPOCH);
    }

    fn expiring(after: Duration, revalidate: Option<Duration>) -> Authorization {
        Authorization {
            expires: Some(SystemTime::UNIX_EPOCH + after),
            revalidate,
            ..Grant::everything().into()
        }
    }

    #[test]
    fn the_earliest_of_expiry_and_revalidation_wins() {
        let now = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let secs = Duration::from_secs;
        let deadline = |authorization| deadline_of(&authorization, now, wall);

        assert_eq!(deadline(Grant::everything().into()), None);
        assert_eq!(deadline(expiring(secs(5), None)), Some(now + secs(5)));
        assert_eq!(
            deadline(expiring(secs(5), Some(secs(2)))),
            Some(now + secs(2))
        );
        assert_eq!(
            deadline(expiring(secs(1), Some(secs(2)))),
            Some(now + secs(1))
        );
        // An expiry in the past, or a zero interval, still waits a little.
        let past = Authorization {
            expires: Some(wall - secs(10)),
            ..Default::default()
        };
        assert_eq!(deadline(past), Some(now + MIN_INTERVAL));
        let zero = Authorization {
            revalidate: Some(Duration::ZERO),
            ..Default::default()
        };
        assert_eq!(deadline(zero), Some(now + MIN_INTERVAL));
    }

    #[test]
    fn a_time_beyond_the_clock_is_never() {
        let now = Instant::now();
        let forever = Authorization {
            revalidate: Some(Duration::MAX),
            ..Default::default()
        };
        assert_eq!(deadline_of(&forever, now, SystemTime::UNIX_EPOCH), None);
        let far = expiring(
            Duration::from_secs(u64::MAX / 4),
            Some(Duration::from_secs(5)),
        );
        assert_eq!(
            deadline_of(&far, now, SystemTime::UNIX_EPOCH),
            Some(now + Duration::from_secs(5)),
            "the revalidation still holds"
        );
    }

    #[test]
    fn the_earliest_peer_deadline_fires_first() {
        let now = Instant::now();
        let (early, late) = (peer(), peer());
        let mut reauth = Reauth::default();
        admit(
            &mut reauth,
            late,
            &expiring(Duration::from_secs(9), None),
            now,
        );
        admit(
            &mut reauth,
            early,
            &expiring(Duration::from_secs(3), None),
            now,
        );
        admit(&mut reauth, peer(), &Grant::everything().into(), now);
        assert_eq!(reauth.next_deadline(), Some(now + Duration::from_secs(3)));

        reauth.expire(now + Duration::from_secs(4));
        let start = reauth.start().expect("the early peer is due");
        assert_eq!(start.peer, early);
        assert!(reauth.start().is_none(), "the late peer is not due yet");
        assert_eq!(reauth.next_deadline(), Some(now + Duration::from_secs(9)));
    }

    #[test]
    fn only_a_due_revalidation_is_decided_as_one() {
        let now = Instant::now();
        let secs = Duration::from_secs;
        let (revalidating, expiring_peer, asked) = (peer(), peer(), peer());
        let mut reauth = Reauth::default();
        admit(
            &mut reauth,
            revalidating,
            &expiring(secs(60), Some(secs(2))),
            now,
        );
        admit(
            &mut reauth,
            expiring_peer,
            &expiring(secs(2), Some(secs(9))),
            now,
        );
        admit(&mut reauth, asked, &Grant::everything().into(), now);

        reauth.expire(now + secs(3));
        reauth.request(&asked);
        let mut events = HashMap::new();
        while let Some(start) = reauth.start() {
            events.insert(start.peer, start.event);
        }
        assert_eq!(events[&revalidating], Event::Revalidate);
        assert_eq!(events[&expiring_peer], Event::Reauthorize);
        assert_eq!(events[&asked], Event::Reauthorize);
    }

    #[test]
    fn a_request_while_a_revalidation_runs_reauthorizes_next() {
        let now = Instant::now();
        let bob = peer();
        let mut reauth = Reauth::default();
        admit(
            &mut reauth,
            bob,
            &expiring(Duration::from_secs(60), Some(Duration::from_secs(1))),
            now,
        );
        reauth.expire(now + Duration::from_secs(2));
        let first = reauth.start().expect("a revalidation");
        assert_eq!(first.event, Event::Revalidate);
        reauth.request(&bob);
        assert!(reauth.finish(&bob));
        let second = reauth.start().expect("the peer runs again");
        assert_eq!(second.event, Event::Reauthorize);
    }

    #[test]
    fn a_due_revalidation_leaves_a_running_reauthorize_alone() {
        let now = Instant::now();
        let bob = peer();
        let mut reauth = Reauth::default();
        let every_second = expiring(Duration::from_secs(60), Some(Duration::from_secs(1)));
        admit(&mut reauth, bob, &every_second, now);
        assert!(reauth.request(&bob));
        let asked = reauth.start().expect("a reauthorization");
        assert_eq!(asked.event, Event::Reauthorize);
        // The revalidation interval passes while the application answers.
        reauth.expire(now + Duration::from_secs(2));
        assert!(reauth.finish(&bob));
        assert!(reauth.start().is_none());
    }

    #[test]
    fn triggers_while_a_decision_runs_coalesce_into_one_rerun() {
        let now = Instant::now();
        let bob = peer();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &Grant::everything().into(), now);

        assert!(reauth.request(&bob));
        for _round in 0..5 {
            let running = reauth.start().expect("a decision");
            assert_eq!(running.event, Event::Reauthorize);
            for _ in 0..3 {
                assert!(reauth.request(&bob));
            }
            assert!(reauth.start().is_none(), "one decision per peer at a time");
            assert!(reauth.finish(&bob));
        }
        let last = reauth.start().expect("the last trigger runs once more");
        assert!(reauth.finish(&bob));
        assert_eq!(last.peer, bob);
        assert!(reauth.start().is_none());
    }

    #[test]
    fn a_new_session_leaves_the_deadline_and_a_running_decision_alone() {
        let now = Instant::now();
        let bob = peer();
        let mut reauth = Reauth::default();
        let every_second = expiring(Duration::from_secs(60), Some(Duration::from_secs(1)));
        admit(&mut reauth, bob, &every_second, now);
        reauth.register(bob);
        assert_eq!(reauth.next_deadline(), Some(now + Duration::from_secs(1)));

        reauth.request(&bob);
        let running = reauth.start().expect("a decision");
        reauth.register(bob);
        assert!(reauth.finish(&bob));
        assert_eq!(running.peer, bob);
        assert!(reauth.start().is_none(), "a new session needs no decision");
    }

    #[test]
    fn a_removed_peer_clears_its_deadline_and_its_decision() {
        let now = Instant::now();
        let bob = peer();
        let mut reauth = Reauth::default();
        admit(
            &mut reauth,
            bob,
            &expiring(Duration::from_secs(1), None),
            now,
        );
        reauth.request(&bob);
        reauth.start().expect("a decision");

        reauth.remove(&bob);
        assert!(!reauth.contains(&bob));
        assert_eq!(reauth.next_deadline(), None);
        assert!(!reauth.finish(&bob));
        assert!(!reauth.request(&bob), "an unknown peer is not queued");
        reauth.expire(now + Duration::from_secs(5));
        assert!(reauth.start().is_none());
    }

    #[test]
    fn at_most_a_bounded_number_of_decisions_run_at_once() {
        let now = Instant::now();
        let mut reauth = Reauth::default();
        let peers: Vec<_> = (0..MAX_CONCURRENT + 4).map(|_| peer()).collect();
        for peer in &peers {
            admit(&mut reauth, *peer, &Grant::everything().into(), now);
        }
        reauth.request_all();
        reauth.request_all();
        let started: Vec<Start> = std::iter::from_fn(|| reauth.start()).collect();
        assert_eq!(started.len(), MAX_CONCURRENT);

        assert!(reauth.finish(&started[0].peer));
        assert!(reauth.start().is_some(), "a finished decision frees a slot");
        assert!(reauth.start().is_none());
    }

    #[test]
    fn a_queued_peer_runs_once_for_many_triggers() {
        let now = Instant::now();
        let bob = peer();
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
