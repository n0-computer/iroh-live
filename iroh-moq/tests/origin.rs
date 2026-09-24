//! What the transport builds on in moq-net's origin model, checked over real
//! iroh connections.
//!
//! These started as the two spikes phase 3 of the API refactor depended on and
//! stay as regression tests, because a moq-net release that changes either
//! behaviour breaks publishing in ways no unit test of ours would notice:
//!
//! - an existing broadcast can be published at several paths at once by
//!   answering `origin::Dynamic` requests with `Request::accept`, which splices
//!   the one broadcast rather than copying it;
//! - a per-session publish origin can carry a different set of publications for
//!   each peer, change while the session runs, and forward a route learned from
//!   elsewhere with its hop chain intact.

mod common;

use std::time::Duration;

use common::{MAX_AGE, TIMEOUT, TestBroadcast, endpoint, read_counter, step};
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use moq_net::{
    Hop,
    announce::Kind as AnnounceKind,
    broadcast,
    origin::{self, Route},
    track,
};
use n0_future::task::AbortOnDropHandle;
use tokio::sync::mpsc;

type Transport = moq_tokio::transport::Session<web_transport_iroh::Session>;

/// Creates an origin with hop `hop`, its driver spawned.
fn origin(hop: u64) -> origin::Producer {
    moq_tokio::origin::spawn_config(origin::Config::new(Hop::new(hop).expect("a non-zero hop")))
}

fn now() -> std::time::Instant {
    tokio::time::Instant::now().into_std()
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

/// A node that accepts raw MoQ connections and lets the test decide, per
/// connection, which origins the session gets.
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

    /// Accepts the next session, publishing `publisher` to it and writing what
    /// it announces into `subscriber`.
    async fn accept(
        &mut self,
        publisher: origin::Consumer,
        subscriber: Option<origin::Producer>,
    ) -> Session {
        let connection = tokio::time::timeout(TIMEOUT, self.incoming.recv())
            .await
            .expect("timed out waiting for a connection")
            .expect("router gone");
        // Whichever ALPN the two sides settled on: an H3 one answers a CONNECT
        // first, a raw one carries MoQ directly.
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
        let mut server = moq_net::Server::new().with_publisher(publisher);
        if let Some(subscriber) = subscriber {
            server = server.with_subscriber(subscriber);
        }
        let (session, driver) = server
            .accept(now(), Transport::new(transport))
            .await
            .expect("handshake failed");
        Session::new(session, driver)
    }
}

/// A MoQ session and the task driving it.
struct Session {
    _session: moq_net::Session,
    _driver: AbortOnDropHandle<()>,
}

impl Session {
    fn new(session: moq_net::Session, driver: moq_net::Driver<Transport>) -> Self {
        let task = tokio::spawn(async move {
            moq_net::time::run(driver).await;
        });
        Self {
            _session: session,
            _driver: AbortOnDropHandle::new(task),
        }
    }
}

/// Dials `remote`, writing what it announces into `subscriber` and publishing
/// `publisher`, if given.
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
        .connect(now(), Transport::new(transport))
        .await
        .expect("handshake failed");
    Session::new(session, driver)
}

/// Answers every request under `dynamic` with `broadcast`, until the broadcast
/// ends, and then retracts the route.
///
/// Retracting on close matters: a route that stays up after its broadcast ended
/// sends every new request to a handler that can only hand back a closed
/// broadcast.
fn serve(dynamic: origin::Dynamic, broadcast: broadcast::Consumer) -> AbortOnDropHandle<()> {
    AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = broadcast.closed() => break,
                request = dynamic.requested_broadcast() => match request {
                    Ok(request) => request.accept(&broadcast),
                    Err(_) => break,
                },
            }
        }
        drop(dynamic);
    }))
}

/// Waits for `path` to be announced on `origin` and returns the route.
async fn announced_route(origin: &origin::Producer, path: &str) -> Route {
    tokio::time::timeout(TIMEOUT, origin.consume().routed(path))
        .await
        .unwrap_or_else(|_| panic!("{path} was never announced"))
        .expect("origin closed")
}

/// Resolves `path` through `origin`.
async fn resolve(origin: &origin::Producer, path: &str) -> broadcast::Consumer {
    tokio::time::timeout(TIMEOUT, origin.consume().routed_broadcast(path))
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
    while let Ok(Some(update)) =
        tokio::time::timeout(Duration::from_millis(300), announced.next()).await
    {
        let path = update.prefix.as_str().to_owned();
        if update.kind == AnnounceKind::Retracted {
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

/// Spike 1: one existing broadcast, published at two paths at once through
/// dynamic routes answered with `Request::accept`, reaches a subscriber at both,
/// and ends at both when it finishes.
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
        session_origin
            .dynamic(&live_path, Route::default())
            .expect("live route"),
        broadcast.producer.consume(),
    );
    let room = serve(
        session_origin
            .dynamic(&room_path, Route::default())
            .expect("room route"),
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
    broadcast.producer.finish();
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

/// Spike 2: each session gets its own publish origin, so two peers see
/// different publications, and what a session carries changes while it runs:
/// an offer made later appears, a withdrawn one retracts, and a route learned
/// from another peer is forwarded with its hop chain intact.
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
        upstream_origin
            .dynamic("live/upstream/cam", Route::default())
            .expect("route"),
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
        bob_view
            .dynamic("live/node/everyone", Route::default())
            .expect("route"),
        everyone.producer.consume(),
    );
    let _bob_peers = serve(
        bob_view
            .dynamic("live/node/peers", Route::default())
            .expect("route"),
        peers.producer.consume(),
    );
    let _carol_public = serve(
        carol_view
            .dynamic("live/node/everyone", Route::default())
            .expect("route"),
        everyone.producer.consume(),
    );
    let _carol_manual = serve(
        carol_view
            .dynamic("live/node/manual", Route::default())
            .expect("route"),
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
    let refused = bob_ingest
        .consume()
        .request_broadcast("live/node/manual")
        .await;
    assert!(
        matches!(refused, Err(moq_net::Error::Unroutable)),
        "{:?}",
        refused.err()
    );

    // Offers change on a running session: Bob loses the public broadcast (its
    // publication was withdrawn from his view) and gains the forwarded one,
    // with the upstream hop chain kept.
    drop(bob_public);
    let forwarded = bob_view
        .dynamic(
            "live/upstream/cam",
            Route::default().with_hops(upstream_route.hops.clone()),
        )
        .expect("forwarded route");
    let _forwarded = {
        let learned = learned.clone();
        AbortOnDropHandle::new(tokio::spawn(async move {
            while let Ok(request) = forwarded.requested_broadcast().await {
                match learned
                    .consume()
                    .request_broadcast(request.path().to_owned())
                    .await
                {
                    Ok(broadcast) => request.accept(broadcast),
                    Err(err) => request.reject(err),
                }
            }
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

/// Mirrors every route `ingest` holds into `table`, answering requests there by
/// resolving the path through `ingest`.
///
/// This is how a node keeps one route table while still knowing which link each
/// route came over: every link writes into an origin of its own, and the table
/// holds one dynamic route per link route, with the hop chain and cost copied.
fn bridge(ingest: origin::Producer, table: origin::Producer) -> AbortOnDropHandle<()> {
    AbortOnDropHandle::new(tokio::spawn(async move {
        let mut announced = ingest.consume().announced();
        let mut routes = std::collections::HashMap::new();
        while let Some(update) = announced.next().await {
            let prefix = update.prefix.clone();
            if update.kind == AnnounceKind::Retracted {
                routes.remove(&prefix);
                continue;
            }
            let route = Route::default()
                .with_hops(update.route.hops.clone())
                .with_cost(update.route.cost);
            let dynamic = table.dynamic(&prefix, route).expect("mirror route");
            let ingest = ingest.clone();
            let task = AbortOnDropHandle::new(tokio::spawn(async move {
                while let Ok(request) = dynamic.requested_broadcast().await {
                    match ingest
                        .consume()
                        .request_broadcast(request.path().to_owned())
                        .await
                    {
                        Ok(broadcast) => request.accept(broadcast),
                        Err(err) => request.reject(err),
                    }
                }
            }));
            routes.insert(prefix, task);
        }
    }))
}

/// Spike 3: two links to one broadcast, a direct one and one through a relay,
/// each mirrored into one route table. The table serves the cheaper route, and
/// when the direct link dies the subscription carries on through the relay,
/// because both routes share their first hop (the publisher).
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
        published
            .dynamic("live/p/cam", Route::default())
            .expect("route"),
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

    // The node: one ingest origin per link, both mirrored into the table.
    let node = endpoint().await;
    let table = origin(NODE_HOP);
    let direct_ingest = origin(NODE_HOP);
    let relay_ingest = origin(NODE_HOP);
    let _direct_bridge = bridge(direct_ingest.clone(), table.clone());
    let _relay_bridge = bridge(relay_ingest.clone(), table.clone());

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
                    .with_subscriber(relay_ingest.clone())
                    .with_cost(RELAY_COST)
                    .connect(now(), Transport::new(transport))
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
                direct_ingest.clone(),
                None
            ),
            publisher.accept(published.consume(), None),
        )
    })
    .await;

    // Both routes are in; the direct one is cheaper and serves.
    let direct = announced_route(&direct_ingest, "live/p/cam").await;
    let via_relay = announced_route(&relay_ingest, "live/p/cam").await;
    let first = |route: &Route| route.hops.iter().next().map(|hop| hop.id());
    assert_eq!(first(&direct), Some(PUBLISHER_HOP));
    assert_eq!(first(&via_relay), Some(PUBLISHER_HOP));
    assert!(
        direct.cost < via_relay.cost,
        "{:?} {:?}",
        direct.cost,
        via_relay.cost
    );

    let consumer = resolve(&table, "live/p/cam").await;
    let mut subscriber = step(
        "subscribe to the track",
        consumer
            .track("video")
            .expect("track")
            .subscribe(track::Subscription::default().with_max_age(MAX_AGE)),
    )
    .await
    .expect("subscribe");
    let mut last = 0;
    for _ in 0..3 {
        let mut group = tokio::time::timeout(TIMEOUT, subscriber.recv_group())
            .await
            .expect("no group")
            .expect("track failed")
            .expect("track ended");
        if let Some(frame) = step("read a frame", group.read_frame())
            .await
            .expect("group")
        {
            last = u64::from_be_bytes(frame.payload[..].try_into().expect("u64"));
        }
    }

    // The direct link goes; the relay's route takes over the same broadcast.
    drop(direct_link);
    drop(direct_server);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let group = subscriber
                .recv_group()
                .await
                .expect("track failed after failover")
                .expect("track ended at failover");
            let mut group = group;
            if let Some(frame) = group.read_frame().await.expect("group") {
                let n = u64::from_be_bytes(frame.payload[..].try_into().expect("u64"));
                // Well past anything the direct link could still have had queued.
                if n > last + 200 {
                    break;
                }
            }
        }
    })
    .await
    .expect("the subscription did not continue through the relay");
    assert!(
        !consumer.is_closed(),
        "failover within one first hop re-splices"
    );
}
