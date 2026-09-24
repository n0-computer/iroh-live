//! Publishing and subscribing on a node, over real QUIC connections: paths
//! that name their publisher, audiences, admission, routes, and the one release
//! of interoperability with nodes on the older path layout.

mod common;

use std::{collections::BTreeSet, time::Duration};

use common::{Node, TIMEOUT, TestBroadcast, read_counter, step};
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

    first.producer.finish();
    step("the path frees up", async {
        loop {
            if alice
                .moq
                .publish("cam", &second.producer, Audience::Everyone)
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    let err = alice
        .moq
        .publish("", &second.producer, Audience::Everyone)
        .expect_err("an empty name");
    assert!(matches!(err, Error::InvalidPath { .. }), "{err:#}");
    alice.shutdown().await;
}

/// A publication for a set of peers reaches those peers only, and follows the
/// set as it changes.
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
    read_counter(&for_bob.as_moq()).await;

    // Carol connects, but nothing is offered to her.
    let mut carol_subscribe = Box::pin(carol.moq.subscribe(publication.path(), Reach::Direct));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut carol_subscribe)
            .await
            .is_err(),
        "carol resolved a broadcast whose audience does not name her"
    );

    // Adding her to the set offers it on her session, which is already open.
    members.set(BTreeSet::from([bob.id(), carol.id()])).ok();
    let for_carol = step("carol", carol_subscribe)
        .await
        .expect("carol after joining the set");
    read_counter(&for_carol.as_moq()).await;

    alice.shutdown().await;
    bob.shutdown().await;
    carol.shutdown().await;
}

/// A manual publication is offered per session, and withdrawn with the guard.
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
    step("connect", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let session = step("alice sees bob", async {
        let mut sessions = alice.moq.sessions();
        loop {
            if let Some(session) = sessions.get().into_iter().next() {
                return session;
            }
            sessions.updated().await.expect("node gone");
        }
    })
    .await;

    let mut routes = bob.moq.routes(publication.path());
    assert!(routes.get().is_empty(), "offered before the offer");

    let offer = session.offer(&publication).expect("offer");
    let subscription = step(
        "subscribe",
        bob.moq.subscribe(publication.path(), Reach::Direct),
    )
    .await
    .expect("subscribe after the offer");
    read_counter(&subscription.as_moq()).await;

    drop(offer);
    step("the route is withdrawn", async {
        while !routes.get().is_empty() {
            routes.updated().await.expect("node gone");
        }
    })
    .await;

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
        Err(_) => {}
        Ok(session) => {
            let reason = step("the refused session closes", session.closed()).await;
            assert!(
                matches!(&reason, Error::SessionClosed { source, .. }
                    if !matches!(source, moq_net::Error::Cancel)),
                "a session without a token was admitted: {reason:#}"
            );
        }
    }

    drop(accept_loop);
    alice.shutdown().await;
    bob.shutdown().await;
    mallory.shutdown().await;
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
