//! Re-decides: the policy is asked again at expiry, on revalidation, on change and on request.
//!
//! These run on real time with short durations, and wait on conditions with
//! a timeout rather than for fixed spans.

mod common;

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use common::{Peer, ends, eventually, publish, reading, step};
use iroh::EndpointId;
use iroh_live_node::{
    Audience, Error, Grant, Node, NodeConfig, Reach,
    policy::{self, Authorization, Decision, Event, PeerRequest, Policy, Rejection},
};
use n0_future::{StreamExt, boxed::BoxStream};
use n0_tracing_test::traced_test;
use n0_watcher::{Watchable, Watcher};

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
