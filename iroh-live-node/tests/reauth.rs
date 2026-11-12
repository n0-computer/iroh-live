//! Re-decides: the policy is asked again at expiry and on request.
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

use common::{Peer, endpoint, ends, eventually, has_session_with, publish, reading, step};
use iroh::{Endpoint, EndpointId};
use iroh_live_node::{
    Audience, ConnectOptions, Error, Grant, Node, NodeConfig, Reach, Session,
    policy::{self, Authorization, Decision, Event, PeerRequest, Policy, Rejection},
};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;
use tokio::sync::Notify;

/// How long an authorization in these tests lasts before the node asks again.
const SHORT: Duration = Duration::from_millis(400);

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
                }),
                Event::Reauthorize => Ok(Grant::subscribe_only().into()),
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
async fn a_rejection_at_expiry_closes_the_sessions_with_its_code() {
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        |request: &PeerRequest| match request.event() {
            Event::Connect => Ok(Authorization {
                grant: Grant::subscribe_only(),
                expires: Some(SystemTime::now() + SHORT),
            }),
            Event::Reauthorize => Err(Rejection::new("revoked").with_code(moq_net::Error::App(42))),
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

#[tokio::test]
#[traced_test]
async fn reauthorize_all_decides_every_peer_again() {
    let grant = Arc::new(Mutex::new(Grant::everything()));
    let current = grant.clone();
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        move |_request: &PeerRequest| Ok(current.lock().expect("poisoned").clone().into()),
    )))
    .await;
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

    *grant.lock().expect("poisoned") = Grant::subscribe_only();
    alice.node.reauthorize_all();
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
async fn a_queue_asks_the_application_to_reauthorize() {
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
                Event::Connect => pending.admit(Grant::everything()),
                Event::Reauthorize => pending.admit(Grant::subscribe_only()),
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

/// A policy that decides slowly, and rejects every re-decide once the test says so.
#[derive(Debug, Clone, Default)]
struct Slow {
    rejecting: Arc<AtomicBool>,
}

impl Policy<EndpointId> for Slow {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if request.event() != Event::Connect && self.rejecting.load(Ordering::SeqCst) {
            return Err(Rejection::new("revoked"));
        }
        Ok(Grant::everything().into())
    }
}

/// A rejection lands while the application asks for re-decides faster than the policy decides.
#[tokio::test]
#[traced_test]
async fn a_rejection_lands_while_the_triggers_keep_coming() {
    let policy = Slow::default();
    let alice = Peer::bare(NodeConfig::default().with_policy(policy.clone())).await;
    let bob = Peer::spawn().await;
    let session = step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");

    policy.rejecting.store(true, Ordering::SeqCst);
    let node = alice.node.clone();
    let _flood = n0_future::task::AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            node.reauthorize_all();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }));
    step("the session closes", session.closed()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// An expired authorization grants nothing before a slow application answers.
#[tokio::test]
#[traced_test]
async fn an_expired_authorization_narrows_before_a_slow_answer() {
    let queue = policy::Queue::new(16);
    let alice = Peer::bare(NodeConfig::default().with_policy(queue.clone())).await;
    let bob = Peer::spawn().await;
    let _answering = n0_future::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let mut slow = n0_future::task::JoinSet::new();
        while let Some(pending) = queue.next().await {
            match pending.request().event() {
                Event::Connect => pending.admit(Authorization {
                    grant: Grant::everything(),
                    expires: Some(SystemTime::now() + Duration::from_millis(300)),
                }),
                _ => {
                    slow.spawn(async move {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        pending.admit(Grant::subscribe_only());
                    });
                }
            }
        }
    }));

    step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");
    eventually("alice registers bob", || {
        sessions_with(&alice.node, bob.id()).len() == 1
    })
    .await;
    let session = sessions_with(&alice.node, bob.id()).remove(0);
    assert_eq!(session.grant(), Grant::everything());

    tokio::time::timeout(Duration::from_millis(1500), async {
        while session.grant() != Grant::nothing() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the grant lapses at expiry, before the application answers");
    eventually("the application's answer applies", || {
        session.grant() == Grant::subscribe_only()
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A queue too small for every re-decide at once keeps the sessions and asks again.
#[tokio::test]
#[traced_test]
async fn a_full_queue_keeps_the_sessions_and_asks_again() {
    const PEERS: usize = 5;
    let queue = policy::Queue::new(2);
    let alice = Peer::bare(NodeConfig::default().with_policy(queue.clone())).await;
    let asked = Arc::new(Mutex::new(BTreeSet::new()));
    let seen = asked.clone();
    let _answering = n0_future::task::AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(pending) = queue.next().await {
            if pending.request().event() != Event::Connect {
                seen.lock()
                    .expect("poisoned")
                    .insert(*pending.request().peer());
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            pending.admit(Grant::everything());
        }
    }));
    let mut peers = Vec::new();
    let mut sessions = Vec::new();
    for _ in 0..PEERS {
        let peer = Peer::spawn().await;
        sessions.push(
            step("a peer dials", peer.node.connect(alice.endpoint.addr()))
                .await
                .expect("admitted"),
        );
        let id = peer.id();
        eventually("alice registers the peer", || {
            alice.node.grant_for(id).is_some()
        })
        .await;
        peers.push(peer);
    }

    alice.node.reauthorize_all();
    eventually("every peer reaches the application", || {
        asked.lock().expect("poisoned").len() == PEERS
    })
    .await;
    for (peer, session) in peers.iter().zip(&sessions) {
        assert!(
            session.connection().close_reason().is_none(),
            "a full queue closed a session"
        );
        assert!(has_session_with(&alice.node, peer.id()));
    }

    alice.shutdown().await;
    for peer in &peers {
        peer.shutdown().await;
    }
}

/// A policy whose first re-decide takes longer than the node waits.
#[derive(Debug, Clone, Default)]
struct SlowOnce {
    redecides: Arc<AtomicUsize>,
}

impl Policy<EndpointId> for SlowOnce {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        if request.event() == Event::Reauthorize
            && self.redecides.fetch_add(1, Ordering::SeqCst) == 0
        {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Ok(Grant::everything().into())
    }
}

/// A re-decide that times out keeps the session and its authorization, and runs again.
#[tokio::test]
#[traced_test]
async fn a_redecide_that_times_out_keeps_the_session() {
    let policy = SlowOnce::default();
    let alice = Peer::bare(NodeConfig {
        decide_timeout: Duration::from_millis(300),
        ..NodeConfig::default().with_policy(policy.clone())
    })
    .await;
    let bob = Peer::spawn().await;
    let session = step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");

    eventually("alice registers bob", || {
        alice.node.grant_for(bob.id()).is_some()
    })
    .await;
    alice.node.reauthorize(bob.id());
    eventually("a re-decide after the timeout succeeds", || {
        policy.redecides.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert!(
        session.connection().close_reason().is_none(),
        "the timeout closed the session"
    );
    assert_eq!(
        alice.node.grant_for(bob.id()).map(|current| current.grant),
        Some(Grant::everything())
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// An expiry beyond the clock's range means never, and the node keeps running.
#[tokio::test]
#[traced_test]
async fn an_expiry_beyond_the_clock_is_never() {
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        |_request: &PeerRequest| {
            Ok(Authorization {
                grant: Grant::everything(),
                expires: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::MAX / 4)),
            })
        },
    )))
    .await;
    let bob = Peer::spawn().await;
    step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");
    eventually("alice registers bob", || {
        alice.node.grant_for(bob.id()).is_some()
    })
    .await;

    // The node still answers: a second peer connects.
    let carol = Peer::spawn().await;
    step("carol dials", carol.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");
    eventually("alice registers carol", || {
        alice.node.grant_for(carol.id()).is_some()
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// Reauthorizing a peer the node holds no session with asks the policy nothing.
#[tokio::test]
#[traced_test]
async fn reauthorizing_a_peer_without_a_session_asks_nothing() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        move |request: &PeerRequest| {
            seen.lock()
                .expect("poisoned")
                .push((*request.peer(), request.event()));
            Ok(Grant::subscribe_only().into())
        },
    )))
    .await;
    let stranger = iroh::SecretKey::generate().public();
    alice.node.reauthorize(stranger);
    assert_eq!(alice.node.grant_for(stranger), None);

    // The node still decides a new session once, at its connect.
    let bob = Peer::spawn().await;
    step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");
    eventually("alice registers bob", || {
        alice.node.grant_for(bob.id()).is_some()
    })
    .await;
    assert_eq!(
        *events.lock().expect("poisoned"),
        [(bob.id(), Event::Connect)]
    );
    assert_eq!(alice.node.grant_for(stranger), None);

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A shutdown while the application holds a re-decide finishes, and closes the peer's sessions.
#[tokio::test]
#[traced_test]
async fn a_shutdown_during_a_held_reauthorize_finishes() {
    let queue = policy::Queue::new(16);
    let alice = Peer::bare(NodeConfig {
        decide_timeout: Duration::from_secs(600),
        ..NodeConfig::default().with_policy(queue.clone())
    })
    .await;
    let bob = Peer::spawn().await;
    let held = Arc::new(Mutex::new(Vec::new()));
    let holding = held.clone();
    let _answering = n0_future::task::AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(pending) = queue.next().await {
            match pending.request().event() {
                Event::Connect => pending.admit(Grant::everything()),
                // Never answered while the test runs.
                _ => holding.lock().expect("poisoned").push(pending),
            }
        }
    }));

    let session = step("bob dials", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("admitted");
    eventually("alice registers bob", || {
        alice.node.grant_for(bob.id()).is_some()
    })
    .await;
    alice.node.reauthorize(bob.id());
    eventually("the re-decide waits for the application", || {
        !held.lock().expect("poisoned").is_empty()
    })
    .await;

    step("the shutdown ends", alice.node.shutdown()).await;
    assert!(alice.node.sessions().get().is_empty());
    step("bob sees the close", session.closed()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}
