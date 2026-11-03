//! The moq-net origin behaviour the transport builds on, over real iroh connections.
//!
//! A moq-net release that changes any of it breaks publishing in ways our
//! other tests would not notice.

mod common;

use std::time::Duration;

use common::{TIMEOUT, TestBroadcast, endpoint, read_counter, step, subscription};
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use moq_net::{
    Hop, announce, broadcast,
    origin::{self, Link, Route},
};
use n0_future::task::AbortOnDropHandle;
use tokio::sync::mpsc;

type Transport = iroh_moq::AdaptedTransport<web_transport_iroh::Session>;

/// Creates an origin with hop `hop`, its driver spawned.
fn origin(hop: u64) -> origin::Producer {
    moq_tokio::origin::spawn_config(origin::Config::new(Hop::new(hop).expect("a non-zero hop")))
}

fn now() -> moq_net::time::Instant {
    moq_net::time::Instant::now()
}

/// Hands every accepted connection to the test.
#[derive(Debug, Clone)]
struct Forward(mpsc::Sender<Connection>);

impl ProtocolHandler for Forward {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.0.send(connection).await.ok();
        Ok(())
    }
}

/// A node that lets the test choose each accepted session's origins.
struct Server {
    endpoint: Endpoint,
    incoming: mpsc::Receiver<Connection>,
    _router: Router,
}

impl Server {
    async fn spawn() -> Self {
        let endpoint = endpoint().await;
        let (tx, incoming) = mpsc::channel(4);
        let mut router = Router::builder(endpoint.clone());
        for alpn in iroh_moq::alpns() {
            router = router.accept(alpn, Forward(tx.clone()));
        }
        Self {
            endpoint,
            incoming,
            _router: router.spawn(),
        }
    }

    /// Accepts the next session with `publisher` and `subscriber` as its origins.
    async fn accept(
        &mut self,
        publisher: origin::Consumer,
        subscriber: Option<origin::Producer>,
    ) -> Session {
        let connection = tokio::time::timeout(TIMEOUT, self.incoming.recv())
            .await
            .expect("timed out waiting for a connection")
            .expect("router gone");
        let (transport, target) = iroh_moq::transport::accept(connection)
            .await
            .expect("transport handshake");
        let address = match target {
            Some(_) => moq_net::transport::Address::Uri,
            None => moq_net::transport::Address::Setup,
        };
        let mut server = moq_net::Server::new().with_publisher(publisher);
        if let Some(subscriber) = subscriber {
            server = server.with_subscriber(subscriber);
        }
        let (session, driver) = server
            .accept(now(), Transport::new(transport, address))
            .await
            .expect("handshake failed");
        Session::new(session, driver)
    }
}

/// A MoQ session and its driver task.
struct Session {
    _session: moq_net::Session,
    _driver: AbortOnDropHandle<()>,
}

impl Session {
    fn new(session: moq_net::Session, driver: moq_net::Driver) -> Self {
        let task = tokio::spawn(async move {
            moq_net::time::run(driver).await;
        });
        Self {
            _session: session,
            _driver: AbortOnDropHandle::new(task),
        }
    }
}

/// Dials `remote` with `subscriber` and `publisher` as the session's origins.
async fn dial(
    endpoint: &Endpoint,
    remote: impl Into<EndpointAddr>,
    subscriber: origin::Producer,
    publisher: Option<origin::Consumer>,
) -> Session {
    let transport = iroh_moq::transport::dial(endpoint, remote)
        .await
        .expect("dial failed");
    let mut client = moq_net::Client::new().with_subscriber(subscriber);
    if let Some(publisher) = publisher {
        client = client.with_publisher(publisher);
    }
    let (session, driver) = client
        .connect(
            now(),
            Transport::new(transport, moq_net::transport::Address::Setup),
        )
        .await
        .expect("handshake failed");
    Session::new(session, driver)
}

/// Offers `broadcast` at `path` on `origin` under `route`, and retracts when it ends.
///
/// moq-net creates a broadcast in the origin that announces it, so the offer
/// is a copy of `broadcast`, as the node makes one.
fn serve(
    origin: &origin::Producer,
    path: &str,
    route: Route,
    broadcast: broadcast::Consumer,
) -> AbortOnDropHandle<()> {
    let publication = origin.create_broadcast(path).expect("create broadcast");
    let copy = iroh_moq::forward_broadcast(broadcast, publication.broadcast().clone());
    publication.announce(route).expect("announce");
    AbortOnDropHandle::new(tokio::spawn(async move {
        let _publication = publication;
        copy.await;
    }))
}

/// Waits for `path` to be announced on `origin` and returns the route.
async fn announced_route(origin: &origin::Producer, path: &str) -> Route {
    let mut announced = origin.consume().announced();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match announced.next().await.expect("origin closed") {
                announce::Event::Start(a)
                | announce::Event::Update(a)
                | announce::Event::Restart(a)
                | announce::Event::Takeover { announce: a, .. }
                    if a.prefix.as_str() == path =>
                {
                    return a.route;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{path} was never announced"))
}

/// Resolves `path` through `origin`.
async fn resolve(origin: &origin::Producer, path: &str) -> broadcast::Consumer {
    tokio::time::timeout(
        TIMEOUT,
        origin
            .consume()
            .follow(path, None)
            .expect("follow")
            .available(),
    )
    .await
    .unwrap_or_else(|_| panic!("{path} did not resolve"))
    .expect("resolve failed")
}

/// Collects the paths currently announced on `origin`.
async fn announced_paths(origin: &origin::Producer) -> Vec<String> {
    let mut announced = origin.consume().announced();
    let mut paths = Vec::new();
    // The replay of what is already there arrives without waiting, so a short
    // quiet period means the cursor has caught up.
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(300), announced.next()).await
    {
        let (announce, active) = match event {
            announce::Event::End(announce) => (announce, false),
            announce::Event::Start(announce)
            | announce::Event::Update(announce)
            | announce::Event::Restart(announce)
            | announce::Event::Takeover { announce, .. } => (announce, true),
        };
        let path = announce.prefix.as_str().to_owned();
        if !active {
            paths.retain(|p| *p != path);
        } else if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

/// Waits until the announced paths on `origin` are exactly `expected`.
async fn wait_for_paths(origin: &origin::Producer, expected: &[&str]) {
    let mut expected: Vec<String> = expected.iter().map(|p| (*p).to_owned()).collect();
    expected.sort();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let paths = announced_paths(origin).await;
        if paths == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "announced paths stayed at {paths:?}, expected {expected:?}"
        );
    }
}

/// A broadcast spliced at two paths reaches a subscriber at both, and ends at both.
#[tokio::test]
async fn a_broadcast_is_spliced_at_two_paths() {
    const PUBLISHER_HOP: u64 = 11;
    const SUBSCRIBER_HOP: u64 = 22;

    let mut publisher = Server::spawn().await;
    let id = publisher.endpoint.id();
    let live_path = format!("live/{id}/cam");
    let room_path = format!("rooms/topic/{id}/cam");

    let broadcast = TestBroadcast::start();
    let session_origin = origin(PUBLISHER_HOP);
    let live = serve(
        &session_origin,
        &live_path,
        Route::default(),
        broadcast.producer.consume(),
    );
    let room = serve(
        &session_origin,
        &room_path,
        Route::default(),
        broadcast.producer.consume(),
    );

    let subscriber = endpoint().await;
    let ingest = origin(SUBSCRIBER_HOP);
    let (_client, _server) = tokio::join!(
        dial(&subscriber, publisher.endpoint.addr(), ingest.clone(), None),
        publisher.accept(session_origin.consume(), None),
    );

    // The receiver stamps the peer's hop onto what it announces, so a route
    // published locally with no hops arrives with exactly the publisher's.
    for path in [&live_path, &room_path] {
        let route = announced_route(&ingest, path).await;
        let hops: Vec<u64> = route.hops.iter().map(|hop| hop.id()).collect();
        assert_eq!(hops, [PUBLISHER_HOP], "hop chain of {path}");
    }

    let at_live = resolve(&ingest, &live_path).await;
    let at_room = resolve(&ingest, &room_path).await;
    let first = read_counter(&at_live).await;
    let second = read_counter(&at_room).await;
    // Both read the one track the one writer feeds, not two copies of it.
    assert!(first < 10_000 && second < 10_000, "{first} {second}");

    // Finishing the source ends it at both paths, and the handlers retract
    // their routes so no new request is answered with a closed broadcast.
    broadcast.producer.close();
    for consumer in [&at_live, &at_room] {
        tokio::time::timeout(TIMEOUT, consumer.closed())
            .await
            .expect("a finished broadcast did not close at the subscriber");
    }
    tokio::time::timeout(TIMEOUT, async {
        live.await.ok();
        room.await.ok();
    })
    .await
    .expect("handlers did not stop");
    wait_for_paths(&ingest, &[]).await;
}

/// Per-session publish origins show two peers different publications.
///
/// What a session carries changes while it runs: a later offer appears, a
/// withdrawn one retracts, and a forwarded route keeps its hop chain.
#[tokio::test]
async fn per_session_origins_carry_different_offers() {
    const UPSTREAM_HOP: u64 = 100;
    const NODE_HOP: u64 = 200;
    const BOB_HOP: u64 = 300;
    const CAROL_HOP: u64 = 400;

    // Upstream publishes one broadcast the node will learn and forward.
    let mut upstream = Server::spawn().await;
    let upstream_broadcast = TestBroadcast::start();
    let upstream_origin = origin(UPSTREAM_HOP);
    let _upstream_route = serve(
        &upstream_origin,
        "live/upstream/cam",
        Route::default(),
        upstream_broadcast.producer.consume(),
    );

    let mut node = Server::spawn().await;
    let learned = origin(NODE_HOP);
    let (_to_upstream, _upstream_session) = tokio::join!(
        dial(
            &node.endpoint,
            upstream.endpoint.addr(),
            learned.clone(),
            None
        ),
        upstream.accept(upstream_origin.consume(), None),
    );
    let upstream_route = announced_route(&learned, "live/upstream/cam").await;

    // The node's own publications.
    let everyone = TestBroadcast::start();
    let peers = TestBroadcast::start();
    let manual = TestBroadcast::start();

    // Bob's view: the public broadcast and the one whose audience names him.
    // Carol's: the public one and the manual one offered to her alone.
    let bob_view = origin(NODE_HOP);
    let carol_view = origin(NODE_HOP);
    let bob_public = serve(
        &bob_view,
        "live/node/everyone",
        Route::default(),
        everyone.producer.consume(),
    );
    let _bob_peers = serve(
        &bob_view,
        "live/node/peers",
        Route::default(),
        peers.producer.consume(),
    );
    let _carol_public = serve(
        &carol_view,
        "live/node/everyone",
        Route::default(),
        everyone.producer.consume(),
    );
    let _carol_manual = serve(
        &carol_view,
        "live/node/manual",
        Route::default(),
        manual.producer.consume(),
    );

    let bob = endpoint().await;
    let bob_ingest = origin(BOB_HOP);
    let (_bob_client, _bob_server) = tokio::join!(
        dial(&bob, node.endpoint.addr(), bob_ingest.clone(), None),
        node.accept(bob_view.consume(), None),
    );
    let carol = endpoint().await;
    let carol_ingest = origin(CAROL_HOP);
    let (_carol_client, _carol_server) = tokio::join!(
        dial(&carol, node.endpoint.addr(), carol_ingest.clone(), None),
        node.accept(carol_view.consume(), None),
    );

    wait_for_paths(&bob_ingest, &["live/node/everyone", "live/node/peers"]).await;
    wait_for_paths(&carol_ingest, &["live/node/everyone", "live/node/manual"]).await;
    read_counter(&resolve(&bob_ingest, "live/node/peers").await).await;
    read_counter(&resolve(&carol_ingest, "live/node/manual").await).await;

    // A path Bob was never offered does not resolve for him: nothing covers it.
    let refused = bob_ingest.consume().request("live/node/manual", None).await;
    assert!(
        matches!(refused, Err(moq_net::Error::Unroutable)),
        "{:?}",
        refused.err()
    );

    // Offers change on a running session: Bob loses the public broadcast (its
    // publication was withdrawn from his view) and gains the forwarded one,
    // with the upstream hop chain kept.
    drop(bob_public);
    let _forwarded = {
        let learned = learned.clone();
        let bob_view = bob_view.clone();
        let route = Route::default().with_hops(upstream_route.hops.clone());
        AbortOnDropHandle::new(tokio::spawn(async move {
            let upstream = learned
                .consume()
                .request("live/upstream/cam", None)
                .await
                .expect("request upstream");
            serve(&bob_view, "live/upstream/cam", route, upstream)
                .await
                .ok();
        }))
    };
    wait_for_paths(&bob_ingest, &["live/node/peers", "live/upstream/cam"]).await;

    let route = announced_route(&bob_ingest, "live/upstream/cam").await;
    let hops: Vec<u64> = route.hops.iter().map(|hop| hop.id()).collect();
    assert_eq!(
        hops,
        [UPSTREAM_HOP, NODE_HOP],
        "the forwarded route keeps its origin first and gains the node's hop"
    );
    read_counter(&resolve(&bob_ingest, "live/upstream/cam").await).await;

    // Carol's session never saw any of Bob's changes.
    wait_for_paths(&carol_ingest, &["live/node/everyone", "live/node/manual"]).await;
}

/// A route table serves the cheaper of two routes, and moves to the other when it dies.
///
/// Both sessions announce straight into the table, tagged as links. The
/// publisher's stream has one instance on the direct route and on the relay's,
/// so a reader moves from one to the other without noticing.
#[tokio::test]
#[n0_tracing_test::traced_test]
async fn a_route_table_fails_over_between_links() {
    const PUBLISHER_HOP: u64 = 1000;
    const RELAY_HOP: u64 = 2000;
    const NODE_HOP: u64 = 3000;
    const RELAY_COST: u64 = 10;

    let mut publisher = Server::spawn().await;
    let broadcast = TestBroadcast::start();
    let published = origin(PUBLISHER_HOP);
    let _route = serve(
        &published,
        "live/p/cam",
        Route::default(),
        broadcast.producer.consume(),
    );

    // The relay pulls from the publisher and offers everything it learned.
    let mut relay = Server::spawn().await;
    let relayed = origin(RELAY_HOP);
    let (_relay_up, _publisher_side) = step("relay pulls from the publisher", async {
        tokio::join!(
            dial(
                &relay.endpoint,
                publisher.endpoint.addr(),
                relayed.clone(),
                None
            ),
            publisher.accept(published.consume(), None),
        )
    })
    .await;
    announced_route(&relayed, "live/p/cam").await;

    // The node: one route table, both links announce into it.
    let node = endpoint().await;
    let table = origin(NODE_HOP);
    let relay_addr = relay.endpoint.addr();
    // The client's driver has to run inside the join: from moq-lite-05 on the
    // SETUP a server waits for is written by the driver, not by `connect`.
    let (_relay_link, _relay_side) = step("relay handshake", async {
        tokio::join!(
            async {
                let transport = iroh_moq::transport::dial(&node, relay_addr)
                    .await
                    .expect("dial relay");
                let (session, driver) = moq_net::Client::new()
                    .with_subscriber(table.clone().with_link(Link::Upstream))
                    .with_peer_hop(Hop::new(RELAY_HOP).expect("a hop"))
                    .with_cost(RELAY_COST)
                    .connect(
                        now(),
                        Transport::new(transport, moq_net::transport::Address::Setup),
                    )
                    .await
                    .expect("relay handshake");
                Session::new(session, driver)
            },
            relay.accept(relayed.consume(), None),
        )
    })
    .await;
    let (direct_link, direct_server) = step("direct handshake", async {
        tokio::join!(
            dial(
                &node,
                publisher.endpoint.addr(),
                table.clone().with_link(Link::Peer),
                None
            ),
            publisher.accept(published.consume(), None),
        )
    })
    .await;

    // Both routes are in the table once they have had a moment to arrive, and
    // it announces the best: the direct one, which is cheaper.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let best = announced_route(&table, "live/p/cam").await;
    assert_eq!(
        best.hops.iter().next().map(|hop| hop.id()),
        Some(PUBLISHER_HOP)
    );
    assert!(best.cost.value() < RELAY_COST, "{:?}", best.cost);

    let consumer = resolve(&table, "live/p/cam").await;
    let mut subscriber = step(
        "subscribe to the track",
        consumer
            .track("video")
            .expect("track")
            .subscribe(subscription()),
    )
    .await
    .expect("subscribe");
    let mut last = 0;
    for _ in 0..3 {
        last = next_counter(&mut subscriber).await;
    }

    // The direct link goes. Both routes serve one instance, so the table moves
    // the same subscription to the relay's route, which continues the source.
    drop(direct_link);
    drop(direct_server);
    step("the relay continues the source", async {
        loop {
            if next_counter(&mut subscriber).await > last {
                break;
            }
        }
    })
    .await;
    assert!(
        !consumer.is_closed(),
        "the broadcast ended with the direct link"
    );
}

/// Reads the next frame's counter from `subscriber`.
async fn next_counter(subscriber: &mut moq_net::track::Subscriber) -> u64 {
    loop {
        let mut group = tokio::time::timeout(TIMEOUT, subscriber.recv_group())
            .await
            .expect("no group")
            .expect("track failed")
            .expect("track ended");
        if let Some(frame) = step("read a frame", group.read_frame())
            .await
            .expect("group")
        {
            return u64::from_be_bytes(frame.payload[..].try_into().expect("u64"));
        }
    }
}
