//! Re-decides: the policy is asked again at expiry, on revalidation, on change and on request.
//!
//! These run on real time with short durations, and wait on conditions with
//! a timeout rather than for fixed spans.

mod common;

use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use common::{Peer, endpoint, ends, eventually, publish, reading, step};
use iroh::{Endpoint, EndpointId};
use iroh_live_node::{
    Audience, ConnectOptions, Error, Grant, Node, NodeConfig, Reach, Session,
    policy::{self, Authorization, Decision, Event, PeerRequest, Policy, Rejection},
};
use n0_future::{StreamExt, boxed::BoxStream};
use n0_tracing_test::traced_test;
use n0_watcher::{Watchable, Watcher};
use tokio::sync::Notify;

/// How long an authorization in these tests lasts before the node asks again.
const SHORT: Duration = Duration::from_millis(400);

/// Reports whether `node` lists an open session with `peer`.
fn has_session_with(node: &Node, peer: EndpointId) -> bool {
    node.sessions()
        .get()
        .iter()
        .any(|session| session.peer() == peer)
}

#[tokio::test]
#[traced_test]
async fn an_expiring_authorization_narrows_the_live_session() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        move |request: &PeerRequest| {
            seen.lock().expect("poisoned").push(request.event());
            match request.event() {
                Event::Connect => Ok(Authorization {
                    grant: Grant::everything(),
                    expires: Some(SystemTime::now() + SHORT),
                    ..Default::default()
                }),
                Event::Revalidate | Event::Reauthorize => Ok(Grant::subscribe_only().into()),
            }
        },
    )))
    .await;
    let bob = Peer::spawn().await;
    let cam = publish(&bob.node, bob.path("cam"), Audience::Granted).expect("publish");

    let subscription = step(
        "alice subscribes",
        alice.node.subscribe(cam.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect("the first grant lets bob publish");
    let session = subscription.session().expect("a session");
    let mut track = reading(&subscription.broadcast()).await;
    assert_eq!(session.grant(), Grant::everything());

    ends("the lapsed grant ends the read", &mut track).await;
    eventually("the grant narrows", || {
        alice.node.grant_for(bob.id()) == Some(Grant::subscribe_only().into())
    })
    .await;
    assert_eq!(
        session.grant(),
        Grant::subscribe_only(),
        "the session's grant is the new one"
    );
    assert!(
        events
            .lock()
            .expect("poisoned")
            .contains(&Event::Reauthorize),
        "an expiry decides with Reauthorize"
    );
    assert!(
        has_session_with(&alice.node, bob.id()),
        "narrowed, not closed"
    );
    // The narrower grant holds for a new read too.
    let err = step(
        "subscribe again",
        alice.node.subscribe(cam.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect_err("bob may no longer publish");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_rejection_at_revalidation_closes_the_sessions_with_its_code() {
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        |request: &PeerRequest| match request.event() {
            Event::Connect => Ok(Authorization {
                grant: Grant::subscribe_only(),
                revalidate: Some(SHORT),
                ..Default::default()
            }),
            Event::Revalidate | Event::Reauthorize => {
                Err(Rejection::new("revoked").with_code(moq_net::Error::App(42)))
            }
        },
    )))
    .await;
    let bob = Peer::spawn().await;

    let session = step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted at first");
    let err = step("the session closes", session.closed()).await;
    let Error::SessionClosed { source, .. } = &err else {
        panic!("not a closed session: {err:#}");
    };
    assert!(
        matches!(
            source,
            moq_net::Error::Session(moq_net::SessionError::App(42))
        ),
        "bob sees the rejection's code: {err:#}"
    );
    eventually("alice forgets bob", || {
        !has_session_with(&alice.node, bob.id()) && alice.node.grant_for(bob.id()).is_none()
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A policy whose grant the test switches, and which reports the switch.
#[derive(Clone)]
struct Switchable {
    grant: Arc<Mutex<Grant>>,
    changed: Watchable<u64>,
}

impl std::fmt::Debug for Switchable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Switchable").finish_non_exhaustive()
    }
}

impl Switchable {
    fn new(grant: Grant) -> Self {
        Self {
            grant: Arc::new(Mutex::new(grant)),
            changed: Watchable::new(0),
        }
    }

    /// Replaces the grant and tells the node.
    fn switch(&self, grant: Grant) {
        *self.grant.lock().expect("poisoned") = grant;
        let next = self.changed.get() + 1;
        self.changed.set(next).ok();
    }
}

impl Policy<EndpointId> for Switchable {
    async fn decide(&self, _request: &PeerRequest) -> Decision {
        Ok(self.grant.lock().expect("poisoned").clone().into())
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        Some(Box::pin(
            self.changed.watch().stream_updates_only().map(|_| ()),
        ))
    }
}

#[tokio::test]
#[traced_test]
async fn a_policy_change_decides_every_peer_again() {
    let policy = Switchable::new(Grant::everything());
    let alice = Peer::bare(NodeConfig::default().with_policy(policy.clone())).await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;
    for peer in [&bob, &carol] {
        step("dial alice", peer.node.connect(alice.endpoint.addr()))
            .await
            .expect("admitted");
        eventually("alice registers the peer", || {
            alice.node.grant_for(peer.id()).is_some()
        })
        .await;
    }

    policy.switch(Grant::subscribe_only());
    let narrowed = Some(Authorization::from(Grant::subscribe_only()));
    eventually("both peers narrow", || {
        alice.node.grant_for(bob.id()) == narrowed && alice.node.grant_for(carol.id()) == narrowed
    })
    .await;
    assert!(has_session_with(&alice.node, bob.id()));
    assert!(has_session_with(&alice.node, carol.id()));

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn reauthorize_decides_one_peer_again() {
    let contacts = Arc::new(Mutex::new(BTreeSet::new()));
    let decisions = Arc::new(Mutex::new(HashMap::<EndpointId, usize>::new()));
    let (allowed, counted) = (contacts.clone(), decisions.clone());
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        move |request: &PeerRequest| {
            *counted
                .lock()
                .expect("poisoned")
                .entry(*request.peer())
                .or_default() += 1;
            if allowed.lock().expect("poisoned").contains(request.peer()) {
                Ok(Grant::everything().into())
            } else {
                Err(Rejection::new("not a contact"))
            }
        },
    )))
    .await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;
    contacts
        .lock()
        .expect("poisoned")
        .extend([bob.id(), carol.id()]);
    let mut sessions = Vec::new();
    for peer in [&bob, &carol] {
        sessions.push(
            step("dial alice", peer.node.connect(alice.endpoint.addr()))
                .await
                .expect("a contact is admitted"),
        );
        eventually("alice registers the peer", || {
            alice.node.grant_for(peer.id()).is_some()
        })
        .await;
    }

    // A peer without a session is not decided at all.
    alice.node.reauthorize(alice.id());

    contacts.lock().expect("poisoned").remove(&carol.id());
    alice.node.reauthorize(carol.id());
    step("carol's session closes", sessions[1].closed()).await;
    eventually("alice forgets carol", || {
        alice.node.grant_for(carol.id()).is_none()
    })
    .await;
    assert_eq!(
        decisions.lock().expect("poisoned")[&bob.id()],
        1,
        "bob is not asked again"
    );
    assert_eq!(
        alice.node.grant_for(bob.id()),
        Some(Grant::everything().into())
    );
    assert!(
        sessions[0].connection().close_reason().is_none(),
        "bob stays"
    );

    // The rejected peer gets nothing from its old authorization when it comes back.
    let again = step(
        "carol dials again",
        carol.node.connect(alice.endpoint.addr()),
    )
    .await;
    if let Ok(session) = again {
        step("the new session closes", session.closed()).await;
    }
    assert!(alice.node.grant_for(carol.id()).is_none());
    assert_eq!(decisions.lock().expect("poisoned")[&carol.id()], 3);

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_queue_answers_revalidations_and_asks_the_application_to_reauthorize() {
    let queue = policy::Queue::new(16);
    let alice = Peer::bare(NodeConfig::default().with_policy(queue.clone())).await;
    let bob = Peer::spawn().await;
    let answered = Arc::new(Mutex::new(Vec::new()));
    let seen = answered.clone();
    let _answering = n0_future::task::AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(pending) = queue.next().await {
            let event = pending.request().event();
            seen.lock().expect("poisoned").push(event);
            match event {
                Event::Connect => pending.admit(Authorization {
                    grant: Grant::everything(),
                    revalidate: Some(Duration::from_millis(100)),
                    ..Default::default()
                }),
                _ => pending.admit(Grant::subscribe_only()),
            }
        }
    }));

    step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");
    eventually("alice registers bob", || {
        alice.node.grant_for(bob.id()).is_some()
    })
    .await;
    // Several revalidation intervals pass without the application.
    tokio::time::sleep(SHORT).await;
    assert_eq!(*answered.lock().expect("poisoned"), [Event::Connect]);
    assert_eq!(
        alice.node.grant_for(bob.id()).map(|current| current.grant),
        Some(Grant::everything()),
        "the queue keeps the authorization"
    );

    alice.node.reauthorize(bob.id());
    eventually("the application narrows bob", || {
        alice.node.grant_for(bob.id()) == Some(Grant::subscribe_only().into())
    })
    .await;
    assert_eq!(
        *answered.lock().expect("poisoned"),
        [Event::Connect, Event::Reauthorize]
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Returns a dial-only node on `endpoint`, so several act as one peer with several sessions.
fn dialer(endpoint: &Endpoint) -> Node {
    Node::new(endpoint.clone(), NodeConfig::default())
}

/// Shuts the dial-only `nodes` down and closes the endpoint they share.
async fn close(nodes: &[&Node], endpoint: &Endpoint) {
    for node in nodes {
        node.shutdown().await;
    }
    endpoint.close().await;
}

/// Returns the sessions `node` lists with `peer`.
fn sessions_with(node: &Node, peer: EndpointId) -> Vec<Session> {
    node.sessions()
        .get()
        .into_iter()
        .filter(|session| session.peer() == peer)
        .collect()
}

/// A session that registers while a queued reauthorization waits does not keep the old rights.
#[tokio::test]
#[traced_test]
async fn a_session_opened_during_a_queued_reauthorize_is_rejected_with_the_rest() {
    let queue = policy::Queue::new(16);
    let alice = Peer::bare(NodeConfig::default().with_policy(queue.clone())).await;
    let bob_endpoint = endpoint().await;
    let bob = bob_endpoint.id();
    let first = dialer(&bob_endpoint);

    let dial = tokio::spawn({
        let first = first.clone();
        let addr = alice.endpoint.addr();
        async move { first.connect(addr).await }
    });
    let pending = step("bob asks to connect", queue.next())
        .await
        .expect("a request");
    pending.admit(Grant::everything());
    let first_session = step("bob's first dial", dial)
        .await
        .expect("no panic")
        .expect("admitted");
    eventually("alice registers bob", || {
        alice.node.grant_for(bob).is_some()
    })
    .await;

    alice.node.reauthorize(bob);
    let held = step("the application is asked", queue.next())
        .await
        .expect("a request");
    assert_eq!(held.request().event(), Event::Reauthorize);

    // Bob opens a second session while the application decides.
    let second = dialer(&bob_endpoint);
    let second_session = step("bob's second dial", second.connect(alice.endpoint.addr()))
        .await
        .expect("the stored authorization admits it");
    eventually("alice registers both sessions", || {
        sessions_with(&alice.node, bob).len() == 2
    })
    .await;

    held.reject(Rejection::new("removed"));
    step("the first session closes", first_session.closed()).await;
    step("the second session closes", second_session.closed()).await;
    eventually("alice forgets bob", || {
        !has_session_with(&alice.node, bob) && alice.node.grant_for(bob).is_none()
    })
    .await;

    close(&[&first, &second], &bob_endpoint).await;
    alice.shutdown().await;
}

/// A policy whose `Connect` decision for the path `slow` waits for the test.
///
/// It decides that one as the policy stood when it started, as a snapshot
/// taken before a rejection would.
#[derive(Debug, Clone, Default)]
struct Gated {
    rejecting: Arc<AtomicBool>,
    started: Arc<Notify>,
    gate: Arc<Notify>,
}

impl Policy<EndpointId> for Gated {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        let admitted = !self.rejecting.load(Ordering::SeqCst);
        if request.event() == Event::Connect && request.path().trim_start_matches('/') == "slow" {
            self.started.notify_one();
            self.gate.notified().await;
        }
        match admitted {
            true => Ok(Grant::everything().into()),
            false => Err(Rejection::new("rejected")),
        }
    }
}

/// A session decided before a rejection registers after it with nothing, and closes.
#[tokio::test]
#[traced_test]
async fn a_session_decided_before_a_rejection_does_not_undo_it() {
    let policy = Gated::default();
    let alice = Peer::bare(NodeConfig::default().with_policy(policy.clone())).await;
    let bob_endpoint = endpoint().await;
    let bob = bob_endpoint.id();

    // The slow session's decision starts while bob holds nothing.
    let slow = dialer(&bob_endpoint);
    let slow_dial = tokio::spawn({
        let slow = slow.clone();
        let addr = alice.endpoint.addr();
        async move {
            let options = ConnectOptions {
                path: Some("slow".into()),
                ..Default::default()
            };
            slow.connect_with(addr, options).await
        }
    });
    step("the slow decision starts", policy.started.notified()).await;

    let fast = dialer(&bob_endpoint);
    let fast_session = step("bob's fast dial", fast.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");
    eventually("alice registers bob", || {
        alice.node.grant_for(bob).is_some()
    })
    .await;

    policy.rejecting.store(true, Ordering::SeqCst);
    alice.node.reauthorize(bob);
    step("the fast session closes", fast_session.closed()).await;
    eventually("alice forgets bob's rights", || {
        alice.node.grant_for(bob).is_none()
    })
    .await;

    // The slow decision admits, as the policy stood before the rejection.
    policy.gate.notify_one();
    let slow_session = step("bob's slow dial", slow_dial)
        .await
        .expect("no panic")
        .expect("bob's side of the handshake completes");
    eventually("alice registers the slow session or refuses it", || {
        alice
            .node
            .grant_for(bob)
            .is_none_or(|current| current.grant != Grant::everything())
    })
    .await;
    step("the slow session closes", slow_session.closed()).await;
    eventually("alice holds nothing for bob", || {
        !has_session_with(&alice.node, bob) && alice.node.grant_for(bob).is_none()
    })
    .await;

    close(&[&slow, &fast], &bob_endpoint).await;
    alice.shutdown().await;
}

/// A peer that opens sessions faster than its revalidation interval is still revalidated.
#[tokio::test]
#[traced_test]
async fn a_peer_that_keeps_reconnecting_is_still_revalidated() {
    let revalidations = Arc::new(AtomicUsize::new(0));
    let counted = revalidations.clone();
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        move |request: &PeerRequest| {
            if request.event() == Event::Revalidate {
                counted.fetch_add(1, Ordering::SeqCst);
            }
            Ok(Authorization {
                grant: Grant::everything(),
                revalidate: Some(Duration::from_millis(300)),
                ..Default::default()
            })
        },
    )))
    .await;
    let bob_endpoint = endpoint().await;
    let mut nodes = vec![dialer(&bob_endpoint)];
    step("bob dials", nodes[0].connect(alice.endpoint.addr()))
        .await
        .expect("admitted");

    let churn = tokio::time::Instant::now() + Duration::from_millis(1200);
    while tokio::time::Instant::now() < churn && revalidations.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let node = dialer(&bob_endpoint);
        step("bob dials again", node.connect(alice.endpoint.addr()))
            .await
            .expect("admitted");
        nodes.push(node);
    }
    assert!(
        revalidations.load(Ordering::SeqCst) > 0,
        "new sessions every 200 ms held off a 300 ms revalidation"
    );

    close(&nodes.iter().collect::<Vec<_>>(), &bob_endpoint).await;
    alice.shutdown().await;
}

/// A peer removed while this node dials it never holds the grant it had before.
#[tokio::test]
#[traced_test]
async fn a_peer_removed_during_a_dial_never_gets_its_old_grant() {
    let contacts = Arc::new(Mutex::new(BTreeSet::new()));
    let started = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let policy = Contacts {
        contacts: contacts.clone(),
        started: started.clone(),
        gate: gate.clone(),
    };
    let alice = Peer::bare(NodeConfig::default().with_policy(policy)).await;
    let bob = Peer::spawn().await;
    contacts.lock().expect("poisoned").insert(bob.id());

    let dial = tokio::spawn({
        let node = alice.node.clone();
        let addr = bob.endpoint.addr();
        async move { node.connect(addr).await }
    });
    step("alice's policy decides the dial", started.notified()).await;
    contacts.lock().expect("poisoned").remove(&bob.id());
    alice.node.reauthorize(bob.id());
    gate.notify_one();

    let session = step("the dial completes", dial)
        .await
        .expect("no panic")
        .expect("the dial itself succeeds");
    assert_eq!(
        session.grant(),
        Grant::nothing(),
        "the decision from before the removal never applies"
    );
    step("the re-decide closes the session", session.closed()).await;
    eventually("alice holds nothing for bob", || {
        alice.node.grant_for(bob.id()).is_none()
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A contacts policy whose dialed `Connect` decision checks first and then waits for the test.
#[derive(Debug, Clone)]
struct Contacts {
    contacts: Arc<Mutex<BTreeSet<EndpointId>>>,
    started: Arc<Notify>,
    gate: Arc<Notify>,
}

impl Policy<EndpointId> for Contacts {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        let contact = self
            .contacts
            .lock()
            .expect("poisoned")
            .contains(request.peer());
        if request.event() == Event::Connect && request.dialed() {
            self.started.notify_one();
            self.gate.notified().await;
        }
        match contact {
            true => Ok(Grant::everything().into()),
            false => Err(Rejection::new("not a contact")),
        }
    }
}

/// A policy that changes all the time and decides slowly still applies a rejection.
#[derive(Debug, Clone, Default)]
struct Chatty {
    rejecting: Arc<AtomicBool>,
}

impl Policy<EndpointId> for Chatty {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if request.event() != Event::Connect && self.rejecting.load(Ordering::SeqCst) {
            return Err(Rejection::new("revoked"));
        }
        Ok(Grant::everything().into())
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        Some(Box::pin(n0_future::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Some(((), ()))
        })))
    }
}

#[tokio::test]
#[traced_test]
async fn a_rejection_lands_while_the_policy_keeps_changing() {
    let policy = Chatty::default();
    let alice = Peer::bare(NodeConfig::default().with_policy(policy.clone())).await;
    let bob = Peer::spawn().await;
    let session = step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");

    policy.rejecting.store(true, Ordering::SeqCst);
    step("the session closes", session.closed()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}
