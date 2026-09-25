//! Publishing and subscribing on a node, over real QUIC connections.

mod common;

use std::{collections::BTreeSet, time::Duration};

use common::{
    Node, TIMEOUT, TestBroadcast, announced, ends, read_counter, reading, retracted, stays_pending,
    step,
};
use iroh_moq::{Admission, Audience, ConnectOptions, Error, Grant, LinkKind, MoqConfig, Reach};
use moq_net::{Pattern, Patterns};
use n0_future::task::AbortOnDropHandle;
use n0_tracing_test::traced_test;
use n0_watcher::{Watchable, Watcher};

/// A subscriber reaches a broadcast by dialing its publisher.
#[tokio::test]
#[traced_test]
async fn a_path_resolves_by_dialing_its_publisher() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish(alice.path("cam"), &broadcast.producer, Audience::Everyone)
        .expect("publish");
    let subscription = step(
        "subscribe",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.as_moq()).await;
    let session = subscription.session().expect("served by a direct session");
    assert_eq!(session.remote_id(), alice.id());
    let link = subscription.link().expect("a serving link");
    assert_eq!(link.kind, LinkKind::Direct);

    alice.shutdown().await;
    bob.shutdown().await;
}

/// The node's own public publication resolves locally, without a link.
#[tokio::test]
#[traced_test]
async fn an_own_publication_resolves_locally() {
    let alice = Node::spawn().await;
    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish(alice.path("cam"), &broadcast.producer, Audience::Everyone)
        .expect("publish");
    let subscription = step(
        "subscribe",
        alice
            .moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.as_moq()).await;
    assert!(subscription.link().is_none(), "served over a link");
    assert!(alice.moq.sessions().get().is_empty(), "dialed itself");
    alice.shutdown().await;
}

/// One path holds one publication, and a broadcast that ends frees its path.
#[tokio::test]
#[traced_test]
async fn a_path_holds_one_publication_until_its_broadcast_ends() {
    let alice = Node::spawn().await;
    let first = TestBroadcast::start();
    alice
        .moq
        .publish(alice.path("cam"), &first.producer, Audience::Everyone)
        .expect("publish");
    let second = TestBroadcast::start();
    let err = alice
        .moq
        .publish(alice.path("cam"), &second.producer, Audience::Everyone)
        .expect_err("a second publication at one path");
    assert!(matches!(err, Error::Duplicate { .. }), "{err:#}");

    // A broadcast that ended frees its path at once, before the node has
    // noticed on its own.
    first.producer.finish();
    alice
        .moq
        .publish(alice.path("cam"), &second.producer, Audience::Everyone)
        .expect("publish over an ended broadcast");

    let err = alice
        .moq
        .publish("", &second.producer, Audience::Everyone)
        .expect_err("an empty path");
    assert!(matches!(err, Error::InvalidPath { .. }), "{err:#}");
    alice.shutdown().await;
}

/// How long a negative check waits for something that must not happen.
const QUIET: Duration = Duration::from_secs(2);

/// A `Peers` publication reaches only its set, and follows the set as it changes.
#[tokio::test]
#[traced_test]
async fn a_peers_audience_follows_its_set() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let carol = Node::spawn().await;

    let members = Watchable::new(BTreeSet::from([bob.id()]));
    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish(
            alice.path("cam"),
            &broadcast.producer,
            Audience::Peers(members.clone()),
        )
        .expect("publish");

    let for_bob = step(
        "bob",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("bob is a member");
    reading(&for_bob.as_moq()).await;

    // Carol has a session, but nothing is offered on it.
    let session = step("carol connects", carol.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    stays_pending(
        "carol resolved the path",
        QUIET,
        session.subscribe(publication.path()),
    )
    .await;

    // Adding her to the set offers it on her open session.
    members.set(BTreeSet::from([bob.id(), carol.id()])).ok();
    let for_carol = step(
        "carol",
        carol
            .moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("carol after joining the set");
    reading(&for_carol.as_moq()).await;

    // Taking her out again withdraws the path from her, and only from her.
    let path = publication.path().as_str();
    let mut carol_updates = session.origin().announced();
    announced(&mut carol_updates, path).await;
    members.set(BTreeSet::from([bob.id()])).ok();
    retracted(&mut carol_updates, path).await;
    stays_pending(
        "carol resolved the path after leaving the set",
        QUIET,
        session.subscribe(publication.path()),
    )
    .await;
    read_counter(&for_bob.as_moq()).await;

    // An empty set offers to nobody.
    let mut bob_updates = bob.moq.origin().announced();
    announced(&mut bob_updates, path).await;
    members.set(BTreeSet::new()).ok();
    retracted(&mut bob_updates, path).await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// A peer's offers appear in the route table and on the session, and leave again.
#[tokio::test]
#[traced_test]
async fn offers_show_in_the_route_table() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let carol = Node::spawn().await;

    let members = Watchable::new(BTreeSet::from([bob.id()]));
    let broadcast = TestBroadcast::start();
    let path = alice.path("cam");
    let _publication = alice
        .moq
        .publish(&path, &broadcast.producer, Audience::Peers(members.clone()))
        .expect("publish");
    let session = step("bob connects", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    announced(&mut session.origin().announced(), &path).await;
    let mut updates = bob.moq.origin().announced();
    announced(&mut updates, &path).await;

    step("carol connects", carol.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    stays_pending(
        "carol saw an offer to bob",
        QUIET,
        announced(&mut carol.moq.origin().announced(), &path),
    )
    .await;

    members.set(BTreeSet::new()).ok();
    retracted(&mut updates, &path).await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// Unpublishing takes the path away from peers, and says so on the publication.
#[tokio::test]
#[traced_test]
async fn unpublishing_withdraws_the_path() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish(alice.path("cam"), &broadcast.producer, Audience::Everyone)
        .expect("publish");
    let subscription = step(
        "subscribe",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    reading(&subscription.as_moq()).await;
    let mut updates = bob.moq.origin().announced();
    announced(&mut updates, publication.path().as_str()).await;
    assert!(!publication.is_withdrawn());

    publication.unpublish();
    step("withdrawn", publication.withdrawn()).await;
    retracted(&mut updates, publication.path().as_str()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Closing a session ends what the peer reads.
///
/// Withdrawing a path leaves tracks already read running, as moq-lite wants,
/// so this is how a peer is cut off at once.
#[tokio::test]
#[traced_test]
async fn closing_a_session_cuts_the_peer_off() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish(alice.path("cam"), &broadcast.producer, Audience::Everyone)
        .expect("publish");
    let subscription = step(
        "subscribe",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    let mut bob_reading = reading(&subscription.as_moq()).await;

    let session = step("alice sees bob", session_with(&alice, bob.id())).await;
    session.close("cut off");
    ends("bob after alice closed the session", &mut bob_reading).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Manual admission checks a token, and the grant bounds what the peer sees.
#[tokio::test]
#[traced_test]
async fn manual_admission_checks_a_token_and_bounds_offers() {
    let alice = Node::with_config(MoqConfig {
        admission: Admission::Manual,
        ..Default::default()
    })
    .await;
    let bob = Node::spawn().await;
    let mallory = Node::spawn().await;

    let public = TestBroadcast::start();
    let public = (
        alice
            .moq
            .publish(alice.path("public"), &public.producer, Audience::Everyone)
            .expect("publish"),
        public,
    );
    let secret = TestBroadcast::start();
    let secret = (
        alice
            .moq
            .publish(alice.path("secret"), &secret.producer, Audience::Everyone)
            .expect("publish"),
        secret,
    );

    let alice_id = alice.id();
    let moq = alice.moq.clone();
    let accept_loop = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut sessions = Vec::new();
        while let Some(incoming) = moq.accept().await {
            if incoming.request().query("jwt") != Some("letmein") {
                incoming.reject(moq_net::Error::Unauthorized);
                continue;
            }
            let pattern: Pattern = format!("live/{alice_id}/public").parse().expect("pattern");
            let grant = Grant {
                subscribe: Patterns::from(pattern),
                publish: Patterns::new(),
            };
            sessions.push(incoming.admit(grant).await.expect("admit"));
        }
    }));

    let session = step(
        "bob connects with the token",
        bob.moq.connect_with(
            alice.endpoint.addr(),
            ConnectOptions {
                token: Some("letmein".into()),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("admitted");
    let subscription = step(
        "bob subscribes",
        bob.moq
            .subscribe(public.0.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("the offered publication");
    read_counter(&subscription.as_moq()).await;
    assert_eq!(session.remote_id(), alice.id());

    // The grant does not cover the secret one, so it is not offered.
    stays_pending(
        "bob resolved the secret one",
        QUIET,
        session.subscribe(secret.0.path()),
    )
    .await;

    // Without the token the session is refused. The dialer's half of the
    // handshake completes first, so the refusal arrives as the session closing.
    match step(
        "mallory connects",
        mallory.moq.connect(alice.endpoint.addr()),
    )
    .await
    {
        Err(err) => assert!(matches!(err, Error::Refused { .. }), "{err:#}"),
        Ok(session) => {
            let reason = step("the refused session closes", session.closed()).await;
            assert!(
                matches!(&reason, Error::SessionClosed { source, .. }
                    if !matches!(source, moq_net::Error::Cancel)),
                "a session without a token was admitted: {reason:#}"
            );
        }
    }
    assert!(
        !alice
            .moq
            .sessions()
            .get()
            .iter()
            .any(|session| session.remote_id() == mallory.id()),
        "mallory was admitted"
    );
    let err = step(
        "mallory subscribes",
        mallory
            .moq
            .subscribe(public.0.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect_err("mallory resolved a broadcast without a token");
    assert!(
        matches!(
            err,
            Error::NotAnnounced { .. } | Error::Refused { .. } | Error::Connect { .. }
        ),
        "{err:#}"
    );

    drop(accept_loop);
    alice.shutdown().await;
    bob.shutdown().await;
    mallory.shutdown().await;
}

/// A counter offset that tells a forged broadcast's frames from the real one's.
const FORGED: u64 = 1_000_000;

/// A peer cannot route another publisher's path through this node's table.
///
/// With a grant that keeps each peer to paths naming it, a path resolves to
/// its publisher's broadcast and never to a third peer's.
#[tokio::test]
#[traced_test]
async fn a_peer_cannot_route_another_publishers_path() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let mallory = Node::spawn().await;

    let real = TestBroadcast::start();
    let publication = alice
        .moq
        .publish(alice.path("cam"), &real.producer, Audience::Everyone)
        .expect("publish");
    let forged = TestBroadcast::starting_at(FORGED);
    let _forged = mallory
        .moq
        .publish(publication.path(), &forged.producer, Audience::Everyone)
        .expect("publish at alice's path");
    let own = mallory
        .moq
        .publish(mallory.path("cam"), &forged.producer, Audience::Everyone)
        .expect("publish");
    step("mallory connects", mallory.moq.connect(bob.endpoint.addr()))
        .await
        .expect("connect");
    let session = step("bob sees mallory", session_with(&bob, mallory.id())).await;

    // Her own path reaches bob's table, and the forged one does not even
    // reach her session: the grant refuses it.
    let mut table = bob.moq.origin().announced();
    announced(&mut table, own.path().as_str()).await;
    let mut from_mallory = session.origin().announced();
    let path = publication.path().as_str();
    tokio::join!(
        stays_pending(
            "mallory announced alice's path",
            QUIET,
            announced(&mut from_mallory, path),
        ),
        stays_pending(
            "mallory routed alice's path",
            QUIET,
            announced(&mut table, path),
        ),
    );

    let subscription = step(
        "subscribe",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    assert_eq!(
        subscription.session().map(|session| session.remote_id()),
        Some(alice.id()),
        "served by someone other than the publisher"
    );
    assert!(read_counter(&subscription.as_moq()).await < FORGED);

    alice.shutdown().await;
    bob.shutdown().await;
    mallory.shutdown().await;
}

/// A subscriber that starts before its publisher resolves the path once published.
#[tokio::test]
#[traced_test]
async fn a_subscriber_started_first_gets_the_named_path() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let path = alice.path("cam");
    let subscribing = tokio::spawn({
        let moq = bob.moq.clone();
        let (path, publisher) = (path.clone(), alice.id());
        async move { moq.subscribe(path, Reach::Direct(publisher)).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let broadcast = TestBroadcast::start();
    let _publication = alice
        .moq
        .publish(alice.path("cam"), &broadcast.producer, Audience::Everyone)
        .expect("publish");

    let subscription = step("subscribe", subscribing)
        .await
        .expect("task")
        .expect("subscribe");
    assert_eq!(subscription.path().as_str(), path);
    read_counter(&subscription.as_moq()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Dials `to` from `dialer` and waits for the session to be refused.
async fn refused(dialer: &Node, to: &Node, within: Duration) {
    match step("connect", dialer.moq.connect(to.endpoint.addr())).await {
        Err(_) => {}
        Ok(session) => {
            tokio::time::timeout(within, session.closed())
                .await
                .expect("the refused session stayed open");
        }
    }
    assert!(
        !to.moq
            .sessions()
            .get()
            .iter()
            .any(|session| session.remote_id() == dialer.id()),
        "the session was admitted"
    );
}

/// An incoming session the application drops without deciding is refused.
#[tokio::test]
#[traced_test]
async fn an_undecided_session_is_refused() {
    let alice = Node::with_config(MoqConfig {
        admission: Admission::Manual,
        ..Default::default()
    })
    .await;
    let bob = Node::spawn().await;
    let moq = alice.moq.clone();
    let _accept = AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(incoming) = moq.accept().await {
            drop(incoming);
        }
    }));
    refused(&bob, &alice, TIMEOUT).await;
    alice.shutdown().await;
    bob.shutdown().await;
}

/// An `accept` parked in an unpolled future does not hold up the shutdown.
#[tokio::test]
#[traced_test]
async fn a_parked_accept_does_not_hold_up_shutdown() {
    let alice = Node::with_config(MoqConfig {
        admission: Admission::Manual,
        ..Default::default()
    })
    .await;
    let mut accepting = Box::pin(alice.moq.accept());
    assert!(
        n0_future::future::poll_once(&mut accepting).await.is_none(),
        "nothing to accept yet"
    );
    step("the shutdown ends", alice.moq.shutdown()).await;
    assert!(
        step("the parked accept returns", accepting).await.is_none(),
        "accept handed out a session after the shutdown"
    );
    alice.shutdown().await;
}

/// Shutting the router down shuts the node down.
#[tokio::test]
#[traced_test]
async fn the_router_shuts_the_node_down() {
    let alice = Node::spawn().await;
    step("router shutdown", alice.router.shutdown())
        .await
        .expect("router");
    let broadcast = TestBroadcast::start();
    let err = alice
        .moq
        .publish(alice.path("cam"), &broadcast.producer, Audience::Everyone)
        .expect_err("publish after the router shut down");
    assert!(matches!(err, Error::ShutDown { .. }), "{err:#}");
    alice.shutdown().await;
}

/// A peer publishes into this node only within its grant, and a subscribe outside it fails.
#[tokio::test]
#[traced_test]
async fn a_grant_bounds_what_a_peer_publishes() {
    let alice = Node::with_config(MoqConfig {
        admission: Admission::Manual,
        ..Default::default()
    })
    .await;
    let bob = Node::spawn().await;
    let (allowed, other) = (TestBroadcast::start(), TestBroadcast::start());
    let allowed = bob
        .moq
        .publish(bob.path("allowed"), &allowed.producer, Audience::Everyone)
        .expect("publish");
    let other = bob
        .moq
        .publish(bob.path("other"), &other.producer, Audience::Everyone)
        .expect("publish");

    let moq = alice.moq.clone();
    let within: Pattern = allowed.path().as_str().parse().expect("pattern");
    let _accept = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut sessions = Vec::new();
        while let Some(incoming) = moq.accept().await {
            let grant = Grant::publish_under(within.clone());
            sessions.push(incoming.admit(grant).await.expect("admit"));
        }
    }));
    step("bob connects", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    step("alice sees bob", session_with(&alice, bob.id())).await;

    let subscription = step(
        "within the grant",
        alice.moq.subscribe(allowed.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.as_moq()).await;
    let err = step(
        "outside the grant",
        alice.moq.subscribe(other.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect_err("resolved a path outside bob's grant");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");
    let session = step("alice sees bob", session_with(&alice, bob.id())).await;
    let err = step("outside the grant", session.subscribe(other.path()))
        .await
        .expect_err("resolved a path outside bob's grant");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");
    stays_pending(
        "bob's broadcast outside his grant reached alice's table",
        QUIET,
        announced(&mut alice.moq.origin().announced(), other.path().as_str()),
    )
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// The route table carries the node's public publications and no other.
#[tokio::test]
#[traced_test]
async fn the_route_table_carries_public_publications_only() {
    let alice = Node::spawn().await;
    let origin = alice.moq.origin();
    let (public, secret) = (TestBroadcast::start(), TestBroadcast::start());
    let public = alice
        .moq
        .publish(alice.path("public"), &public.producer, Audience::Everyone)
        .expect("publish");
    let secret = alice
        .moq
        .publish(
            alice.path("secret"),
            &secret.producer,
            Audience::Peers(Watchable::new(BTreeSet::new())),
        )
        .expect("publish");

    let served = step(
        "the table serves the public one",
        origin.request_broadcast(public.path()),
    )
    .await
    .expect("resolve");
    read_counter(&served).await;
    let unrouted = step(
        "the table does not serve the private one",
        origin.request_broadcast(secret.path()),
    )
    .await;
    assert!(unrouted.is_err(), "a private publication reached the table");

    alice.shutdown().await;
}

/// Waits until `node` has a session with `remote`, and returns it.
async fn session_with(node: &Node, remote: iroh::EndpointId) -> iroh_moq::Session {
    let mut sessions = node.moq.sessions();
    loop {
        if let Some(session) = sessions
            .get()
            .into_iter()
            .find(|session| session.remote_id() == remote)
        {
            return session;
        }
        sessions.updated().await.expect("node gone");
    }
}
