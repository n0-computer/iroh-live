//! Ticket claims and ticket subscriptions: a peer's grant, widened to one path.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use common::{Peer, announced, ends, eventually, publish, reading, stays_pending, step};
use iroh::EndpointId;
use iroh_live_node::{
    Audience, Error, Grant, Layout, NodeConfig, Reach, Session,
    policy::{self, Authorization, PeerRequest, Rejection, Transit},
};
use moq_net::{Path, Pattern, Patterns};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;

/// Returns a node whose default policy lets every peer subscribe and publish nothing.
async fn viewer() -> Peer {
    Peer::bare(NodeConfig::default()).await
}

/// Returns a node whose policy grants what `current` holds, or [`common::own_paths`] while it holds `None`.
async fn switchable(current: Arc<Mutex<Option<Grant>>>) -> Peer {
    let policy = policy::from_fn(move |request: &PeerRequest| {
        let grant = current.lock().expect("poisoned").clone();
        Ok(grant
            .unwrap_or_else(|| common::own_paths(*request.peer()))
            .into())
    });
    Peer::bare(NodeConfig::default().with_policy(policy)).await
}

/// Returns the grant `node`'s policy gave the peer of `session`, without its claims.
fn policy_grant(node: &Peer, session: &Session) -> Grant {
    node.node
        .grant_for(session.peer())
        .map_or_else(Grant::nothing, |authorization| authorization.grant)
}

/// Waits until `node` holds `grant` as the policy grant of `peer`.
async fn until_granted(node: &Peer, peer: EndpointId, grant: &Grant) {
    eventually("the policy grant changes", || {
        node.node
            .grant_for(peer)
            .map(|current| current.grant)
            .as_ref()
            == Some(grant)
    })
    .await;
}

fn path_of(peer: &Peer, name: &str) -> Path {
    Path::new(&peer.path(name)).expect("path")
}

#[tokio::test]
#[traced_test]
async fn a_default_viewer_reads_a_claimed_path() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");

    let err = step(
        "refused",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect_err("the default grant lets alice publish nothing");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");

    let claim = bob
        .node
        .claim_path(alice.id(), publication.path())
        .expect("claim");
    let subscription = step(
        "subscribe",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe with the claim");
    reading(&subscription.broadcast()).await;
    drop(claim);

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_claim_widens_a_session_bob_accepted() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let session = step("dial", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("alice dials bob");
    assert!(session.dialed());

    let _claim = bob
        .node
        .claim_path(alice.id(), publication.path())
        .expect("claim");
    // The live session applies the wider grant a moment after the claim, so a
    // read that starts at once can still be refused. Retry until it is let in.
    let deadline = tokio::time::Instant::now() + common::TIMEOUT;
    let subscription = loop {
        let subscription = step(
            "subscribe",
            bob.node
                .subscribe(publication.path(), Reach::Direct(alice.id())),
        )
        .await
        .expect("subscribe over the accepted session");
        let track = subscription
            .broadcast()
            .track("video")
            .expect("track")
            .subscribe(common::subscription())
            .await;
        if let Ok(mut track) = track
            && matches!(track.recv_group().await, Ok(Some(_)))
        {
            break subscription;
        }
        assert!(tokio::time::Instant::now() < deadline, "never let in");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(!subscription.session().expect("a session").dialed());

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_forged_path_is_refused() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;

    for forged in [
        bob.path("cam"),
        format!("live/{}/{}/cam", alice.id(), bob.id()),
        "live/someone/cam".to_string(),
        format!(
            "live/{}/cam",
            data_encoding::BASE32_NOPAD
                .encode(alice.id().as_bytes())
                .to_ascii_lowercase()
        ),
        format!("live/{}/{}", alice.id(), alice.id()),
    ] {
        let path = Path::new(&forged).expect("path");
        let err = bob
            .node
            .claim_path(alice.id(), &path)
            .expect_err("alice does not own it");
        assert!(matches!(err, Error::NotOwner { .. }), "{forged}: {err:#}");
    }

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn dropping_the_last_claim_narrows_the_grant_again() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let path = publication.path();

    let first = bob.node.claim_path(alice.id(), path).expect("claim");
    let second = bob.node.claim_path(alice.id(), path).expect("claim again");
    let subscription = step(
        "subscribe",
        bob.node.subscribe(path, Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    let mut track = reading(&subscription.broadcast()).await;
    let session = subscription.session().expect("a session");
    assert!(session.grant().allows_publish(path.as_str()));

    drop(first);
    assert!(
        session.grant().allows_publish(path.as_str()),
        "one claim still holds the path"
    );
    drop(second);
    assert!(!session.grant().allows_publish(path.as_str()));
    ends("the path left the grant", &mut track).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_narrower_policy_grant_leaves_claimed_paths_alone() {
    let alice = Peer::spawn().await;
    let policy_grant = Arc::new(Mutex::new(Some(Grant::subscribe_only())));
    let bob = switchable(policy_grant.clone()).await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let path = publication.path();
    let session = step("dial", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");

    let _claim = bob.node.claim_path(alice.id(), path).expect("claim");
    *policy_grant.lock().expect("poisoned") = Some(Grant::nothing());
    bob.node.reauthorize(alice.id());
    until_granted(&bob, alice.id(), &Grant::nothing()).await;
    assert!(
        session.grant().allows_publish(path.as_str()),
        "a live claim survives a narrower base"
    );
    assert!(!session.grant().allows_publish("live/other"));

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Returns the sessions of `node` with `peer`, waiting up to a second for two.
///
/// The accepted session of a simultaneous dial registers a moment after the
/// dialed one.
async fn two_sessions_with(node: &iroh_live_node::Node, peer: iroh::EndpointId) -> Vec<Session> {
    let mut sessions = node.sessions();
    let with_peer = |all: Vec<Session>| -> Vec<Session> {
        all.into_iter()
            .filter(|listed| listed.peer() == peer)
            .collect()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let current = with_peer(sessions.get());
        if current.len() >= 2 {
            return current;
        }
        match tokio::time::timeout_at(deadline, sessions.updated()).await {
            Ok(Ok(_)) => {}
            _ => return current,
        }
    }
}

#[tokio::test]
#[traced_test]
async fn the_sessions_of_one_peer_share_its_grant() {
    // A simultaneous dial leaves two sessions with one peer on at least one
    // side almost every time. Try a few pairs until one does.
    const PAIRS: usize = 8;
    for _ in 0..PAIRS {
        let alice_grant = Arc::new(Mutex::new(None));
        let bob_grant = Arc::new(Mutex::new(None));
        let alice = switchable(alice_grant.clone()).await;
        let bob = switchable(bob_grant.clone()).await;
        let (at_alice, at_bob) = step("both dial at once", async {
            tokio::join!(
                alice.node.connect(bob.endpoint.addr()),
                bob.node.connect(alice.endpoint.addr()),
            )
        })
        .await;
        at_alice.expect("alice's dial failed");
        at_bob.expect("bob's dial failed");

        let mut checked = false;
        for (node, peer, policy_grant) in [(&alice, &bob, &alice_grant), (&bob, &alice, &bob_grant)]
        {
            let with_peer = two_sessions_with(&node.node, peer.id()).await;
            let [first, second] = with_peer.as_slice() else {
                continue;
            };
            assert_eq!(first.grant(), common::own_paths(peer.id()));
            assert_eq!(first.grant(), second.grant());

            for grant in [Grant::nothing(), Grant::everything()] {
                *policy_grant.lock().expect("poisoned") = Some(grant.clone());
                node.node.reauthorize(peer.id());
                until_granted(node, peer.id(), &grant).await;
                assert_eq!(first.grant(), grant, "on the first session");
                assert_eq!(second.grant(), grant, "on the second session");
            }
            checked = true;
        }

        alice.shutdown().await;
        bob.shutdown().await;
        if checked {
            return;
        }
    }
    panic!("no simultaneous dial in {PAIRS} pairs left two sessions with one peer");
}

#[tokio::test]
#[traced_test]
async fn a_peer_holds_a_bounded_number_of_claims() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;

    let claims: Vec<_> = (0..64)
        .map(|n| {
            bob.node
                .claim_path(alice.id(), &path_of(&alice, &format!("cam{n}")))
                .expect("within the limit")
        })
        .collect();
    let err = bob
        .node
        .claim_path(alice.id(), &path_of(&alice, "one-too-many"))
        .expect_err("over the limit");
    assert!(matches!(err, Error::TooManyGrants { .. }), "{err:#}");
    drop(claims);
    bob.node
        .claim_path(alice.id(), &path_of(&alice, "one-too-many"))
        .expect("room again");

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_claim_stays_on_its_peers_links() {
    let alice = Peer::spawn().await;
    let carol = Peer::spawn().await;
    let bob = viewer().await;
    let with_alice = step("dial", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let with_carol = step("dial", bob.node.connect(carol.endpoint.addr()))
        .await
        .expect("connect");

    let path = path_of(&alice, "cam");
    let _claim = bob.node.claim_path(alice.id(), &path).expect("claim");
    assert!(with_alice.grant().allows_publish(path.as_str()));
    assert_eq!(with_carol.grant(), policy_grant(&bob, &with_carol));

    alice.shutdown().await;
    carol.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn concurrent_claims_all_land() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let session = step("dial", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");

    let tasks: Vec<_> = (0..8)
        .map(|n| {
            let node = bob.node.clone();
            let (peer, path) = (alice.id(), path_of(&alice, &format!("cam{n}")));
            tokio::spawn(async move { node.claim_path(peer, &path).expect("claim") })
        })
        .collect();
    let mut claims = Vec::new();
    for task in tasks {
        claims.push(task.await.expect("task"));
    }
    for n in 0..8 {
        let path = path_of(&alice, &format!("cam{n}"));
        assert!(session.grant().allows_publish(path.as_str()), "cam{n}");
    }
    drop(claims);
    assert_eq!(session.grant(), policy_grant(&bob, &session));

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn claims_are_capped_by_bytes_and_repeats_are_free() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;

    let long = |n: usize| {
        let name = format!("{n}{}", "x".repeat(3000));
        path_of(&alice, &name)
    };
    let held: Vec<_> = (0..5)
        .map(|n| bob.node.claim_path(alice.id(), &long(n)).expect("room"))
        .collect();
    let err = bob
        .node
        .claim_path(alice.id(), &long(5))
        .expect_err("over 16 KiB");
    assert!(matches!(err, Error::TooManyGrants { .. }), "{err:#}");
    bob.node
        .claim_path(alice.id(), &long(0))
        .expect("a held path repeats freely");
    drop(held);

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A layout in which one peer owns `fleet/cam` and nothing else.
#[derive(Debug)]
struct Fleet(EndpointId);

impl Layout<EndpointId> for Fleet {
    fn scope(&self, peer: &EndpointId) -> Patterns {
        if *peer == self.0 {
            Patterns::from(Pattern::literal("fleet/cam").expect("a pattern"))
        } else {
            Patterns::new()
        }
    }

    fn owner(&self, path: &str) -> Option<EndpointId> {
        (path == "fleet/cam").then_some(self.0)
    }
}

#[tokio::test]
#[traced_test]
async fn a_layout_replaces_the_generic_owner_rule() {
    let alice = Peer::spawn().await;
    let bob = Peer::bare(NodeConfig {
        layout: Some(Arc::new(Fleet(alice.id()))),
        ..NodeConfig::default().with_policy(policy::Open(Grant::nothing()))
    })
    .await;

    bob.node
        .claim_path(alice.id(), &Path::new("fleet/cam").expect("path"))
        .expect("the layout names alice the owner");
    let err = bob
        .node
        .claim_path(alice.id(), &path_of(&alice, "cam"))
        .expect_err("the generic rule no longer applies");
    assert!(matches!(err, Error::NotOwner { .. }), "{err:#}");

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_forged_claim_reaches_nothing() {
    let alice = Peer::spawn().await;
    let mallory = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let mut announces = bob.node.origin().announced();

    bob.node
        .claim_path(mallory.id(), publication.path())
        .expect_err("alice's path is not mallory's");
    let session = step("dial", mallory.node.connect(bob.endpoint.addr()))
        .await
        .expect("mallory dials bob");
    let forged = publish(&mallory.node, alice.path("cam"), Audience::Granted);
    drop(forged);
    stays_pending(
        "nothing reaches bob's table",
        Duration::from_millis(500),
        announced(&mut announces, publication.path().as_str()),
    )
    .await;
    assert_eq!(session.grant(), policy_grant(&mallory, &session));

    alice.shutdown().await;
    mallory.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_default_viewer_subscribes_to_a_ticket() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let ticket = publication.ticket();
    assert_eq!(ticket.peer(), alice.id());

    let subscription = step("subscribe", bob.node.subscribe_ticket(&ticket))
        .await
        .expect("a ticket subscribe needs no grant");
    reading(&subscription.broadcast()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_ticket_subscription_releases_its_claim_on_drop() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let subscription = step(
        "subscribe",
        bob.node.subscribe_ticket(&publication.ticket()),
    )
    .await
    .expect("subscribe");
    let session = subscription.session().expect("a session");
    let clone = subscription.clone();
    assert_ne!(session.grant(), policy_grant(&bob, &session));

    drop(subscription);
    assert_ne!(
        session.grant(),
        policy_grant(&bob, &session),
        "a clone holds it"
    );
    drop(clone);
    assert_eq!(session.grant(), policy_grant(&bob, &session));

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_cancelled_ticket_subscribe_releases_its_claim() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let session = step("dial", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let ticket = iroh_live_node::BroadcastTicket::at(alice.id(), path_of(&alice, "never"));

    let waited = tokio::time::timeout(
        Duration::from_millis(500),
        bob.node.subscribe_ticket(&ticket),
    )
    .await;
    assert!(waited.is_err(), "nothing is published at the path");
    assert_eq!(session.grant(), policy_grant(&bob, &session));

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_covered_ticket_claims_nothing() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");

    let subscription = step(
        "subscribe",
        bob.node.subscribe_ticket(&publication.ticket()),
    )
    .await
    .expect("subscribe");
    let session = subscription.session().expect("a session");
    assert_eq!(session.grant(), policy_grant(&bob, &session));

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn an_own_ticket_resolves_locally() {
    let alice = Peer::spawn().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");

    let subscription = step(
        "subscribe",
        alice.node.subscribe_ticket(&publication.ticket()),
    )
    .await
    .expect("subscribe");
    assert!(subscription.session().is_none(), "no session with oneself");
    reading(&subscription.broadcast()).await;

    alice.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_forged_ticket_fails_not_owner() {
    let alice = Peer::spawn().await;
    let mallory = Peer::spawn().await;
    let bob = viewer().await;
    let forged = iroh_live_node::BroadcastTicket::at(mallory.id(), path_of(&alice, "cam"));

    let err = step("subscribe", bob.node.subscribe_ticket(&forged))
        .await
        .expect_err("mallory does not own the path");
    assert!(matches!(err, Error::NotOwner { .. }), "{err:#}");

    alice.shutdown().await;
    mallory.shutdown().await;
    bob.shutdown().await;
}

/// Returns a node whose policy rejects `banned` and lets everyone else subscribe.
async fn rejecting(banned: iroh::EndpointId) -> Peer {
    Peer::bare(
        NodeConfig::default().with_policy(policy::from_fn(move |request: &PeerRequest| {
            if *request.peer() == banned {
                Err(Rejection::new("banned"))
            } else {
                Ok(Grant::subscribe_only().into())
            }
        })),
    )
    .await
}

/// Waits until `node` holds a session with `peer`, and returns it.
async fn session_with(node: &iroh_live_node::Node, peer: iroh::EndpointId) -> Session {
    let mut sessions = node.sessions();
    step("the session registers", async {
        loop {
            if let Some(session) = sessions.get().into_iter().find(|s| s.peer() == peer) {
                return session;
            }
            sessions.updated().await.expect("the node is alive");
        }
    })
    .await
}

/// Reports whether `node` lists a session with `peer`.
fn has_session_with(node: &iroh_live_node::Node, peer: iroh::EndpointId) -> bool {
    node.sessions()
        .get()
        .iter()
        .any(|session| session.peer() == peer)
}

/// Waits for a session the remote side refuses to end, or for the dial to fail.
async fn refused(dial: Result<Session, Error>) {
    match dial {
        Ok(session) => {
            step("the refused session closes", session.closed()).await;
        }
        Err(err) => assert!(matches!(err, Error::Refused { .. }), "{err:#}"),
    }
}

#[tokio::test]
#[traced_test]
async fn a_policy_rejection_refuses_an_accept_and_fails_a_dial() {
    let mallory = Peer::spawn().await;
    let bob = rejecting(mallory.id()).await;

    refused(step("mallory dials", mallory.node.connect(bob.endpoint.addr())).await).await;
    assert!(!has_session_with(&bob.node, mallory.id()));

    let err = step("bob dials", bob.node.connect(mallory.endpoint.addr()))
        .await
        .expect_err("bob's own policy rejects mallory");
    let Error::Rejected { source, .. } = &err else {
        panic!("not a rejection: {err:#}");
    };
    assert_eq!(source.reason(), "banned");
    assert!(
        !has_session_with(&mallory.node, bob.id()),
        "the dial never reached mallory"
    );

    // Others still get in.
    let carol = Peer::spawn().await;
    step("carol dials", carol.node.connect(bob.endpoint.addr()))
        .await
        .expect("carol is admitted");
    session_with(&bob.node, carol.id()).await;

    mallory.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_policy_grant_holds_in_both_directions() {
    let fleet = |peer: &iroh::EndpointId| -> Grant {
        Grant::publish_under(
            format!("fleet/{peer}/**")
                .parse::<moq_net::Pattern>()
                .expect("pattern"),
        )
    };
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        move |request: &PeerRequest| Ok(fleet(request.peer()).into()),
    )))
    .await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;
    let bob_cam = publish(
        &bob.node,
        format!("fleet/{}/cam", bob.id()),
        Audience::Granted,
    )
    .expect("publish");
    let carol_cam = publish(
        &carol.node,
        format!("fleet/{}/cam", carol.id()),
        Audience::Granted,
    )
    .expect("publish");

    // Alice dials bob: her policy decides the dialed session.
    let from_bob = step(
        "alice dials bob",
        alice
            .node
            .subscribe(bob_cam.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect("the policy lets bob publish under fleet");
    let session = from_bob.session().expect("a session");
    assert!(session.dialed());
    assert_eq!(session.grant(), fleet(&bob.id()));
    reading(&from_bob.broadcast()).await;

    // Carol dials alice: the same policy decides the accepted session.
    step(
        "carol dials alice",
        carol.node.connect(alice.endpoint.addr()),
    )
    .await
    .expect("carol connects");
    let accepted = session_with(&alice.node, carol.id()).await;
    assert!(!accepted.dialed());
    assert_eq!(accepted.grant(), fleet(&carol.id()));
    let from_carol = step(
        "alice subscribes over the accepted session",
        alice
            .node
            .subscribe(carol_cam.path(), Reach::Direct(carol.id())),
    )
    .await
    .expect("the policy lets carol publish under fleet");
    assert!(!from_carol.session().expect("a session").dialed());
    reading(&from_carol.broadcast()).await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_policy_that_asks_for_transit_closes_the_session() {
    let alice = Peer::bare(NodeConfig::default().with_policy(policy::from_fn(
        |_: &PeerRequest| {
            Ok(Authorization {
                grant: Grant::everything(),
                transit: Transit {
                    peer_forwards: true,
                    ..Default::default()
                },
                ..Default::default()
            })
        },
    )))
    .await;
    let bob = Peer::spawn().await;

    refused(step("bob dials", bob.node.connect(alice.endpoint.addr())).await).await;
    assert!(!has_session_with(&alice.node, bob.id()));

    let err = step("alice dials", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect_err("transit is refused");
    let Error::Rejected { source, .. } = &err else {
        panic!("not a rejection: {err:#}");
    };
    assert!(source.reason().contains("transit"), "{err:#}");

    alice.shutdown().await;
    bob.shutdown().await;
}
