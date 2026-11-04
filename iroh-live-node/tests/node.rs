//! Publishing and subscribing on a node, over real QUIC connections.

mod common;

use std::{collections::BTreeSet, time::Duration};

use common::{
    Peer, TIMEOUT, announced, ends, publish, publish_from, read_counter, reading, retracted,
    stays_pending, step,
};
use iroh_live_node::{
    Admission, Audience, ConnectOptions, Error, Grant, LinkKind, NodeConfig, Reach,
};
use moq_net::{Pattern, Patterns};
use n0_future::task::AbortOnDropHandle;
use n0_tracing_test::traced_test;
use n0_watcher::{Watchable, Watcher};

/// A subscriber reaches a broadcast by dialing its publisher.
#[tokio::test]
#[traced_test]
async fn a_path_resolves_by_dialing_its_publisher() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
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
    let alice = Peer::spawn().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
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
    let alice = Peer::spawn().await;
    let first = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let err = publish(&alice.moq, alice.path("cam"), Audience::Everyone)
        .expect_err("a second publication at one path");
    assert!(matches!(err, Error::Duplicate { .. }), "{err:#}");

    // A broadcast that ended frees its path at once, before the node has
    // noticed on its own.
    first.broadcast().close();
    let _second = publish(&alice.moq, alice.path("cam"), Audience::Everyone)
        .expect("publish over an ended broadcast");

    let err = publish(&alice.moq, "", Audience::Everyone).expect_err("an empty path");
    assert!(matches!(err, Error::InvalidPath { .. }), "{err:#}");
    alice.shutdown().await;
}

/// A fill runs outside the node's lock: it may call back into the node, and a failed or refused one leaves nothing behind.
#[tokio::test]
#[traced_test]
async fn a_fill_runs_outside_the_nodes_lock() {
    let alice = Peer::spawn().await;
    let moq = alice.moq.clone();

    // A fill that calls back into the node does not deadlock.
    let (first, inner) = alice
        .moq
        .publish_with(alice.path("outer"), Audience::Everyone, |_| {
            moq.publish(alice.path("inner"), Audience::Everyone)
        })
        .expect("publish");
    assert_eq!(inner.path().as_str(), alice.path("inner"));

    // A failed fill closes its broadcast, and the path stays free.
    let mut filled = None;
    let err = alice
        .moq
        .publish_with(alice.path("failed"), Audience::Everyone, |producer| {
            filled = Some(producer.consume());
            Err::<(), _>(std::io::Error::other("no catalog"))
        })
        .expect_err("the fill failed");
    assert!(matches!(err, Error::Fill { .. }), "{err:#}");
    assert!(filled.expect("the fill ran").is_closed());
    publish(&alice.moq, alice.path("failed"), Audience::Everyone).expect("the path is free");

    // A fill for a taken path is closed when the clash is found.
    let mut filled = None;
    let err = alice
        .moq
        .publish_with(first.path().as_str(), Audience::Everyone, |producer| {
            filled = Some(producer.consume());
            Ok::<_, std::convert::Infallible>(())
        })
        .expect_err("a second publication at one path");
    assert!(matches!(err, Error::Duplicate { .. }), "{err:#}");
    assert!(filled.expect("the fill ran").is_closed());
    assert!(
        !first.is_withdrawn(),
        "the clash withdrew the first publication"
    );

    alice.shutdown().await;
}

/// How long a negative check waits for something that must not happen.
const QUIET: Duration = Duration::from_secs(2);

/// A `Peers` publication reaches only its set, and follows the set as it changes.
#[tokio::test]
#[traced_test]
async fn a_peers_audience_follows_its_set() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;

    let members = Watchable::new(BTreeSet::from([bob.id()]));
    let publication = publish(
        &alice.moq,
        alice.path("cam"),
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
    let mut carol_updates = carol.moq.origin().announced();
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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;

    let members = Watchable::new(BTreeSet::from([bob.id()]));
    let path = alice.path("cam");
    let _publication =
        publish(&alice.moq, &path, Audience::Peers(members.clone())).expect("publish");
    let _session = step("bob connects", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    announced(&mut bob.moq.origin().announced(), &path).await;
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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
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
    let alice = Peer::with_config(NodeConfig {
        admission: Admission::Manual,
        ..Default::default()
    })
    .await;
    let bob = Peer::spawn().await;
    let mallory = Peer::spawn().await;

    let public = publish(&alice.moq, alice.path("public"), Audience::Everyone).expect("publish");
    let secret = publish(&alice.moq, alice.path("secret"), Audience::Everyone).expect("publish");

    let alice_id = alice.id();
    let moq = alice.moq.clone();
    let accept_loop = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut sessions = Vec::new();
        while let Some(incoming) = moq.accept().await {
            if incoming.request().token() != Some("letmein") {
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
        bob.moq.subscribe(public.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("the offered publication");
    read_counter(&subscription.as_moq()).await;
    assert_eq!(session.remote_id(), alice.id());

    // The grant does not cover the secret one, so it is not offered.
    stays_pending(
        "bob resolved the secret one",
        QUIET,
        session.subscribe(secret.path()),
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
            .subscribe(public.path(), Reach::Direct(alice.id())),
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
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let mallory = Peer::spawn().await;

    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let _forged = publish_from(&mallory.moq, publication.path(), Audience::Everyone, FORGED)
        .expect("publish at alice's path");
    let own = publish_from(
        &mallory.moq,
        mallory.path("cam"),
        Audience::Everyone,
        FORGED,
    )
    .expect("publish");
    step("mallory connects", mallory.moq.connect(bob.endpoint.addr()))
        .await
        .expect("connect");
    let _session = step("bob sees mallory", session_with(&bob, mallory.id())).await;

    // Her own path reaches bob's table, and the forged one does not even
    // reach her session: the grant refuses it.
    let mut table = bob.moq.origin().announced();
    announced(&mut table, own.path().as_str()).await;
    let mut from_mallory = bob.moq.origin().announced();
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
    // The serving link is recorded when the first track is requested, which
    // moq-net's origin now only sees once a reader asks for one.
    assert!(read_counter(&subscription.as_moq()).await < FORGED);
    assert_eq!(
        subscription.session().map(|session| session.remote_id()),
        Some(alice.id()),
        "served by someone other than the publisher"
    );

    alice.shutdown().await;
    bob.shutdown().await;
    mallory.shutdown().await;
}

/// A subscriber that starts before its publisher resolves the path once published.
#[tokio::test]
#[traced_test]
async fn a_subscriber_started_first_gets_the_named_path() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

    let path = alice.path("cam");
    let subscribing = tokio::spawn({
        let moq = bob.moq.clone();
        let (path, publisher) = (path.clone(), alice.id());
        async move { moq.subscribe(path, Reach::Direct(publisher)).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");

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
async fn refused(dialer: &Peer, to: &Peer, within: Duration) {
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
    let alice = Peer::with_config(NodeConfig {
        admission: Admission::Manual,
        ..Default::default()
    })
    .await;
    let bob = Peer::spawn().await;
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
    let alice = Peer::with_config(NodeConfig {
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
    let alice = Peer::spawn().await;
    step("router shutdown", alice.router.shutdown())
        .await
        .expect("router");
    let err = publish(&alice.moq, alice.path("cam"), Audience::Everyone)
        .expect_err("publish after the router shut down");
    assert!(matches!(err, Error::ShutDown { .. }), "{err:#}");
    alice.shutdown().await;
}

/// A peer publishes into this node only within its grant, and a subscribe outside it fails.
#[tokio::test]
#[traced_test]
async fn a_grant_bounds_what_a_peer_publishes() {
    let alice = Peer::with_config(NodeConfig {
        admission: Admission::Manual,
        ..Default::default()
    })
    .await;
    let bob = Peer::spawn().await;
    let allowed = publish(&bob.moq, bob.path("allowed"), Audience::Everyone).expect("publish");
    let other = publish(&bob.moq, bob.path("other"), Audience::Everyone).expect("publish");

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

/// A dial's grant holds from the session's start, and the session can widen up to the node's grant.
#[tokio::test]
#[traced_test]
async fn a_dial_grant_holds_from_the_start_and_widens_to_the_nodes() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let wanted = publish(&bob.moq, bob.path("wanted"), Audience::Everyone).expect("publish");
    let other = publish(&bob.moq, bob.path("other"), Audience::Everyone).expect("publish");

    // Both cursors start before the dial, so they see whatever the session
    // lets into the table from its first moment.
    let mut to_wanted = alice.moq.origin().announced();
    let mut to_other = alice.moq.origin().announced();
    let narrow = Grant {
        subscribe: Patterns::new(),
        publish: Pattern::literal(wanted.path().as_str())
            .expect("pattern")
            .into(),
    };
    let options = ConnectOptions {
        grant: Some(narrow.clone()),
        ..Default::default()
    };
    let session = step(
        "alice dials bob",
        alice.moq.connect_with(bob.endpoint.addr(), options),
    )
    .await
    .expect("connect");
    assert_eq!(session.grant(), narrow);
    announced(&mut to_wanted, wanted.path().as_str()).await;
    stays_pending(
        "bob's broadcast outside the dial's grant reached alice's table",
        QUIET,
        announced(&mut to_other, other.path().as_str()),
    )
    .await;

    // Alice's node grants bob all of `live/<bob>/`, so the session widens to it.
    session.set_grant(common::own_paths(bob.id()));
    announced(&mut alice.moq.origin().announced(), other.path().as_str()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// The route table serves every publication of the node, whatever its audience.
///
/// A publication is created in the table, so a local reader resolves it with
/// no link involved. Audiences bound peers, not this node, so a private
/// publication resolves locally too.
#[tokio::test]
#[traced_test]
async fn the_route_table_serves_every_publication() {
    let alice = Peer::spawn().await;
    let origin = alice.moq.origin();
    let public = publish(&alice.moq, alice.path("public"), Audience::Everyone).expect("publish");
    let nobody = Audience::Peers(Watchable::new(BTreeSet::new()));
    let secret = publish(&alice.moq, alice.path("secret"), nobody).expect("publish");

    for publication in [&public, &secret] {
        let served = step(
            "the table serves it",
            origin.request(publication.path(), None),
        )
        .await
        .expect("resolve");
        read_counter(&served).await;
    }

    // Unpublishing takes it out of the table.
    secret.unpublish();
    let unrouted = step("an unpublished path", origin.request(secret.path(), None)).await;
    assert!(unrouted.is_err(), "an unpublished path still resolves");

    alice.shutdown().await;
}

/// Reads groups from `subscriber` until its track ends, and returns why.
async fn reset(subscriber: &mut moq_net::track::Subscriber) -> moq_net::Error {
    step("the track ends", async {
        loop {
            match subscriber.recv_group().await {
                Ok(Some(_)) => continue,
                Ok(None) => panic!("the track finished instead of being reset"),
                Err(err) => return err,
            }
        }
    })
    .await
}

/// Reports whether `err` is moq's refusal of a path the grant no longer covers.
fn unauthorized(err: &moq_net::Error) -> bool {
    matches!(
        err,
        moq_net::Error::Unauthorized | moq_net::Error::Stream(moq_net::StreamError::Unauthorized)
    )
}

/// A `Peers` publication reaches its set on sessions this node dialed, too.
///
/// The node authorizes a session before its handshake, on either side, so a
/// dialed peer outside the set never sees the path, and one inside does.
#[tokio::test]
#[traced_test]
async fn a_peers_audience_holds_on_a_dialed_session() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;

    let members = Watchable::new(BTreeSet::from([bob.id()]));
    let publication =
        publish(&alice.moq, alice.path("cam"), Audience::Peers(members)).expect("publish");
    let public = publish(&alice.moq, alice.path("public"), Audience::Everyone).expect("publish");
    let path = publication.path().as_str();

    // Alice dials both, so she is the client on each session.
    let to_bob = step("alice dials bob", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("connect");
    let to_carol = step(
        "alice dials carol",
        alice.moq.connect(carol.endpoint.addr()),
    )
    .await
    .expect("connect");
    assert!(to_bob.dialed() && to_carol.dialed());

    announced(&mut bob.moq.origin().announced(), path).await;
    let from_alice = step("bob sees alice", session_with(&bob, alice.id())).await;
    let subscription = step("bob subscribes", from_alice.subscribe(publication.path()))
        .await
        .expect("bob is in the set");
    read_counter(&subscription.as_moq()).await;

    // Carol sees the public one, and never the one for bob.
    let mut carol_table = carol.moq.origin().announced();
    announced(&mut carol_table, public.path().as_str()).await;
    let from_alice = step("carol sees alice", session_with(&carol, alice.id())).await;
    stays_pending(
        "carol resolved a path whose audience leaves her out",
        QUIET,
        from_alice.subscribe(publication.path()),
    )
    .await;
    let refused = carol.moq.origin().request(publication.path(), None).await;
    assert!(refused.is_err(), "carol's table routes bob's path");

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// Changing a `Peers` set re-authorizes live sessions in both directions.
///
/// A peer taken out loses the path, and what it reads there is reset with
/// `Unauthorized`. A peer put in gets the path on the session it already has.
#[tokio::test]
#[traced_test]
async fn a_changed_audience_retracts_and_grants_live() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;

    let members = Watchable::new(BTreeSet::from([bob.id()]));
    let publication = publish(
        &alice.moq,
        alice.path("cam"),
        Audience::Peers(members.clone()),
    )
    .expect("publish");
    let path = publication.path().as_str();

    let for_bob = step(
        "bob subscribes",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("bob is in the set");
    let mut bob_reading = reading(&for_bob.as_moq()).await;
    let carol_session = step("carol connects", carol.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let mut bob_table = bob.moq.origin().announced();
    announced(&mut bob_table, path).await;

    // Swap bob for carol.
    members.set(BTreeSet::from([carol.id()])).ok();
    let err = reset(&mut bob_reading).await;
    assert!(unauthorized(&err), "bob's read ended with {err:#}");
    retracted(&mut bob_table, path).await;

    let for_carol = step(
        "carol subscribes",
        carol_session.subscribe(publication.path()),
    )
    .await
    .expect("carol was added");
    read_counter(&for_carol.as_moq()).await;

    // The audience can change wholesale, too.
    let mut carol_reading = reading(&for_carol.as_moq()).await;
    publication.set_audience(Audience::Peers(Watchable::new(BTreeSet::new())));
    let err = reset(&mut carol_reading).await;
    assert!(unauthorized(&err), "carol's read ended with {err:#}");
    publication.set_audience(Audience::Everyone);
    let again = step(
        "bob subscribes once everyone may",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("everyone may see it");
    read_counter(&again.as_moq()).await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// A peer reads the publisher's broadcast through its session, with nothing in between.
///
/// The publisher's track sees the peer's subscription as soon as the peer
/// asks, and the peer reads the groups the publisher wrote, sequence for
/// sequence.
#[tokio::test]
#[traced_test]
async fn a_peer_reads_the_publishers_own_broadcast() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;

    let publication = alice
        .moq
        .publish(alice.path("cam"), Audience::Everyone)
        .expect("publish");
    let mut track = publication
        .broadcast()
        .create_track("video", moq_net::track::Info::default())
        .expect("create track");
    let subscription = step(
        "bob subscribes",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    let mut subscriber = step(
        "bob subscribes to the track",
        subscription
            .as_moq()
            .track("video")
            .expect("track")
            .subscribe(common::subscription()),
    )
    .await
    .expect("subscribe to the track");
    step("alice's track sees bob", async {
        while track.subscription().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    for n in 0..3u8 {
        track
            .write_frame(None, moq_net::bytes::Bytes::from(vec![n]))
            .expect("write");
        let mut group = step("a group", subscriber.recv_group())
            .await
            .expect("track failed")
            .expect("track ended");
        let frame = step("a frame", group.read_frame())
            .await
            .expect("group failed")
            .expect("a frame");
        assert_eq!(frame.payload.as_ref(), [n]);
        assert_eq!(group.sequence, u64::from(n), "group sequence");
    }

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A grant that lets every peer publish anywhere.
fn anywhere(_peer: iroh::EndpointId) -> Grant {
    Grant::everything()
}

/// With overlapping grants, a path only carol publishes is not resolved through bob's session.
#[tokio::test]
#[traced_test]
async fn a_pinned_subscribe_ignores_a_third_peer_with_an_overlapping_grant() {
    let alice = Peer::with_config(NodeConfig {
        grant: Some(std::sync::Arc::new(anywhere)),
        ..Default::default()
    })
    .await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;

    let publication = publish(&carol.moq, "shared/cam", Audience::Everyone).expect("publish");
    step("bob connects", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    step("carol connects", carol.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let with_bob = step("alice sees bob", session_with(&alice, bob.id())).await;
    let with_carol = step("alice sees carol", session_with(&alice, carol.id())).await;

    let subscription = step("carol's session", with_carol.subscribe(publication.path()))
        .await
        .expect("carol's session resolves her own path");
    read_counter(&subscription.as_moq()).await;
    stays_pending(
        "bob's session resolved a path only carol publishes",
        QUIET,
        with_bob.subscribe(publication.path()),
    )
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// Waits until `node` has a session with `remote`, and returns it.
async fn session_with(node: &Peer, remote: iroh::EndpointId) -> iroh_live_node::Session {
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
