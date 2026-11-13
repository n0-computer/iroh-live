//! Publishing and subscribing on a node, over real QUIC connections.

mod common;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use common::{
    Peer, TIMEOUT, announced, ends, has_session_with, publish, publish_from, read_counter, reading,
    retracted, session_with, stays_pending, step,
};
use iroh::endpoint::PathId;
use iroh_live_node::{
    Audience, Congestion, ConnectOptions, Direction, Error, Grant, LinkKind, NodeConfig, Reach,
    Session,
    policy::{self, Event, PeerRequest, Queue, Rejection},
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

    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let subscription = step(
        "subscribe",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.broadcast()).await;
    let session = subscription.session().expect("served by a direct session");
    assert_eq!(session.peer(), alice.id());
    let link = subscription.serving_link().expect("a serving link");
    assert_eq!(link.kind, LinkKind::Direct);

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Returns whether the first path of `session`'s connection runs the controller `C`.
fn runs<C: 'static>(session: &Session) -> bool {
    session
        .connection()
        .congestion_state(PathId::ZERO)
        .expect("the first path is open")
        .into_any()
        .downcast::<C>()
        .is_ok()
}

/// A dial with its own congestion controller gets it, and the endpoint's other dials keep theirs.
#[tokio::test]
#[traced_test]
async fn a_dial_can_choose_its_congestion_controller() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;
    let options = ConnectOptions {
        quic: iroh::endpoint::ConnectOptions::new()
            .with_transport_config(Congestion::NewReno.transport_config()),
        ..Default::default()
    };
    let chosen = step(
        "bob dials with NewReno",
        bob.node.connect_with(alice.endpoint.addr(), options),
    )
    .await
    .expect("connected");
    let default = step("carol dials", carol.node.connect(alice.endpoint.addr()))
        .await
        .expect("connected");
    assert!(runs::<noq_proto::congestion::NewReno>(&chosen));
    // The test endpoints bind without `MoqPreset`, so iroh's CUBIC.
    assert!(runs::<noq_proto::congestion::Cubic>(&default));

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// The node's own public publication resolves locally, without a link.
#[tokio::test]
#[traced_test]
async fn an_own_publication_resolves_locally() {
    let alice = Peer::spawn().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let subscription = step(
        "subscribe",
        alice
            .node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.broadcast()).await;
    assert!(subscription.serving_link().is_none(), "served over a link");
    assert!(alice.node.sessions().get().is_empty(), "dialed itself");
    alice.shutdown().await;
}

/// A subscription follows a new instance of its path without anyone awaiting `closed`.
#[tokio::test]
#[traced_test]
async fn a_subscription_follows_a_new_instance_on_its_own() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let first = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let subscription = step(
        "subscribe",
        bob.node.subscribe(first.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    assert!(read_counter(&subscription.broadcast()).await < 1000);

    // The publisher replaces its broadcast with a new instance at the same path.
    first.broadcast().close();
    drop(first);
    let _second = publish_from(&alice.node, alice.path("cam"), Audience::Granted, 1000)
        .expect("publish again");

    step("the subscription moves to the new instance", async {
        loop {
            let broadcast = subscription.broadcast();
            if !broadcast.is_closed()
                && let Ok(Some(counter)) =
                    tokio::time::timeout(Duration::from_millis(500), try_counter(&broadcast)).await
                && counter >= 1000
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Reads one counter from the test track of `broadcast`, or `None` if the broadcast fails.
async fn try_counter(broadcast: &moq_net::broadcast::Consumer) -> Option<u64> {
    let mut subscriber = broadcast
        .track("video")
        .ok()?
        .subscribe(common::subscription())
        .await
        .ok()?;
    loop {
        let mut group = subscriber.recv_group().await.ok()??;
        if let Some(frame) = group.read_frame().await.ok()? {
            let bytes: [u8; 8] = frame.payload[..].try_into().ok()?;
            return Some(u64::from_be_bytes(bytes));
        }
    }
}

/// One path holds one publication, and a broadcast that ends frees its path.
#[tokio::test]
#[traced_test]
async fn a_path_holds_one_publication_until_its_broadcast_ends() {
    let alice = Peer::spawn().await;
    let first = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let err = publish(&alice.node, alice.path("cam"), Audience::Granted)
        .expect_err("a second publication at one path");
    assert!(matches!(err, Error::Duplicate { .. }), "{err:#}");

    // A broadcast that ended frees its path at once, before the node has
    // noticed on its own.
    first.broadcast().close();
    let _second = publish(&alice.node, alice.path("cam"), Audience::Granted)
        .expect("publish over an ended broadcast");

    let err = publish(&alice.node, "", Audience::Granted).expect_err("an empty path");
    assert!(matches!(err, Error::InvalidPath { .. }), "{err:#}");
    alice.shutdown().await;
}

/// A fill runs outside the node's lock.
///
/// It may call back into the node, and a failed or refused one leaves nothing
/// behind.
#[tokio::test]
#[traced_test]
async fn a_fill_runs_outside_the_nodes_lock() {
    let alice = Peer::spawn().await;
    let moq = alice.node.clone();

    // A fill that calls back into the node does not deadlock.
    let (first, inner) = alice
        .node
        .publish_with(alice.path("outer"), Audience::Granted, |_| {
            moq.publish(alice.path("inner"), Audience::Granted)
        })
        .expect("publish");
    assert_eq!(inner.path().as_str(), alice.path("inner"));

    // A failed fill closes its broadcast, and the path stays free.
    let mut filled = None;
    let err = alice
        .node
        .publish_with(alice.path("failed"), Audience::Granted, |producer| {
            filled = Some(producer.consume());
            Err::<(), _>(std::io::Error::other("no catalog"))
        })
        .expect_err("the fill failed");
    assert!(matches!(err, Error::Fill { .. }), "{err:#}");
    assert!(filled.expect("the fill ran").is_closed());
    publish(&alice.node, alice.path("failed"), Audience::Granted).expect("the path is free");

    // A fill for a taken path is closed when the clash is found.
    let mut filled = None;
    let err = alice
        .node
        .publish_with(first.path().as_str(), Audience::Granted, |producer| {
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
        &alice.node,
        alice.path("cam"),
        Audience::Peers(members.clone()),
    )
    .expect("publish");

    let for_bob = step(
        "bob",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("bob is a member");
    reading(&for_bob.broadcast()).await;

    // Carol has a session, but nothing is offered on it.
    let session = step("carol connects", carol.node.connect(alice.endpoint.addr()))
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
            .node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("carol after joining the set");
    reading(&for_carol.broadcast()).await;

    // Taking her out again withdraws the path from her, and only from her.
    let path = publication.path().as_str();
    let mut carol_updates = carol.node.origin().announced();
    announced(&mut carol_updates, path).await;
    members.set(BTreeSet::from([bob.id()])).ok();
    retracted(&mut carol_updates, path).await;
    stays_pending(
        "carol resolved the path after leaving the set",
        QUIET,
        session.subscribe(publication.path()),
    )
    .await;
    read_counter(&for_bob.broadcast()).await;

    // An empty set offers to nobody.
    let mut bob_updates = bob.node.origin().announced();
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
        publish(&alice.node, &path, Audience::Peers(members.clone())).expect("publish");
    let _session = step("bob connects", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    announced(&mut bob.node.origin().announced(), &path).await;
    let mut updates = bob.node.origin().announced();
    announced(&mut updates, &path).await;

    step("carol connects", carol.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    stays_pending(
        "carol saw an offer to bob",
        QUIET,
        announced(&mut carol.node.origin().announced(), &path),
    )
    .await;

    members.set(BTreeSet::new()).ok();
    retracted(&mut updates, &path).await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// A value published with `publish_value` reaches a late reader of `value_track`, then each update.
#[tokio::test]
#[traced_test]
async fn a_value_track_gives_a_late_reader_the_newest_value() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let (_publication, mut writer) = alice
        .node
        .publish_value::<String>(alice.path("status"), Audience::Granted, "status")
        .expect("publish");
    for status in ["starting", "ready"] {
        writer.update(&status.to_owned()).expect("write");
    }

    let subscription = step(
        "subscribe",
        bob.node
            .subscribe(alice.path("status"), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    let mut reader = subscription
        .value_track::<String>("status")
        .expect("the track");
    let mut read = async || {
        step("a value", reader.latest())
            .await
            .expect("a readable track")
            .expect("the track is open")
            .value
    };
    assert_eq!(read().await, "ready", "the newest value, not an older one");
    writer.update(&"busy".to_owned()).expect("write");
    assert_eq!(read().await, "busy");

    // The publisher answers for a remote track, so a missing one fails on read.
    let mut missing = subscription
        .value_track::<String>("missing")
        .expect("a lazy track");
    let read = step("the missing track's answer", missing.latest()).await;
    assert!(read.is_err(), "a track nobody publishes read {read:?}");

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Unpublishing takes the path away from peers, and says so on the publication.
#[tokio::test]
#[traced_test]
async fn unpublishing_withdraws_the_path() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let subscription = step(
        "subscribe",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    reading(&subscription.broadcast()).await;
    let mut updates = bob.node.origin().announced();
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
    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let subscription = step(
        "subscribe",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    let mut bob_reading = reading(&subscription.broadcast()).await;

    let session = step("alice sees bob", session_with(&alice.node, bob.id())).await;
    session.close(moq_net::Error::Cancel);
    ends("bob after alice closed the session", &mut bob_reading).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Returns a node configuration whose policy is `queue`.
fn queued(queue: &Queue) -> NodeConfig {
    NodeConfig::default().with_policy(queue.clone())
}

/// Answers `queue` as an application does: a peer that presents the token reads `public`.
///
/// `public` is the path of a publication of `alice`. A peer without the token
/// is rejected.
fn answer_with_token(queue: &Queue, alice: iroh::EndpointId) -> AbortOnDropHandle<()> {
    let answering = queue.clone();
    AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(pending) = answering.next().await {
            assert_eq!(pending.request().event(), Event::Connect);
            if pending.request().token() != Some("letmein") {
                pending.reject(Rejection::new("no token"));
                continue;
            }
            let pattern: Pattern = format!("live/{alice}/public").parse().expect("pattern");
            pending.admit(Grant {
                subscribe: Patterns::from(pattern),
                publish: Patterns::new(),
            });
        }
    }))
}

/// A queue admits a peer with the token, and the grant it answers bounds what the peer sees.
#[tokio::test]
#[traced_test]
async fn a_queue_admits_a_token_within_the_grant_it_answers() {
    let queue = Queue::new(16);
    let alice = Peer::with_config(queued(&queue)).await;
    let bob = Peer::spawn().await;

    let public = publish(&alice.node, alice.path("public"), Audience::Granted).expect("publish");
    let secret = publish(&alice.node, alice.path("secret"), Audience::Granted).expect("publish");
    let _answers = answer_with_token(&queue, alice.id());

    let session = step(
        "bob connects with the token",
        bob.node.connect_with(
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
        bob.node.subscribe(public.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("the offered publication");
    read_counter(&subscription.broadcast()).await;
    assert_eq!(session.peer(), alice.id());

    // The grant does not cover the secret one, so it is not offered.
    stays_pending(
        "bob resolved the secret one",
        QUIET,
        session.subscribe(secret.path()),
    )
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A queue whose application wants a token refuses a peer that presents none.
#[tokio::test]
#[traced_test]
async fn a_queue_refuses_a_peer_without_a_token() {
    let queue = Queue::new(16);
    let alice = Peer::with_config(queued(&queue)).await;
    let mallory = Peer::spawn().await;

    let public = publish(&alice.node, alice.path("public"), Audience::Granted).expect("publish");
    let _answers = answer_with_token(&queue, alice.id());

    // The dialer's half of the handshake completes first, so the refusal may
    // arrive as the session closing.
    match step(
        "mallory connects",
        mallory.node.connect(alice.endpoint.addr()),
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
        !has_session_with(&alice.node, mallory.id()),
        "mallory was admitted"
    );
    let err = step(
        "mallory subscribes",
        mallory
            .node
            .subscribe(public.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect_err("mallory resolved a broadcast without a token");
    assert!(
        matches!(
            err,
            Error::NotAnnounced { .. } | Error::Refused { .. } | Error::Transport { .. }
        ),
        "{err:#}"
    );

    alice.shutdown().await;
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

    let publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");
    let _forged = publish_from(&mallory.node, publication.path(), Audience::Granted, FORGED)
        .expect("publish at alice's path");
    let own = publish_from(
        &mallory.node,
        mallory.path("cam"),
        Audience::Granted,
        FORGED,
    )
    .expect("publish");
    step(
        "mallory connects",
        mallory.node.connect(bob.endpoint.addr()),
    )
    .await
    .expect("connect");
    let _session = step("bob sees mallory", session_with(&bob.node, mallory.id())).await;

    // Her own path reaches bob's table, and the forged one does not even
    // reach her session: the grant refuses it.
    let mut table = bob.node.origin().announced();
    announced(&mut table, own.path().as_str()).await;
    let mut from_mallory = bob.node.origin().announced();
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
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    // The serving link is recorded when the first track is requested, which
    // moq-net's origin only sees once a reader asks for one.
    assert!(read_counter(&subscription.broadcast()).await < FORGED);
    assert_eq!(
        subscription.session().map(|session| session.peer()),
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
        let moq = bob.node.clone();
        let (path, publisher) = (path.clone(), alice.id());
        async move { moq.subscribe(path, Reach::Direct(publisher)).await }
    });
    // Publish only once Bob's subscribe has dialed Alice and waits for the path.
    let mut sessions = bob.node.sessions();
    step("bob dials alice", async {
        while !sessions
            .get()
            .iter()
            .any(|session| session.peer() == alice.id())
        {
            sessions.updated().await.expect("node gone");
        }
    })
    .await;
    assert!(
        !subscribing.is_finished(),
        "the subscribe returned before the publish"
    );
    let _publication = publish(&alice.node, alice.path("cam"), Audience::Granted).expect("publish");

    let subscription = step("subscribe", subscribing)
        .await
        .expect("task")
        .expect("subscribe");
    assert_eq!(subscription.path().as_str(), path);
    read_counter(&subscription.broadcast()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Dials `to` from `dialer` and waits for the session to be refused.
async fn refused(dialer: &Peer, to: &Peer, within: Duration) {
    match step("connect", dialer.node.connect(to.endpoint.addr())).await {
        Err(_) => {}
        Ok(session) => {
            tokio::time::timeout(within, session.closed())
                .await
                .expect("the refused session stayed open");
        }
    }
    assert!(
        !to.node
            .sessions()
            .get()
            .iter()
            .any(|session| session.peer() == dialer.id()),
        "the session was admitted"
    );
}

/// An incoming session the application drops without deciding is refused.
#[tokio::test]
#[traced_test]
async fn an_undecided_session_is_refused() {
    let queue = Queue::new(16);
    let alice = Peer::with_config(queued(&queue)).await;
    let bob = Peer::spawn().await;
    let _accept = AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(pending) = queue.next().await {
            drop(pending);
        }
    }));
    refused(&bob, &alice, TIMEOUT).await;
    alice.shutdown().await;
    bob.shutdown().await;
}

/// A session that waits in an unanswered queue does not hold up the shutdown.
#[tokio::test]
#[traced_test]
async fn a_session_waiting_in_the_queue_does_not_hold_up_shutdown() {
    let queue = Queue::new(16);
    let alice = Peer::with_config(NodeConfig {
        decide_timeout: Duration::from_secs(600),
        ..queued(&queue)
    })
    .await;
    let bob = Peer::spawn().await;
    let dialing = {
        let bob = bob.node.clone();
        let addr = alice.endpoint.addr();
        tokio::spawn(async move { bob.connect(addr).await })
    };
    let pending = step("bob's session waits", queue.next())
        .await
        .expect("a request");
    step("the shutdown ends", alice.node.shutdown()).await;
    // Too late: the node refused the session at shutdown.
    pending.admit(Grant::everything());
    match step("bob's dial ends", dialing).await.expect("task") {
        Err(_) => {}
        Ok(session) => {
            step("the refused session closes", session.closed()).await;
        }
    }
    assert!(
        alice.node.sessions().get().is_empty(),
        "admitted after the shutdown"
    );
    alice.shutdown().await;
    bob.shutdown().await;
}

/// A decision slower than the node's decide timeout refuses the session.
#[tokio::test]
#[traced_test]
async fn a_slow_decision_times_out() {
    let queue = Queue::new(16);
    let alice = Peer::with_config(NodeConfig {
        decide_timeout: Duration::from_millis(200),
        ..queued(&queue)
    })
    .await;
    let bob = Peer::spawn().await;
    // Held and never answered, so only the timeout decides.
    let _held = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(pending) = queue.next().await {
            held.push(pending);
        }
    }));
    match step("bob connects", bob.node.connect(alice.endpoint.addr())).await {
        Err(err) => assert!(matches!(err, Error::Refused { .. }), "{err:#}"),
        Ok(session) => {
            let reason = step("the session closes", session.closed()).await;
            assert!(
                matches!(&reason, Error::SessionClosed { source, .. }
                    if matches!(source, moq_net::Error::Session(moq_net::SessionError::Timeout)
                        | moq_net::Error::Timeout)),
                "not closed for the timeout: {reason:#}"
            );
        }
    }
    alice.shutdown().await;
    bob.shutdown().await;
}

/// Shutting the router down shuts the node down.
#[tokio::test]
#[traced_test]
async fn the_router_shuts_the_node_down() {
    let alice = Peer::spawn().await;
    step("router shutdown", alice.router.shutdown())
        .await
        .expect("router");
    let err = publish(&alice.node, alice.path("cam"), Audience::Granted)
        .expect_err("publish after the router shut down");
    assert!(matches!(err, Error::ShutDown { .. }), "{err:#}");
    alice.shutdown().await;
}

/// A peer publishes into this node only within its grant, and a subscribe outside it fails.
#[tokio::test]
#[traced_test]
async fn a_grant_bounds_what_a_peer_publishes() {
    let queue = Queue::new(16);
    let alice = Peer::with_config(queued(&queue)).await;
    let bob = Peer::spawn().await;
    let allowed = publish(&bob.node, bob.path("allowed"), Audience::Granted).expect("publish");
    let other = publish(&bob.node, bob.path("other"), Audience::Granted).expect("publish");

    let within: Pattern = allowed.path().as_str().parse().expect("pattern");
    let _accept = AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(pending) = queue.next().await {
            pending.admit(Grant::publish_under(within.clone()));
        }
    }));
    step("bob connects", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    step("alice sees bob", session_with(&alice.node, bob.id())).await;

    let subscription = step(
        "within the grant",
        alice
            .node
            .subscribe(allowed.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.broadcast()).await;
    let err = step(
        "outside the grant",
        alice.node.subscribe(other.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect_err("resolved a path outside bob's grant");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");
    let session = step("alice sees bob", session_with(&alice.node, bob.id())).await;
    let err = step("outside the grant", session.subscribe(other.path()))
        .await
        .expect_err("resolved a path outside bob's grant");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");
    stays_pending(
        "bob's broadcast outside his grant reached alice's table",
        QUIET,
        announced(&mut alice.node.origin().announced(), other.path().as_str()),
    )
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A dial's policy grant holds from the session's start, and widens when the policy is asked again.
#[tokio::test]
#[traced_test]
async fn a_dial_grant_holds_from_the_start_and_widens_on_reauthorize() {
    let bob = Peer::spawn().await;
    let wanted = publish(&bob.node, bob.path("wanted"), Audience::Granted).expect("publish");
    let other = publish(&bob.node, bob.path("other"), Audience::Granted).expect("publish");
    let narrow = Grant {
        subscribe: Patterns::new(),
        publish: Pattern::literal(wanted.path().as_str())
            .expect("pattern")
            .into(),
    };
    let current = Arc::new(Mutex::new(narrow.clone()));
    let policy = {
        let current = current.clone();
        policy::from_fn(move |_: &PeerRequest| Ok(current.lock().expect("poisoned").clone().into()))
    };
    let alice = Peer::with_config(NodeConfig::default().with_policy(policy)).await;

    // Both cursors start before the dial, so they see whatever the session
    // lets into the table from its first moment.
    let mut to_wanted = alice.node.origin().announced();
    let mut to_other = alice.node.origin().announced();
    let session = step("alice dials bob", alice.node.connect(bob.endpoint.addr()))
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

    // Alice's policy now grants bob all of `live/<bob>/`, so the session widens to it.
    *current.lock().expect("poisoned") = common::own_paths(bob.id());
    alice.node.reauthorize(bob.id());
    announced(&mut alice.node.origin().announced(), other.path().as_str()).await;

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
    let origin = alice.node.origin();
    let public = publish(&alice.node, alice.path("public"), Audience::Granted).expect("publish");
    let nobody = Audience::peers([]);
    let secret = publish(&alice.node, alice.path("secret"), nobody).expect("publish");

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
        publish(&alice.node, alice.path("cam"), Audience::Peers(members)).expect("publish");
    let public = publish(&alice.node, alice.path("public"), Audience::Granted).expect("publish");
    let path = publication.path().as_str();

    // Alice dials both, so she is the client on each session.
    let to_bob = step("alice dials bob", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("connect");
    let to_carol = step(
        "alice dials carol",
        alice.node.connect(carol.endpoint.addr()),
    )
    .await
    .expect("connect");
    assert_eq!(to_bob.direction(), Direction::Dialed);
    assert_eq!(to_carol.direction(), Direction::Dialed);

    announced(&mut bob.node.origin().announced(), path).await;
    let from_alice = step("bob sees alice", session_with(&bob.node, alice.id())).await;
    let subscription = step("bob subscribes", from_alice.subscribe(publication.path()))
        .await
        .expect("bob is in the set");
    read_counter(&subscription.broadcast()).await;

    // Carol sees the public one, and never the one for bob.
    let mut carol_table = carol.node.origin().announced();
    announced(&mut carol_table, public.path().as_str()).await;
    let from_alice = step("carol sees alice", session_with(&carol.node, alice.id())).await;
    stays_pending(
        "carol resolved a path whose audience leaves her out",
        QUIET,
        from_alice.subscribe(publication.path()),
    )
    .await;
    let refused = carol.node.origin().request(publication.path(), None).await;
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
        &alice.node,
        alice.path("cam"),
        Audience::Peers(members.clone()),
    )
    .expect("publish");
    let path = publication.path().as_str();

    let for_bob = step(
        "bob subscribes",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("bob is in the set");
    let mut bob_reading = reading(&for_bob.broadcast()).await;
    let carol_session = step("carol connects", carol.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let mut bob_table = bob.node.origin().announced();
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
    read_counter(&for_carol.broadcast()).await;

    // The audience can change wholesale, too.
    let mut carol_reading = reading(&for_carol.broadcast()).await;
    publication.set_audience(Audience::peers([]));
    let err = reset(&mut carol_reading).await;
    assert!(unauthorized(&err), "carol's read ended with {err:#}");
    publication.set_audience(Audience::Granted);
    let again = step(
        "bob subscribes once the audience is granted",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("everyone may see it");
    read_counter(&again.broadcast()).await;

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
        .node
        .publish(alice.path("cam"), Audience::Granted)
        .expect("publish");
    let mut track = publication
        .broadcast()
        .create_track("video", moq_net::track::Info::default())
        .expect("create track");
    let subscription = step(
        "bob subscribes",
        bob.node
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    let mut subscriber = step(
        "bob subscribes to the track",
        subscription
            .broadcast()
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
            .write_frame(None, bytes::Bytes::from(vec![n]))
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

/// With overlapping grants, a path only carol publishes is not resolved through bob's session.
#[tokio::test]
#[traced_test]
async fn a_pinned_subscribe_ignores_a_third_peer_with_an_overlapping_grant() {
    let alice =
        Peer::with_config(NodeConfig::default().with_policy(policy::Open(Grant::everything())))
            .await;
    let bob = Peer::spawn().await;
    let carol = Peer::spawn().await;

    let publication = publish(&carol.node, "shared/cam", Audience::Granted).expect("publish");
    step("bob connects", bob.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    step("carol connects", carol.node.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let with_bob = step("alice sees bob", session_with(&alice.node, bob.id())).await;
    let with_carol = step("alice sees carol", session_with(&alice.node, carol.id())).await;

    let subscription = step("carol's session", with_carol.subscribe(publication.path()))
        .await
        .expect("carol's session resolves her own path");
    read_counter(&subscription.broadcast()).await;
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

#[tokio::test]
#[traced_test]
async fn attaching_a_relay_with_an_unusable_url_fails() {
    let alice = Peer::spawn().await;
    for url in ["ftp://relay.example/", "iroh://not-an-endpoint-id/"] {
        let config = iroh_live_node::RelayConfig::new(url.parse().expect("a URL"));
        let err = alice.node.attach_relay(config).expect_err(url);
        assert!(
            matches!(err, Error::InvalidRelayUrl { .. }),
            "{url}: {err:#}"
        );
    }
    alice.shutdown().await;
}
