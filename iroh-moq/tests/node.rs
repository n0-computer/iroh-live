//! Publishing and subscribing on a node, over real QUIC connections: paths
//! that name their publisher, audiences, admission, routes, and the one release
//! of interoperability with nodes on the older path layout.

mod common;

use std::{collections::BTreeSet, time::Duration};

use common::{Node, TIMEOUT, TestBroadcast, ends, read_counter, reading, stays_pending, step};
use iroh::protocol::Router;
use iroh_moq::{
    Admission, Audience, BroadcastTicket, ConnectOptions, Error, Grant, LinkKind, MoqConfig, Reach,
    Reject,
};
use moq_net::{Hop, Pattern, Patterns, origin};
use n0_future::task::AbortOnDropHandle;
use n0_tracing_test::traced_test;
use n0_watcher::{Watchable, Watcher};

type Transport = moq_tokio::transport::Session<web_transport_iroh::Session>;

/// A subscriber reaches a broadcast by its ticket alone: the path names the
/// publisher, so the node knows whom to dial.
#[tokio::test]
#[traced_test]
async fn a_ticket_resolves_by_dialing_its_publisher() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish("cam", &broadcast.producer, Audience::Everyone)
        .expect("publish");
    let ticket = publication.ticket().expect("a live path");
    assert_eq!(ticket, BroadcastTicket::new(alice.id(), "cam"));
    assert_eq!(
        publication.path().as_str(),
        format!("live/{}/cam", alice.id())
    );

    let subscription = step("subscribe", bob.moq.subscribe(ticket.path(), Reach::Direct))
        .await
        .expect("subscribe");
    read_counter(&subscription.as_moq()).await;
    let session = subscription.session().expect("served by a direct session");
    assert_eq!(session.remote_id(), alice.id());

    let routes = bob.moq.routes(ticket.path()).get();
    assert_eq!(routes.len(), 1, "{routes:?}");
    assert_eq!(routes[0].kind, LinkKind::Direct);
    assert_eq!(routes[0].remote, Some(alice.id()));
    assert_eq!(routes[0].hops, 1);
    assert!(routes[0].active, "{routes:?}");

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
        .publish("cam", &broadcast.producer, Audience::Everyone)
        .expect("publish");
    let subscription = step(
        "subscribe",
        alice.moq.subscribe(publication.path(), Reach::Direct),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.as_moq()).await;
    let routes = alice.moq.routes(publication.path()).get();
    assert!(
        routes
            .iter()
            .any(|route| route.kind == LinkKind::Local && route.active),
        "{routes:?}"
    );
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
        .publish("cam", &first.producer, Audience::Everyone)
        .expect("publish");
    let second = TestBroadcast::start();
    let err = alice
        .moq
        .publish("cam", &second.producer, Audience::Everyone)
        .expect_err("a second publication at one path");
    assert!(matches!(err, Error::Duplicate { .. }), "{err:#}");

    // The bare alias counts too: two publications answering `cam` would put
    // two routes there on every direct session.
    let err = alice
        .moq
        .publish_at("cam", &second.producer, Audience::Everyone)
        .expect_err("a publication at another's alias");
    assert!(matches!(err, Error::Duplicate { .. }), "{err:#}");

    // A broadcast that ended frees its path at once, before the node has
    // noticed on its own.
    first.producer.finish();
    alice
        .moq
        .publish("cam", &second.producer, Audience::Everyone)
        .expect("publish over an ended broadcast");

    let err = alice
        .moq
        .publish("", &second.producer, Audience::Everyone)
        .expect_err("an empty name");
    assert!(matches!(err, Error::InvalidPath { .. }), "{err:#}");
    alice.shutdown().await;
}

/// Longer than a direct subscribe waits before it tries the older layout's
/// bare name, so a negative check also covers the alias.
const PAST_THE_GRACE: Duration = Duration::from_secs(3);

/// A publication for a set of peers reaches those peers only, follows the set
/// as it changes, and ends for a peer taken out of it.
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
        .publish("cam", &broadcast.producer, Audience::Peers(members.watch()))
        .expect("publish");

    let for_bob = step("bob", bob.moq.subscribe(publication.path(), Reach::Direct))
        .await
        .expect("bob is a member");
    let mut bob_reading = reading(&for_bob.as_moq()).await;

    // Carol has a session, but nothing is offered on it: neither the path nor
    // its bare alias.
    let session = step("carol connects", carol.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    tokio::join!(
        stays_pending(
            "carol resolved the path",
            PAST_THE_GRACE,
            session.subscribe(publication.path()),
        ),
        stays_pending(
            "carol resolved the bare alias",
            PAST_THE_GRACE,
            session.subscribe("cam"),
        ),
    );
    assert!(carol.moq.routes(publication.path()).get().is_empty());

    // Adding her to the set offers it on her open session.
    members.set(BTreeSet::from([bob.id(), carol.id()])).ok();
    let for_carol = step(
        "carol",
        carol.moq.subscribe(publication.path(), Reach::Direct),
    )
    .await
    .expect("carol after joining the set");
    let mut carol_reading = reading(&for_carol.as_moq()).await;

    // Taking her out again ends what she reads, and only for her.
    members.set(BTreeSet::from([bob.id()])).ok();
    ends("carol after leaving the set", &mut carol_reading).await;
    read_counter(&for_bob.as_moq()).await;

    // A set nobody keeps any more offers to nobody.
    drop(members);
    ends("bob after the set was dropped", &mut bob_reading).await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// A manual publication is offered per session, and withdrawing the offer ends
/// what the peer reads through it.
#[tokio::test]
#[traced_test]
async fn a_manual_audience_needs_an_offer() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish("cam", &broadcast.producer, Audience::Manual)
        .expect("publish");
    let bob_session = step("connect", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let session = step("alice sees bob", session_with(&alice, bob.id())).await;
    stays_pending(
        "bob resolved it before the offer",
        PAST_THE_GRACE,
        bob_session.subscribe(publication.path()),
    )
    .await;

    let offer = session.offer(&publication).expect("offer");
    let subscription = step(
        "subscribe",
        bob.moq.subscribe(publication.path(), Reach::Direct),
    )
    .await
    .expect("subscribe after the offer");
    let mut bob_reading = reading(&subscription.as_moq()).await;
    let mut routes = bob.moq.routes(publication.path());

    drop(offer);
    ends("bob after the offer was withdrawn", &mut bob_reading).await;
    step("the route is withdrawn", async {
        while !routes.get().is_empty() {
            routes.updated().await.expect("node gone");
        }
    })
    .await;
    stays_pending(
        "bob resolved it again after the withdrawal",
        Duration::from_secs(1),
        bob_session.subscribe(publication.path()),
    )
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Unpublishing ends what peers read, and says so on the publication.
#[tokio::test]
#[traced_test]
async fn unpublishing_ends_what_peers_read() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let broadcast = TestBroadcast::start();
    let publication = alice
        .moq
        .publish("cam", &broadcast.producer, Audience::Everyone)
        .expect("publish");
    let subscription = step(
        "subscribe",
        bob.moq.subscribe(publication.path(), Reach::Direct),
    )
    .await
    .expect("subscribe");
    let mut bob_reading = reading(&subscription.as_moq()).await;
    assert!(!publication.is_withdrawn());

    publication.unpublish();
    step("withdrawn", publication.withdrawn()).await;
    ends("bob after the unpublish", &mut bob_reading).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Under manual admission the application decides per session, here on a token
/// in the setup path, and the grant bounds what can be offered.
#[tokio::test]
#[traced_test]
async fn manual_admission_checks_a_token_and_bounds_offers() {
    let alice = Node::with_config(MoqConfig::default().with_admission(Admission::Manual)).await;
    let bob = Node::spawn().await;
    let mallory = Node::spawn().await;

    let public = TestBroadcast::start();
    let public = (
        alice
            .moq
            .publish("public", &public.producer, Audience::Manual)
            .expect("publish"),
        public,
    );
    let secret = TestBroadcast::start();
    let secret = (
        alice
            .moq
            .publish("secret", &secret.producer, Audience::Manual)
            .expect("publish"),
        secret,
    );

    let alice_id = alice.id();
    let moq = alice.moq.clone();
    let public_publication = public.0.clone();
    let accept_loop = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut offers = Vec::new();
        while let Some(incoming) = moq.accept().await {
            if incoming.request().query("jwt") != Some("letmein") {
                incoming.reject(Reject::Unauthorized);
                continue;
            }
            let pattern: Pattern = format!("live/{alice_id}/public").parse().expect("pattern");
            let grant = Grant::new(Patterns::from(pattern), Patterns::new());
            let session = incoming.admit(grant).await.expect("admit");
            offers.push(
                session
                    .offer(&public_publication)
                    .expect("within the grant"),
            );
        }
    }));

    let session = step(
        "bob connects with the token",
        bob.moq.connect_with(
            alice.endpoint.addr(),
            ConnectOptions::default().with_token("letmein"),
        ),
    )
    .await
    .expect("admitted");
    let subscription = step(
        "bob subscribes",
        bob.moq.subscribe(public.0.path(), Reach::Direct),
    )
    .await
    .expect("the offered publication");
    read_counter(&subscription.as_moq()).await;
    assert_eq!(session.remote_id(), alice.id());

    // The grant does not cover the secret one, so it cannot be offered.
    let admitted = alice.moq.sessions().get();
    let bob_session = admitted
        .iter()
        .find(|session| session.remote_id() == bob.id())
        .expect("bob's session");
    let err = bob_session
        .offer(&secret.0)
        .expect_err("an offer outside the grant");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");

    // Without the token the session is refused. From moq-lite-05 on the dialer
    // completes its half of the handshake before the other side decides, so the
    // refusal arrives as the session closing with the reason.
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
        mallory.moq.subscribe(public.0.path(), Reach::Direct),
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
/// A ticket resolves to the broadcast of the publisher it names, never to one a
/// third peer announces under that name, and the bare names a peer announces
/// stay on its own session.
#[tokio::test]
#[traced_test]
async fn a_peer_cannot_route_another_publishers_path() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;
    let mallory = Node::spawn().await;

    let real = TestBroadcast::start();
    let publication = alice
        .moq
        .publish("cam", &real.producer, Audience::Everyone)
        .expect("publish");
    let ticket = publication.ticket().expect("a live path");

    let forged = TestBroadcast::starting_at(FORGED);
    let _forged = mallory
        .moq
        .publish_at(ticket.path(), &forged.producer, Audience::Everyone)
        .expect("publish at alice's path");
    let own = mallory
        .moq
        .publish("cam", &forged.producer, Audience::Everyone)
        .expect("publish");
    step("mallory connects", mallory.moq.connect(bob.endpoint.addr()))
        .await
        .expect("connect");
    let session = step("bob sees mallory", session_with(&bob, mallory.id())).await;

    // Both announcements reach bob: the forged one on mallory's session, and
    // her own path into the table.
    step("the forged path arrives", session.subscribe(ticket.path()))
        .await
        .expect("the forged path on mallory's session");
    let mut own_routes = bob.moq.routes(own.path());
    step("bob routes mallory's own path", async {
        while own_routes.get().is_empty() {
            own_routes.updated().await.expect("node gone");
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        bob.moq.routes(ticket.path()).get().is_empty(),
        "mallory routed alice's path"
    );
    let err = step("the bare name", bob.moq.subscribe("cam", Reach::Direct))
        .await
        .expect_err("a bare name in the table");
    assert!(matches!(err, Error::NoRoute { .. }), "{err:#}");

    let subscription = step("subscribe", bob.moq.subscribe(ticket.path(), Reach::Direct))
        .await
        .expect("subscribe");
    assert_eq!(
        subscription.session().map(|session| session.remote_id()),
        Some(alice.id()),
        "served by someone other than the publisher the ticket names"
    );
    assert!(read_counter(&subscription.as_moq()).await < FORGED);

    alice.shutdown().await;
    bob.shutdown().await;
    mallory.shutdown().await;
}

/// A subscriber that starts before its publisher resolves the named path.
///
/// Waiting past the grace for the older layout must not pin it to the bare
/// alias a current publisher announces next to the named path.
#[tokio::test]
#[traced_test]
async fn a_subscriber_started_first_gets_the_named_path() {
    let alice = Node::spawn().await;
    let bob = Node::spawn().await;

    let ticket = BroadcastTicket::new(alice.id(), "cam");
    let subscribing = tokio::spawn({
        let moq = bob.moq.clone();
        let path = ticket.path();
        async move { moq.subscribe(path, Reach::Direct).await }
    });
    // Past the grace, so the bare name is being looked for when both arrive.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let broadcast = TestBroadcast::start();
    let _publication = alice
        .moq
        .publish("cam", &broadcast.producer, Audience::Everyone)
        .expect("publish");

    let subscription = step("subscribe", subscribing)
        .await
        .expect("task")
        .expect("subscribe");
    assert_eq!(subscription.path().as_str(), ticket.path().as_str());
    read_counter(&subscription.as_moq()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// Dials `to` from `dialer` and waits for the session to be refused.
///
/// From moq-lite-05 on the dialer completes its half of the handshake before
/// the other side decides, so a refusal usually arrives as the session closing.
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
    let alice = Node::with_config(MoqConfig::default().with_admission(Admission::Manual)).await;
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

/// Shutting down refuses the sessions still waiting for admission, so their
/// peers learn at once rather than when the connection idles out.
#[tokio::test]
#[traced_test]
async fn shutdown_refuses_the_sessions_waiting_for_admission() {
    let alice = Node::with_config(MoqConfig::default().with_admission(Admission::Manual)).await;
    let bob = Node::spawn().await;
    let session = step("bob connects", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("the dialer's half completes before the decision");
    step("bob is queued at alice, with nobody accepting", async {
        while alice.moq.waiting_for_admission() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    step("alice shuts down", alice.moq.shutdown()).await;
    tokio::time::timeout(Duration::from_secs(5), session.closed())
        .await
        .expect("a queued session outlived the shutdown");
    alice.shutdown().await;
    bob.shutdown().await;
}

/// An `accept` call that holds the admission queue in a future nobody polls
/// does not hold up the shutdown, and refuses the queue once it is polled.
#[tokio::test]
#[traced_test]
async fn a_parked_accept_does_not_hold_up_shutdown() {
    let alice = Node::with_config(MoqConfig::default().with_admission(Admission::Manual)).await;
    let bob = Node::spawn().await;
    let mut accepting = Box::pin(alice.moq.accept());
    assert!(
        futures_lite::future::poll_once(&mut accepting)
            .await
            .is_none(),
        "nothing to accept yet"
    );
    let session = step("bob connects", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("the dialer's half completes before the decision");
    step("bob is queued", async {
        while alice.moq.waiting_for_admission() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    step("the shutdown ends", alice.moq.shutdown()).await;
    assert!(
        step("the parked accept returns", accepting).await.is_none(),
        "accept handed out a session after the shutdown"
    );
    step("bob is refused", session.closed()).await;
    alice.shutdown().await;
    bob.shutdown().await;
}

/// Shutting the router down shuts the node down with it, as for any iroh
/// protocol.
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
        .publish("cam", &broadcast.producer, Audience::Everyone)
        .expect_err("publish after the router shut down");
    assert!(matches!(err, Error::ShutDown { .. }), "{err:#}");
    alice.shutdown().await;
}

/// A peer publishes into this node only within its grant.
#[tokio::test]
#[traced_test]
async fn a_grant_bounds_what_a_peer_publishes() {
    let alice = Node::with_config(MoqConfig::default().with_admission(Admission::Manual)).await;
    let bob = Node::spawn().await;
    let (allowed, other) = (TestBroadcast::start(), TestBroadcast::start());
    let allowed = bob
        .moq
        .publish("allowed", &allowed.producer, Audience::Everyone)
        .expect("publish");
    let other = bob
        .moq
        .publish("other", &other.producer, Audience::Everyone)
        .expect("publish");

    let moq = alice.moq.clone();
    let within: Pattern = allowed.path().as_str().parse().expect("pattern");
    let _accept = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut sessions = Vec::new();
        while let Some(incoming) = moq.accept().await {
            let grant = Grant::new(
                Patterns::from(Pattern::all()),
                Patterns::from(within.clone()),
            );
            sessions.push(incoming.admit(grant).await.expect("admit"));
        }
    }));
    step("bob connects", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    step("alice sees bob", session_with(&alice, bob.id())).await;

    let subscription = step(
        "within the grant",
        alice.moq.subscribe(allowed.path(), Reach::Direct),
    )
    .await
    .expect("subscribe");
    read_counter(&subscription.as_moq()).await;
    stays_pending(
        "alice took a broadcast outside bob's grant",
        PAST_THE_GRACE,
        alice.moq.subscribe(other.path(), Reach::Direct),
    )
    .await;

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A node can share its route table with another server: its public
/// publications are there, and nothing else of its own.
#[tokio::test]
#[traced_test]
async fn a_shared_route_table_carries_public_publications() {
    let (origin, driver) =
        origin::Producer::new(origin::Config::new(Hop::new(42).expect("a valid hop")));
    let _driver = AbortOnDropHandle::new(tokio::spawn(async move {
        moq_net::time::run(driver).await;
    }));
    let alice = Node::with_config(MoqConfig::default().with_origin(origin.clone())).await;
    let (public, secret) = (TestBroadcast::start(), TestBroadcast::start());
    let public = alice
        .moq
        .publish("public", &public.producer, Audience::Everyone)
        .expect("publish");
    let secret = alice
        .moq
        .publish("secret", &secret.producer, Audience::Manual)
        .expect("publish");

    let served = step(
        "the shared table serves the public one",
        origin.consume().request_broadcast(public.path()),
    )
    .await
    .expect("resolve");
    read_counter(&served).await;
    let unrouted = step(
        "the shared table does not serve the manual one",
        origin.consume().request_broadcast(secret.path()),
    )
    .await;
    assert!(
        unrouted.is_err(),
        "a manual publication reached the shared table"
    );

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

/// A node on the older layout publishes bare names, and a current node reaches
/// them through the ticket: the direct subscribe tries the bare name once the
/// publisher-named path has had its chance.
#[tokio::test]
#[traced_test]
async fn a_publisher_on_the_older_layout_is_reached_by_its_bare_name() {
    let old = LegacyServer::spawn().await;
    let broadcast = TestBroadcast::start();
    let (origin, _origin_task) = legacy_origin();
    let _route = serve(&origin, "cam", &broadcast);
    let _accept = old.serve(origin.consume());

    let bob = Node::spawn().await;
    let ticket = BroadcastTicket::new(old.endpoint.id(), "cam");
    let subscription = step("subscribe", bob.moq.subscribe(ticket.path(), Reach::Direct))
        .await
        .expect("subscribe through the bare name");
    assert_eq!(subscription.path().as_str(), "cam");
    read_counter(&subscription.as_moq()).await;
    bob.shutdown().await;
}

/// A node on the older layout asks for bare names, and a current publisher
/// answers them on direct sessions.
#[tokio::test]
#[traced_test]
async fn a_subscriber_on_the_older_layout_finds_the_bare_name() {
    let alice = Node::spawn().await;
    let broadcast = TestBroadcast::start();
    alice
        .moq
        .publish("cam", &broadcast.producer, Audience::Everyone)
        .expect("publish");

    let old = common::endpoint().await;
    let (ingest, _ingest_task) = legacy_origin();
    let transport = step("dial", iroh_moq::dial(&old, alice.endpoint.addr()))
        .await
        .expect("dial");
    let (session, driver) = step(
        "handshake",
        moq_net::Client::new()
            .with_subscriber(ingest.clone())
            .connect(now(), Transport::new(transport)),
    )
    .await
    .expect("handshake");
    let _driver = AbortOnDropHandle::new(tokio::spawn(async move {
        moq_net::time::run(driver).await;
    }));
    let consumer = step(
        "resolve the bare name",
        ingest.consume().routed_broadcast("cam"),
    )
    .await
    .expect("resolve");
    read_counter(&consumer).await;
    drop(session);
    alice.shutdown().await;
}

fn now() -> std::time::Instant {
    tokio::time::Instant::now().into_std()
}

/// An origin as a node on the older layout ran it: a random hop, no paths
/// naming anyone.
fn legacy_origin() -> (origin::Producer, AbortOnDropHandle<()>) {
    let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::random()));
    let task = tokio::spawn(async move {
        moq_net::time::run(driver).await;
    });
    (producer, AbortOnDropHandle::new(task))
}

/// Publishes `broadcast` at `path` on `origin` as the older layout did.
fn serve(
    origin: &origin::Producer,
    path: &str,
    broadcast: &TestBroadcast,
) -> AbortOnDropHandle<()> {
    let dynamic = origin
        .dynamic(path, origin::Route::default())
        .expect("route");
    let consumer = broadcast.producer.consume();
    AbortOnDropHandle::new(tokio::spawn(async move {
        while let Ok(request) = dynamic.requested_broadcast().await {
            request.accept(&consumer);
        }
    }))
}

/// A node on the older layout: it accepts MoQ and publishes one origin to
/// every session, the way `Moq` did before paths named their publisher.
struct LegacyServer {
    endpoint: iroh::Endpoint,
    incoming:
        std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<iroh::endpoint::Connection>>>,
    _router: Router,
}

#[derive(Debug, Clone)]
struct Forward(tokio::sync::mpsc::Sender<iroh::endpoint::Connection>);

impl iroh::protocol::ProtocolHandler for Forward {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        self.0.send(connection).await.ok();
        Ok(())
    }
}

impl LegacyServer {
    async fn spawn() -> Self {
        let endpoint = common::endpoint().await;
        let (tx, incoming) = tokio::sync::mpsc::channel(4);
        let mut router = Router::builder(endpoint.clone());
        for alpn in iroh_moq::alpns() {
            router = router.accept(alpn, Forward(tx.clone()));
        }
        Self {
            endpoint,
            incoming: std::sync::Arc::new(tokio::sync::Mutex::new(incoming)),
            _router: router.spawn(),
        }
    }

    /// Serves `publisher` to the next session, in the background.
    fn serve(&self, publisher: origin::Consumer) -> AbortOnDropHandle<()> {
        let incoming = self.incoming.clone();
        AbortOnDropHandle::new(tokio::spawn(async move {
            let connection = {
                let mut incoming = incoming.lock().await;
                match tokio::time::timeout(TIMEOUT, incoming.recv()).await {
                    Ok(Some(connection)) => connection,
                    _ => return,
                }
            };
            let transport = if connection.alpn() == web_transport_iroh::ALPN_H3.as_bytes() {
                let request = web_transport_iroh::H3Request::accept(connection)
                    .await
                    .expect("H3 CONNECT");
                let mut response = web_transport_proto::ConnectResponse::OK;
                if let Some(protocol) = request.protocols.first() {
                    response = response.with_protocol(protocol);
                }
                request.respond(response).await.expect("H3 response")
            } else {
                web_transport_iroh::Session::raw(connection)
            };
            let (_session, driver) = moq_net::Server::new()
                .with_publisher(publisher)
                .accept(now(), Transport::new(transport))
                .await
                .expect("handshake");
            moq_net::time::run(driver).await;
        }))
    }
}
