//! The moq-net origin behaviour the transport builds on, over real iroh connections.
//!
//! A moq-net release that changes any of it breaks publishing in ways our
//! other tests would not notice.

mod common;

use std::{num::NonZeroU64, time::Duration};

use common::{TIMEOUT, TestBroadcast, endpoint, read_counter, step, subscription};
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use moq_net::{
    Pattern, Patterns, announce, auth, broadcast,
    origin::{self, Link, RouteEvent, RouteInfo, RouteOptions},
};
use n0_future::task::AbortOnDropHandle;
use tokio::sync::mpsc;

/// Creates an origin with hop `hop`, its driver spawned.
fn origin(hop: u64) -> origin::Producer {
    moq_tokio::origin::spawn_config(origin::Config::new(
        NonZeroU64::new(hop).expect("a non-zero hop"),
    ))
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
        for alpn in iroh_live_node::alpns() {
            router = router.accept(alpn, Forward(tx.clone()));
        }
        Self {
            endpoint,
            incoming,
            _router: router.spawn(),
        }
    }

    /// Accepts the next session with `publisher` and `subscriber` as its origins.
    ///
    /// With `grant`, authorizes the session for it before the handshake
    /// completes, as the node does.
    async fn accept(
        &mut self,
        publisher: origin::Consumer,
        subscriber: Option<origin::Producer>,
        grant: Option<auth::Grant>,
    ) -> Session {
        let connection = tokio::time::timeout(TIMEOUT, self.incoming.recv())
            .await
            .expect("timed out waiting for a connection")
            .expect("router gone");
        let transport = moq_transport_iroh::accept(connection).expect("a MoQ ALPN");
        let handshake = moq_net::Server::new()
            .accept_request(now(), transport)
            .await
            .expect("handshake failed");
        if let Some(grant) = grant {
            handshake.auth().authorize(&grant);
        }
        let mut handshake = handshake.with_publisher(publisher);
        if let Some(subscriber) = subscriber {
            handshake = handshake.with_subscriber(subscriber);
        }
        let (session, driver) = handshake.ok().await.expect("handshake failed");
        Session::new(session, driver)
    }
}

/// A MoQ session and its driver task.
struct Session {
    session: moq_net::Session,
    _driver: AbortOnDropHandle<()>,
}

impl Session {
    fn new(session: moq_net::Session, driver: moq_net::Driver) -> Self {
        let task = tokio::spawn(async move {
            moq_net::time::run(driver).await;
        });
        Self {
            session,
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
    let transport = moq_transport_iroh::dial(endpoint, remote)
        .await
        .expect("dial failed");
    let mut client = moq_net::Client::new().with_subscriber(subscriber);
    if let Some(publisher) = publisher {
        client = client.with_publisher(publisher);
    }
    let (session, driver) = client
        .connect(now(), transport)
        .await
        .expect("handshake failed");
    Session::new(session, driver)
}

/// Publishes a counter broadcast at `path` in `origin`, until the handles drop.
fn publish(origin: &origin::Producer, path: &str) -> (origin::Route, TestBroadcast) {
    let route = origin
        .route(path, RouteOptions::default())
        .expect("publish into the origin");
    let writer = TestBroadcast::writing(route.broadcast().clone(), 0);
    (route, writer)
}

/// Returns a grant to subscribe to exactly `paths`, as the node gives a session.
fn grant(paths: &[&str]) -> auth::Grant {
    auth::Grant {
        publish: Patterns::new(),
        subscribe: paths
            .iter()
            .map(|path| Pattern::literal(path).expect("a literal"))
            .collect(),
        expires: None,
    }
}

/// Waits for `path` to be announced on `origin` and returns the route.
async fn announced_route(origin: &origin::Producer, path: &str) -> RouteInfo {
    let mut announced = origin.consume().announced();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match announced.next().await.expect("origin closed") {
                announce::Event::Present(a) if a.prefix.as_str() == path => {
                    return a.route;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{path} was never announced"))
}

/// Waits until the best route `origin` announces at `path` is one that `wanted` accepts.
async fn best_route(
    origin: &origin::Producer,
    path: &str,
    wanted: impl Fn(&RouteInfo) -> bool,
) -> RouteInfo {
    let mut routes = origin.consume().routes();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match routes.next().await.expect("origin closed") {
                RouteEvent::Start(announce)
                | RouteEvent::Update(announce)
                | RouteEvent::Restart(announce)
                | RouteEvent::Takeover { announce, .. }
                    if announce.prefix.as_str() == path && wanted(&announce.route) =>
                {
                    return announce.route;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the wanted route to {path} never became the best"))
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
    .expect("the first change is an instance")
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
            announce::Event::Absent(announce) => (announce, false),
            announce::Event::Present(announce) => (announce, true),
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

/// One origin shows each session what its grant allows, and nothing it learned upstream.
///
/// The node serves every session the same local view of one origin, and the
/// session's grant decides what the peer sees. Re-authorizing a live session
/// narrows or widens it at once: a narrowed peer loses the path and what it
/// reads there is reset, and the other session never notices.
#[tokio::test]
#[n0_tracing_test::traced_test]
async fn grants_show_sessions_different_publications_of_one_origin() {
    const UPSTREAM_HOP: u64 = 100;
    const NODE_HOP: u64 = 200;
    const BOB_HOP: u64 = 300;
    const CAROL_HOP: u64 = 400;
    const EVERYONE: &str = "live/node/everyone";
    const PEERS: &str = "live/node/peers";
    const MANUAL: &str = "live/node/manual";
    const LEARNED: &str = "live/upstream/cam";

    // The node learns a route from upstream, tagged so it is never re-offered.
    let mut upstream = Server::spawn().await;
    let upstream_origin = origin(UPSTREAM_HOP);
    let _upstream_route = publish(&upstream_origin, LEARNED);
    let mut node = Server::spawn().await;
    let table = origin(NODE_HOP);
    let (_to_upstream, _upstream_session) = tokio::join!(
        dial(
            &node.endpoint,
            upstream.endpoint.addr(),
            table.clone().with_link(Link::Upstream),
            None
        ),
        upstream.accept(upstream_origin.consume(), None, None),
    );
    announced_route(&table, LEARNED).await;

    // The node's own publications, each created once in the one origin.
    let _everyone = publish(&table, EVERYONE);
    let _peers = publish(&table, PEERS);
    let _manual = publish(&table, MANUAL);

    // Bob may see the public one and the one whose audience names him. Carol
    // may see the public one and the manual one. Both grants also name the
    // learned path, which the local view hides all the same.
    let served = table.consume().local();
    let bob = endpoint().await;
    let bob_ingest = origin(BOB_HOP);
    let (_bob_client, bob_server) = tokio::join!(
        dial(&bob, node.endpoint.addr(), bob_ingest.clone(), None),
        node.accept(
            served.clone(),
            None,
            Some(grant(&[EVERYONE, PEERS, LEARNED]))
        ),
    );
    let carol = endpoint().await;
    let carol_ingest = origin(CAROL_HOP);
    let (_carol_client, _carol_server) = tokio::join!(
        dial(&carol, node.endpoint.addr(), carol_ingest.clone(), None),
        node.accept(served, None, Some(grant(&[EVERYONE, MANUAL, LEARNED]))),
    );

    wait_for_paths(&bob_ingest, &[EVERYONE, PEERS]).await;
    wait_for_paths(&carol_ingest, &[EVERYONE, MANUAL]).await;
    let bob_everyone = resolve(&bob_ingest, EVERYONE).await;
    let mut bob_reading = step(
        "bob subscribes",
        bob_everyone
            .track("video")
            .expect("track")
            .subscribe(subscription()),
    )
    .await
    .expect("subscribe");
    read_counter(&resolve(&carol_ingest, MANUAL).await).await;

    // A path outside Bob's grant does not resolve for him.
    let refused = bob_ingest.consume().request(MANUAL, None).await;
    assert!(refused.is_err(), "bob resolved a path outside his grant");

    // Narrowing Bob's live session takes the public one from him, resetting
    // what he reads there, and widening it gives him the manual one.
    bob_server
        .session
        .auth()
        .authorize(&grant(&[PEERS, MANUAL]));
    let err = step("bob's read ends", async {
        loop {
            match bob_reading.recv_group().await {
                Ok(Some(_)) => continue,
                Ok(None) => panic!("bob's read finished instead of being reset"),
                Err(err) => return err,
            }
        }
    })
    .await;
    assert!(
        matches!(
            err,
            moq_net::Error::Unauthorized
                | moq_net::Error::Stream(moq_net::StreamError::Unauthorized)
        ),
        "bob's read ended with {err:?}"
    );
    wait_for_paths(&bob_ingest, &[MANUAL, PEERS]).await;
    read_counter(&resolve(&bob_ingest, MANUAL).await).await;

    // Carol's session never saw any of Bob's changes.
    wait_for_paths(&carol_ingest, &[EVERYONE, MANUAL]).await;
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
    let published = origin(PUBLISHER_HOP);
    let _route = publish(&published, "live/p/cam");

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
            publisher.accept(published.consume(), None, None),
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
                let transport = moq_transport_iroh::dial(&node, relay_addr)
                    .await
                    .expect("dial relay");
                let (session, driver) = moq_net::Client::new()
                    .with_subscriber(table.clone().with_link(Link::Upstream))
                    .with_peer_hop(NonZeroU64::new(RELAY_HOP).expect("a hop"))
                    .with_cost(RELAY_COST)
                    .connect(now(), transport)
                    .await
                    .expect("relay handshake");
                Session::new(session, driver)
            },
            relay.accept(relayed.consume(), None, None),
        )
    })
    .await;
    // The relay's route is in the table before the direct one comes.
    let relayed_route = announced_route(&table, "live/p/cam").await;
    assert!(
        relayed_route.cost().value() >= RELAY_COST,
        "{:?}",
        relayed_route.cost()
    );
    let (direct_link, direct_server) = step("direct handshake", async {
        tokio::join!(
            dial(
                &node,
                publisher.endpoint.addr(),
                table.clone().with_link(Link::Peer),
                None
            ),
            publisher.accept(published.consume(), None, None),
        )
    })
    .await;

    // Both routes are in the table, and it announces the best: the direct
    // one, which is cheaper. The relay's route was there first.
    let best = best_route(&table, "live/p/cam", |route| {
        route.cost().value() < RELAY_COST
    })
    .await;
    assert_eq!(best.hops().next(), Some(PUBLISHER_HOP));
    assert!(best.cost().value() < RELAY_COST, "{:?}", best.cost());

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
