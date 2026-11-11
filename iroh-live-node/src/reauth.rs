//! When the node asks its policy again about a peer: deadlines, triggers and generations.
//!
//! The actor owns one [`Reauth`] and drives it with the current time. It
//! starts the decisions [`Reauth::start`] hands out, at most
//! [`MAX_CONCURRENT`] at once and one per peer, and applies a result only if
//! [`Reauth::finish`] says it is still current. Nothing here awaits or reads a
//! clock, so the logic is tested without a runtime.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    time::{Duration, Instant, SystemTime},
};

use iroh::EndpointId;

use crate::{
    SessionRequest,
    policy::{Authorization, Event},
};

/// How many re-decides run at once, across all peers.
pub(crate) const MAX_CONCURRENT: usize = 16;

/// The shortest time between two re-decides of one peer that its deadlines cause.
///
/// Keeps a policy that returns an authorization already expired, or a zero
/// revalidation interval, from asking itself in a loop.
pub(crate) const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// The re-decides the application asked for, until the actor takes them.
///
/// [`Node::reauthorize`](crate::Node::reauthorize) and
/// [`Node::reauthorize_all`](crate::Node::reauthorize_all) fill it.
#[derive(Debug, Default)]
pub(crate) struct Requests {
    /// Whether every peer is to be decided again.
    pub(crate) all: bool,
    /// The peers to decide again. Only peers with a live session go in.
    pub(crate) peers: BTreeSet<EndpointId>,
}

/// The re-decide state of every peer with a live session.
#[derive(Debug, Default)]
pub(crate) struct Reauth {
    peers: HashMap<EndpointId, Entry>,
    /// Peers waiting for a free slot, oldest trigger first.
    queue: VecDeque<EndpointId>,
    /// How many started decisions have not finished.
    running: usize,
    /// The last generation handed out, across all peers.
    generation: u64,
}

/// One peer's re-decide state.
#[derive(Debug)]
struct Entry {
    /// When the peer's authorization lapses or is due for revalidation.
    deadline: Option<Instant>,
    /// When the peer's authorization lapses.
    expires: Option<Instant>,
    /// Whether a trigger since the last start was more than a revalidation.
    reauthorize: bool,
    /// Counts the changes that make an earlier decision stale.
    generation: u64,
    /// The generation of the decision in flight, if one runs.
    running: Option<u64>,
    /// Whether the peer waits in the queue.
    queued: bool,
    /// Whether a trigger arrived while a decision ran, so it runs again.
    wanted: bool,
    /// What the peer asked for when its session opened.
    request: SessionRequest,
    /// Whether this node dialed that session.
    dialed: bool,
}

/// A decision to start, from [`Reauth::start`].
#[derive(Debug, Clone)]
pub(crate) struct Start {
    pub(crate) peer: EndpointId,
    pub(crate) generation: u64,
    /// [`Event::Revalidate`] when only the revalidation interval is due,
    /// [`Event::Reauthorize`] otherwise.
    pub(crate) event: Event,
    pub(crate) request: SessionRequest,
    pub(crate) dialed: bool,
}

/// What to do with a finished decision, from [`Reauth::finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Finished {
    /// The decision is the newest for the peer: apply it.
    Apply,
    /// Something changed while it ran: drop it.
    Stale,
    /// The peer has no session anymore: drop it.
    Gone,
}

/// Returns when `authorization` needs a new decision, if ever.
///
/// That is the earlier of its expiry and `now` plus its revalidation
/// interval, but no earlier than `now` plus [`MIN_INTERVAL`]. `wall` is the
/// wall-clock time at `now`, since an expiry is a wall-clock time.
pub(crate) fn deadline_of(
    authorization: &Authorization,
    now: Instant,
    wall: SystemTime,
) -> Option<Instant> {
    let expires = expiry_of(authorization, now, wall);
    let revalidate = authorization.revalidate.map(|every| now + every);
    let deadline = match (expires, revalidate) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(deadline.max(now + MIN_INTERVAL))
}

/// Returns when `authorization` expires, on the monotonic clock.
fn expiry_of(authorization: &Authorization, now: Instant, wall: SystemTime) -> Option<Instant> {
    authorization
        .expires
        .map(|expires| now + expires.duration_since(wall).unwrap_or_default())
}

impl Reauth {
    /// Records the authorization a newly registered session gave `peer`.
    ///
    /// Sets the peer's deadline from it. A decision in flight for the peer
    /// becomes stale, since this authorization is newer.
    pub(crate) fn admitted(
        &mut self,
        peer: EndpointId,
        authorization: &Authorization,
        request: SessionRequest,
        dialed: bool,
        now: Instant,
        wall: SystemTime,
    ) {
        self.generation += 1;
        let generation = self.generation;
        let deadline = deadline_of(authorization, now, wall);
        let entry = self.peers.entry(peer).or_insert_with(|| Entry {
            deadline: None,
            expires: None,
            reauthorize: false,
            generation,
            running: None,
            queued: false,
            wanted: false,
            request: SessionRequest::default(),
            dialed,
        });
        entry.generation = generation;
        entry.deadline = deadline;
        entry.expires = expiry_of(authorization, now, wall);
        entry.request = request;
        entry.dialed = dialed;
    }

    /// Sets the deadline of `peer` from the authorization a re-decide just applied.
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
    ///
    /// A decision in flight for it then finishes as [`Finished::Gone`].
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
    /// A decision in flight becomes stale and runs again once it finishes.
    /// Returns `false` for a peer without a live session.
    pub(crate) fn request(&mut self, peer: &EndpointId) -> bool {
        self.trigger(peer, true)
    }

    /// Asks for a new decision about `peer`.
    ///
    /// `reauthorize` is false for a revalidation that is due, and nothing more.
    /// Such a revalidation leaves a decision in flight alone: that decision is
    /// at least as fresh, and sets the next deadline when it lands. Making it
    /// stale would drop an answer the application may still be giving.
    fn trigger(&mut self, peer: &EndpointId, reauthorize: bool) -> bool {
        let Some(entry) = self.peers.get_mut(peer) else {
            return false;
        };
        if entry.running.is_some() && !reauthorize {
            return true;
        }
        self.generation += 1;
        entry.generation = self.generation;
        entry.reauthorize |= reauthorize;
        if entry.running.is_some() {
            entry.wanted = true;
        } else if !entry.queued {
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
    /// Clears those deadlines: the decision sets the next one. A peer whose
    /// authorization expired is decided with [`Event::Reauthorize`], one that
    /// is only due for revalidation with [`Event::Revalidate`].
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
            entry.running = Some(entry.generation);
            let event = if std::mem::take(&mut entry.reauthorize) {
                Event::Reauthorize
            } else {
                Event::Revalidate
            };
            self.running += 1;
            return Some(Start {
                peer,
                generation: entry.generation,
                event,
                request: entry.request.clone(),
                dialed: entry.dialed,
            });
        }
        None
    }

    /// Records that the decision for `peer` started at `generation` finished.
    ///
    /// Returns whether to apply it. A stale decision whose peer was asked
    /// about again in the meantime goes back into the queue.
    pub(crate) fn finish(&mut self, peer: &EndpointId, generation: u64) -> Finished {
        self.running = self.running.saturating_sub(1);
        let Some(entry) = self.peers.get_mut(peer) else {
            return Finished::Gone;
        };
        if entry.running == Some(generation) {
            entry.running = None;
        }
        if entry.wanted && entry.running.is_none() {
            entry.wanted = false;
            if !entry.queued {
                entry.queued = true;
                self.queue.push_back(*peer);
            }
        }
        if entry.generation == generation {
            Finished::Apply
        } else {
            Finished::Stale
        }
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

    fn admit(reauth: &mut Reauth, peer: EndpointId, authorization: &Authorization, now: Instant) {
        reauth.admitted(
            peer,
            authorization,
            SessionRequest::default(),
            false,
            now,
            SystemTime::UNIX_EPOCH,
        );
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
        assert_eq!(reauth.finish(&bob, first.generation), Finished::Stale);
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
        assert_eq!(reauth.finish(&bob, asked.generation), Finished::Apply);
        assert!(reauth.start().is_none());
    }

    #[test]
    fn a_decision_overtaken_by_a_newer_one_is_dropped() {
        let now = Instant::now();
        let bob = peer();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &Grant::everything().into(), now);

        assert!(reauth.request(&bob));
        let first = reauth.start().expect("a decision");
        // A trigger while it runs: the result is stale and the peer runs again.
        assert!(reauth.request(&bob));
        assert!(reauth.start().is_none(), "one decision per peer at a time");
        assert_eq!(reauth.finish(&bob, first.generation), Finished::Stale);
        let second = reauth.start().expect("the peer runs again");
        assert!(second.generation > first.generation);
        assert_eq!(reauth.finish(&bob, second.generation), Finished::Apply);
        assert!(reauth.start().is_none());
    }

    #[test]
    fn a_new_session_makes_a_running_decision_stale() {
        let now = Instant::now();
        let bob = peer();
        let mut reauth = Reauth::default();
        admit(&mut reauth, bob, &Grant::everything().into(), now);
        reauth.request(&bob);
        let running = reauth.start().expect("a decision");

        admit(&mut reauth, bob, &Grant::nothing().into(), now);
        assert_eq!(reauth.finish(&bob, running.generation), Finished::Stale);
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
        let running = reauth.start().expect("a decision");

        reauth.remove(&bob);
        assert!(!reauth.contains(&bob));
        assert_eq!(reauth.next_deadline(), None);
        assert_eq!(reauth.finish(&bob, running.generation), Finished::Gone);
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

        let first = &started[0];
        assert_eq!(
            reauth.finish(&first.peer, first.generation),
            Finished::Apply
        );
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
        let start = reauth.start().expect("a decision");
        assert!(reauth.start().is_none());
        assert_eq!(reauth.finish(&bob, start.generation), Finished::Apply);
    }
}
