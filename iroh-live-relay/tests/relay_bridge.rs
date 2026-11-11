//! The relay's bridging, pulls and admission, over real connections.
//!
//! Endpoints use `presets::Minimal` and a shared `MemoryLookup`, so no test
//! needs network discovery. Each test binds its own relay and endpoints, on
//! random ports and with fresh keys, and each endpoint has its own entry in
//! the lookup. The tests therefore share nothing and run in parallel.

use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
    time::Duration,
};

use iroh::{address_lookup::MemoryLookup, protocol::AcceptError};
use iroh_live::{
    layout::LiveLayout,
    media::{PlayerConfig, VideoEncoding, VideoRendition, VideoSource, remote_broadcast, video},
};
use iroh_live_node::{Audience, Error, LinkKind, Reach, RelayConfig, RelayStatus};
use moq_auth::{Algorithm, Claims, Key, KeySet};
use moq_ext_access::policy::{JwtVerifier, Verify};
use moq_net::{announce::Event, origin};
use moq_relay::cluster::Cluster;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tracing::{debug, info};

/// How long any one wait may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The address lookup every test endpoint registers with.
static ADDRESS_LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();

/// Returns the shared MemoryLookup, creating it if needed.
fn shared_lookup() -> MemoryLookup {
    ADDRESS_LOOKUP.get_or_init(Default::default).clone()
}

/// A relay: noq server, iroh endpoint and cluster.
///
/// Its tasks end with it, so a test that panics takes its relay along.
struct TestRelay {
    /// Runs moq-relay: its listener, cluster and sessions.
    _relay_task: AbortOnDropHandle<()>,
    /// Answers the shipped relay's admissions.
    _admit_task: Option<AbortOnDropHandle<()>>,
    /// Accepts iroh clients.
    _iroh_router: iroh::protocol::Router,
    cluster: Cluster,
    noq_addr: std::net::SocketAddr,
    iroh_id: iroh::EndpointId,
}

impl TestRelay {
    /// Starts a relay that lets anyone publish anywhere.
    ///
    /// The forgery tests use it as the relay a node must not trust.
    async fn start() -> Self {
        Self::start_with(false, false, None, None).await
    }

    /// Starts a relay with the admission `iroh_live_relay::run` uses.
    async fn start_shipped() -> Self {
        Self::start_with(true, false, None, None).await
    }

    /// Starts a relay as [`start_shipped`](Self::start_shipped) does, which verifies JWTs with `keys`.
    ///
    /// As `iroh_live_relay::run` does with `--auth-key`.
    async fn start_keyed(keys: KeySet) -> Self {
        Self::start_with(true, false, None, Some(keys)).await
    }

    /// Starts a relay as [`start_shipped`](Self::start_shipped) does, which also pulls tickets.
    ///
    /// An iroh session that names a ticket in its path pulls the ticket's broadcast.
    async fn start_pulling() -> Self {
        Self::start_with(true, true, None, None).await
    }

    /// Starts a relay with the admission of the shipped relay if `shipped`, else an open one.
    ///
    /// Starts a relay that lets anyone publish anywhere, and serves its
    /// certificate's fingerprint over HTTP on the port of its QUIC listener.
    ///
    /// A client dials it as `http://localhost:<port>`: moq-tokio fetches the
    /// fingerprint there and pins the self-signed certificate.
    async fn start_http() -> Self {
        Self::start_with(false, false, Some(free_port()), None).await
    }

    /// With `pull`, the shipped admission pulls the tickets iroh sessions name.
    /// With `port`, QUIC and the web listener bind that port, else QUIC binds any.
    /// With `keys`, the shipped admission and the iroh sessions verify JWTs.
    async fn start_with(
        shipped: bool,
        pull: bool,
        port: Option<u16>,
        keys: Option<KeySet>,
    ) -> Self {
        let alpns = moq_transport_iroh::alpns()
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();

        let iroh = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .address_lookup(shared_lookup())
            .secret_key(iroh::SecretKey::generate())
            .alpns(alpns)
            .bind()
            .await
            .expect("bind relay iroh");

        shared_lookup().add_endpoint_info(iroh.addr());
        let iroh_id = iroh.id();

        let mut config = moq_relay::Config::default();
        let bind = std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port.unwrap_or(0)));
        config.listen.bind = Some(moq_tokio::listen::Bind::Addr(bind));
        if port.is_some() {
            config.web.http.listen = Some(bind);
        }
        config.listen.tls.generate = vec!["localhost".into()];
        if !shipped {
            config.auth.public = vec![moq_net::Pattern::all()];
        }
        let mut relay = moq_relay::Relay::load(config)
            .await
            .expect("load relay")
            .with_signals(false);
        let cluster = relay.cluster().clone();
        let noq_addr = relay.quic_addr().expect("a quic listener");

        // moq-tokio's server has no iroh backend, so iroh clients always come
        // in through a handler: the shipped admission, or one that admits all.
        let (iroh_router, admit_task) = if shipped {
            let pulls = pull.then(|| {
                Arc::new(
                    iroh_live_relay::pull::PullState::new(iroh.clone(), cluster.clone())
                        .with_linger(PULL_LINGER),
                )
            });
            // As the shipped relay does: a browser session that names a
            // ticket reads the pulled broadcast at that name.
            let admissions = relay.admissions().expect("no auth source configured");
            let admit_task = AbortOnDropHandle::new(tokio::spawn(iroh_live_relay::admit(
                admissions,
                pulls.clone(),
                keys.clone(),
            )));
            let mut sessions = iroh_live_relay::IrohSessions::new(cluster.clone(), pulls);
            if let Some(keys) = keys {
                sessions = sessions.with_policy(
                    Verify::new(JwtVerifier::new(keys)).fallback(iroh_live_relay::relay_policy()),
                );
            }
            let router = sessions.router(iroh);
            (router, Some(admit_task))
        } else {
            let mut router = iroh::protocol::Router::builder(iroh);
            for alpn in moq_transport_iroh::alpns() {
                router = router.accept(alpn, OpenSessions(cluster.clone()));
            }
            (router.spawn(), None)
        };
        let ready = relay.ready();
        let relay_task = AbortOnDropHandle::new(tokio::spawn(async move {
            relay.run().await.expect("relay failed");
        }));
        ready.wait().await.expect("relay ready");

        Self {
            _relay_task: relay_task,
            _admit_task: admit_task,
            _iroh_router: iroh_router,
            cluster,
            noq_addr,
            iroh_id,
        }
    }

    /// Returns the URL that pins the relay's certificate through its fingerprint.
    ///
    /// Only a relay from [`start_http`](Self::start_http) serves the fingerprint.
    fn http_url(&self) -> url::Url {
        format!("http://localhost:{}", self.noq_addr.port())
            .parse()
            .expect("valid url")
    }

    /// Returns the WebTransport URL a browser would dial.
    fn url(&self) -> url::Url {
        format!("https://localhost:{}", self.noq_addr.port())
            .parse()
            .expect("valid url")
    }
}

/// Returns a port that is free for both UDP and TCP right now.
///
/// The relay's QUIC listener and its web listener share the port, since an
/// `http://` client fetches the fingerprint from the port it dials.
fn free_port() -> u16 {
    for _ in 0..16 {
        let udp =
            std::net::UdpSocket::bind((std::net::Ipv6Addr::UNSPECIFIED, 0)).expect("bind udp");
        let port = udp.local_addr().expect("an address").port();
        if std::net::TcpListener::bind((std::net::Ipv6Addr::UNSPECIFIED, port)).is_ok() {
            return port;
        }
    }
    panic!("no port free for both UDP and TCP");
}

/// Admits every iroh client to publish and subscribe anywhere in the cluster.
///
/// The relay the forgery tests use as one a node must not trust.
#[derive(derive_more::Debug, Clone)]
struct OpenSessions(#[debug(skip)] Cluster);

impl iroh::protocol::ProtocolHandler for OpenSessions {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        let transport = moq_transport_iroh::accept(connection).map_err(AcceptError::from_err)?;
        let handshake = moq_net::Server::new()
            .accept_request(moq_net::time::Instant::now(), transport)
            .await
            .map_err(AcceptError::from_err)?;
        let (_session, driver) = handshake
            .with_publisher(self.0.origin.consume())
            .with_subscriber(self.0.origin.clone())
            .ok()
            .await
            .map_err(AcceptError::from_err)?;
        let err = moq_net::time::run(driver).await;
        debug!(%err, "iroh session ended");
        Ok(())
    }
}

/// Builds a noq client that trusts the relay's self-signed certificate.
fn noq_client() -> moq_tokio::Client {
    let mut connect = moq_tokio::connect::Config::default();
    connect.tls.insecure = Some(true);
    connect
        .init(moq_tokio::quic::Config::default())
        .expect("init client")
        .with_reconnect(false)
}

/// Waits for a dialed connection to complete its MoQ handshake.
async fn established(connection: moq_tokio::Connection) -> moq_tokio::Connection {
    tokio::time::timeout(TIMEOUT, connection.established())
        .await
        .expect("connect timeout")
        .expect("connect")
}

/// Resolves the broadcast an origin serves at `path`, once it has a route.
async fn routed_broadcast(
    consumer: &origin::Consumer,
    path: &str,
) -> Result<moq_net::broadcast::Consumer, moq_net::Error> {
    consumer
        .follow(path, None)?
        .available()
        .await?
        .ok_or(moq_net::Error::Closed)
}

/// Returns the announcement an event carries.
fn announce_of(event: &Event) -> &moq_net::announce::Announce {
    match event {
        Event::Present(announce) | Event::Absent(announce) => announce,
    }
}

/// Resolves the broadcast behind the next announcement on `origin`.
async fn next_announced(
    origin: &origin::Producer,
    expect: &str,
) -> (String, moq_net::broadcast::Consumer) {
    let consumer = origin.consume();
    let mut announcements = consumer.announced();
    let update = tokio::time::timeout(TIMEOUT, announcements.next())
        .await
        .unwrap_or_else(|_| panic!("announce timeout: {expect}"))
        .expect("closed");
    let path = announce_of(&update).prefix.as_str().to_owned();
    let broadcast = tokio::time::timeout(TIMEOUT, routed_broadcast(&consumer, path.as_str()))
        .await
        .expect("resolve timeout")
        .expect("resolve");
    (path, broadcast)
}

/// Waits until `path` is announced on `origin`, and resolves it.
async fn announced_at(origin: &origin::Producer, path: &str) -> moq_net::broadcast::Consumer {
    let consumer = origin.consume();
    tokio::time::timeout(TIMEOUT, routed_broadcast(&consumer, path))
        .await
        .unwrap_or_else(|_| panic!("{path} was never announced"))
        .expect("resolve")
}

/// Returns the default layout's path of `peer`'s broadcast `name`.
fn live_path(peer: iroh::EndpointId, name: &str) -> String {
    LiveLayout::default()
        .path(peer, name)
        .expect("a valid broadcast name")
        .to_string()
}

/// Publishes a generated video broadcast as `name` on `node`.
fn publish_video(node: &iroh_live::Node, name: &str) -> iroh_live::media::LocalBroadcast {
    let broadcast: iroh_live::media::LocalBroadcast =
        LiveLayout::default().publish(node, name).expect("publish");
    let source = VideoSource::test_pattern(
        video::Size::new(320, 240),
        video::FrameRate::new(30, 1).expect("a valid rate"),
    )
    .expect("the pattern thread starts");
    broadcast
        .set_video(source, VideoEncoding::single(VideoRendition::new("video")))
        .expect("set video");
    broadcast
}

/// Reads the first frame of the latest group on `track`.
async fn first_frame(
    broadcast: &moq_net::broadcast::Consumer,
    track: &str,
) -> moq_net::frame::Frame {
    let track = broadcast.track(track).expect("track");
    let mut subscriber = tokio::time::timeout(TIMEOUT, track.subscribe(None))
        .await
        .expect("subscribe timeout")
        .expect("subscribe");
    let mut group = tokio::time::timeout(TIMEOUT, subscriber.recv_group())
        .await
        .expect("group timeout")
        .expect("group err")
        .expect("group closed");
    tokio::time::timeout(TIMEOUT, group.read_frame())
        .await
        .expect("frame timeout")
        .expect("frame err")
        .expect("frame closed")
}

/// Baseline: noq publish -> relay -> noq subscribe.
#[tokio::test]
async fn noq_publish_noq_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;

    // Publisher
    let pub_origin = moq_tokio::origin::spawn();
    let broadcast = pub_origin
        .route("test", origin::RouteOptions::default())
        .expect("create bc");
    let track = broadcast
        .broadcast()
        .create_track("video", None)
        .expect("track");
    let mut group = track.append_group().expect("group");
    group
        .write_frame(None, b"hello-noq".as_ref())
        .expect("write");
    group.finish().expect("finish");

    let _pub_session = established(
        noq_client()
            .with_publisher(pub_origin.consume())
            .connect(relay.url()),
    )
    .await;

    // Subscriber
    let sub_origin = moq_tokio::origin::spawn();
    let _sub_session = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    let (path, bc) = next_announced(&sub_origin, "noq->noq").await;
    assert_eq!(path, "test");
    let frame = first_frame(&bc, "video").await;
    assert_eq!(&frame.payload[..], b"hello-noq");
}

/// iroh publish -> relay -> iroh subscribe (using iroh-live Node API).
#[tokio::test]
async fn iroh_publish_iroh_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id = relay.iroh_id;

    // Publisher
    let pub_ep = bind_endpoint().await;
    let publisher = iroh_live::Node::new(pub_ep.clone(), LiveLayout::default().config(&[]));
    let _router = serve(&publisher);
    let broadcast = publish_video(&publisher, "relay-test");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.connect(relay_id))
        .await
        .expect("timeout")
        .expect("connect");
    // Through the relay's session, at the path that names the publisher: the
    // same path the publisher offers on a direct session.
    let path = live_path(pub_ep.id(), "relay-test");
    routed(&relay.cluster.origin.consume(), &path).await;

    // Subscriber
    let sub_ep = bind_endpoint().await;
    let subscriber = iroh_live::Node::new(sub_ep.clone(), trusting());
    let sub = tokio::time::timeout(TIMEOUT, async {
        let session = subscriber.connect(relay_id).await?;
        let subscription = session.subscribe(path).await?;
        Ok::<_, iroh_live::node::Error>(remote_broadcast(&subscription))
    })
    .await
    .expect("timeout")
    .expect("subscribe");

    let player = sub
        .play(iroh_live::media::PlayerConfig::default())
        .expect("play");
    let frame = tokio::time::timeout(TIMEOUT, player.video().next())
        .await
        .expect("timeout")
        .expect("closed");
    let size = frame.size();
    assert!(size.width > 0 && size.height > 0);

    drop(player);
    drop(sub);
    drop(_pub_session);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
    sub_ep.close().await;
}

/// noq publish -> relay -> iroh subscribe through `Node::subscribe`.
#[tokio::test]
async fn noq_publish_iroh_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id = relay.iroh_id;

    // Publisher: noq, standing in for a browser, with a hang catalog and a
    // video track.
    let pub_origin = moq_tokio::origin::spawn();
    let broadcast = pub_origin
        .route("browser-stream", origin::RouteOptions::default())
        .expect("bc");

    // hang catalog format: renditions keyed by track name
    let catalog_track = broadcast
        .broadcast()
        .create_track("catalog.json", None)
        .expect("catalog");
    let catalog_json =
        br#"{"video":{"renditions":{"video/h264":{"codec":"avc1.64001f","codedWidth":320,"codedHeight":240,"bitrate":500000,"framerate":30}}}}"#;
    let mut group = catalog_track.append_group().expect("group");
    group
        .write_frame(None, catalog_json.as_ref())
        .expect("write");
    group.finish().expect("finish");

    let video_track = broadcast
        .broadcast()
        .create_track("video/h264", None)
        .expect("video");
    let mut vgroup = video_track.append_group().expect("group");
    vgroup
        .write_frame(None, b"keyframe-data".as_ref())
        .expect("write");
    vgroup.finish().expect("finish");

    let _pub_session = established(
        noq_client()
            .with_publisher(pub_origin.consume())
            .connect(relay.url()),
    )
    .await;

    routed(&relay.cluster.origin.consume(), "browser-stream").await;

    // Subscriber: iroh, through `Node::subscribe`.
    let sub_ep = bind_endpoint().await;
    let subscriber = iroh_live::Node::new(sub_ep.clone(), trusting());
    let sub = tokio::time::timeout(TIMEOUT, async {
        let session = subscriber.connect(relay_id).await?;
        let subscription = session.subscribe("browser-stream").await?;
        Ok::<_, iroh_live::node::Error>(remote_broadcast(&subscription))
    })
    .await
    .expect("subscribe timeout")
    .expect("subscribe");

    // Subscribing proves the route. The catalog arriving and parsing proves
    // the bridge carried the broadcast itself.
    let mut catalog = sub.catalog();
    let parsed = tokio::time::timeout(TIMEOUT, catalog.wait_for(Option::is_some))
        .await
        .expect("no catalog arrived")
        .expect("the broadcast ended")
        .clone()
        .expect("a catalog");
    // The one rendition the noq side wrote, parsed on the far side of the bridge.
    let video = &parsed.video.renditions;
    assert_eq!(video.len(), 1, "the bridged catalog: {video:?}");
    assert_eq!(video["video/h264"].coded_height, Some(240));

    drop(sub);
    drop(_pub_session);
    sub_ep.close().await;
}

/// A noq subscriber reads a broadcast the relay pulls from a ticket.
#[tokio::test]
async fn pull_remote_broadcast_via_ticket() {
    let _ = tracing_subscriber::fmt::try_init();
    // Shipped, since its admission mounts the ticket a browser dials.
    let relay = TestRelay::start_shipped().await;
    let (pub_ep, publisher, _router, broadcast, ticket) = start_publisher("remote-stream").await;

    let local_name = ticket.to_string();
    let pull_state =
        iroh_live_relay::pull::PullState::new(bind_endpoint().await, relay.cluster.clone())
            .with_linger(PULL_LINGER);
    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");

    // Subscriber: noq, standing in for a browser, which dials the ticket as
    // its path and watches the broadcast at its root.
    let sub_origin = moq_tokio::origin::spawn();
    let mut url = relay.url();
    url.set_path(&local_name);
    let _sub_session = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(url),
    )
    .await;

    // The pulled broadcast, read without a copy at the name it asked for.
    let bc = announced_at(&sub_origin, "").await;
    let _frame = first_frame(&bc, "catalog.json").await;
    info!("pull mode test: received catalog from pulled broadcast");

    drop(_sub_session);
    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A relay link over iroh requests the path of its URL, so the relay pulls the ticket it names.
#[tokio::test]
async fn an_iroh_relay_url_requests_its_path() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_pulling().await;
    let (pub_ep, publisher, _router, broadcast, ticket) = start_publisher("by-path").await;
    let name = ticket.to_string();

    let (endpoint, node) = relay_node().await;
    let url = format!("iroh://{}/{name}", relay.iroh_id)
        .parse()
        .expect("a valid URL");
    let link = node.attach_relay(RelayConfig::new(url)).expect("attach");
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &name), &name, true).await,
        "the relay did not pull the ticket the link URL names",
    );

    link.detach().await;
    node.shutdown().await;
    endpoint.close().await;
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A browser that dials a ticket holds a pull of it until its session ends.
#[tokio::test]
async fn a_browser_that_dials_a_ticket_holds_its_pull() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_pulling().await;
    let (pub_ep, publisher, _router, broadcast, ticket) = start_publisher("dialed").await;
    let name = ticket.to_string();

    let sub_origin = moq_tokio::origin::spawn();
    let mut url = relay.url();
    url.set_path(&name);
    let session = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(url),
    )
    .await;
    let bc = announced_at(&sub_origin, "").await;
    let _frame = first_frame(&bc, "catalog.json").await;

    // Past the linger window, the live session still holds the pull. The
    // sleep is the span under test: nothing marks a retirement that does not
    // happen.
    tokio::time::sleep(PULL_LINGER * 3).await;
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &name), &name, true).await,
        "the pull retired while the browser session ran",
    );

    // The pull retires a linger window after the session that held it ends.
    drop(session);
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &name), &name, false).await,
        "the pull outlived the browser session that dialed it",
    );

    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// iroh publish -> relay -> noq subscribe.
#[tokio::test]
async fn iroh_publish_noq_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id = relay.iroh_id;

    // Publisher (iroh via iroh-live)
    let pub_ep = bind_endpoint().await;
    let publisher = iroh_live::Node::new(pub_ep.clone(), LiveLayout::default().config(&[]));
    let _router = serve(&publisher);
    let broadcast = publish_video(&publisher, "cli-stream");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.connect(relay_id))
        .await
        .expect("timeout")
        .expect("connect");

    // Subscriber (noq). `announced_at` waits for the relay to route the path.
    let sub_origin = moq_tokio::origin::spawn();
    let _sub_session = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    let named = live_path(pub_ep.id(), "cli-stream");
    announced_at(&sub_origin, named.as_str()).await;

    info!("noq subscriber received cli-stream announcement");

    drop(_pub_session);
    drop(_sub_session);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// Returns a node config that lets every peer publish anything, a relay it dials included.
///
/// A live node's own policy keeps a peer to `live/<its id>/`, and a relay
/// forwards everyone's broadcasts.
fn trusting() -> iroh_live::NodeConfig {
    iroh_live::NodeConfig::default().with_policy(iroh_live::node::policy::Open(
        iroh_live_node::Grant::everything(),
    ))
}

/// How long an unwatched pull lingers in these tests.
const PULL_LINGER: Duration = Duration::from_millis(200);

/// Mounts `node` on a new router, so peers and the relay can connect to it.
///
/// Keep the router for the whole test: dropping it stops accepting.
fn serve(node: &iroh_live::Node) -> iroh::protocol::Router {
    node.mount(iroh::protocol::Router::builder(node.endpoint().clone()))
        .spawn()
}

/// Starts an iroh publisher with a video broadcast, and returns a ticket to it.
///
/// Keep the router: the relay dials the publisher through it.
async fn start_publisher(
    name: &str,
) -> (
    iroh::Endpoint,
    iroh_live::Node,
    iroh::protocol::Router,
    iroh_live::media::LocalBroadcast,
    iroh_live::BroadcastTicket,
) {
    let endpoint = bind_endpoint().await;

    let node = iroh_live::Node::new(endpoint.clone(), LiveLayout::default().config(&[]));
    let router = serve(&node);
    let broadcast = publish_video(&node, name);
    let ticket = LiveLayout::default()
        .ticket(endpoint.id(), name)
        .expect("a valid broadcast name");
    (endpoint, node, router, broadcast, ticket)
}

/// Binds an endpoint with a fresh identity that the other test endpoints can dial.
async fn bind_endpoint() -> iroh::Endpoint {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind endpoint");
    shared_lookup().add_endpoint_info(endpoint.addr());
    endpoint
}

/// Waits until `node` has no session with `peer`, and panics with `what` after [`TIMEOUT`].
async fn session_closed(node: &iroh_live::Node, peer: iroh::EndpointId, what: &str) {
    let mut sessions = node.sessions();
    tokio::time::timeout(TIMEOUT, async {
        while sessions.get().iter().any(|session| session.peer() == peer) {
            sessions.updated().await.expect("node gone");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

/// Returns the cluster as a session that named the ticket `name` reads it.
fn pulled(cluster: &Cluster, name: &str) -> Cluster {
    iroh_live_relay::pull::mounted(cluster, name).expect("a ticket the cluster mounts")
}

/// Waits until `name` has a route in `cluster` if `present`, or has none if not.
///
/// Returns `false` if that does not happen within [`TIMEOUT`].
async fn wait_for_broadcast(cluster: &Cluster, name: &str, present: bool) -> bool {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let consumer = cluster.origin.consume();
    loop {
        let found =
            tokio::time::timeout(Duration::from_millis(50), routed_broadcast(&consumer, name))
                .await
                .ok()
                .and_then(Result::ok)
                .is_some();
        if found == present {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A pull is announced under the name the client asked for.
///
/// A subscriber is announced only the exact path it subscribed to.
#[tokio::test]
async fn a_pull_is_announced_under_the_name_that_was_asked_for() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, _router, broadcast, ticket) = start_publisher("spelling").await;

    let canonical = ticket.to_string();
    // The scheme is case-insensitive, so this is another spelling of the ticket.
    let bare = canonical
        .strip_prefix("ilv1:")
        .map(|rest| format!("ILV1:{rest}"))
        .expect("a ticket serializes with its scheme");
    assert_ne!(bare, canonical);

    let pull_state =
        iroh_live_relay::pull::PullState::new(bind_endpoint().await, relay.cluster.clone())
            .with_linger(PULL_LINGER);

    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&bare, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &bare), &bare, true).await,
        "the pull was announced somewhere other than the name that was asked for",
    );

    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// An unwatched pull retires and closes its connection, and the next pull dials anew.
#[tokio::test]
async fn pull_retires_an_unwatched_session() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, _router, broadcast, ticket) = start_publisher("retired-stream").await;
    let local_name = ticket.to_string();

    let pull_ep = bind_endpoint().await;
    let pull_id = pull_ep.id();
    let pull_state = iroh_live_relay::pull::PullState::new(pull_ep, relay.cluster.clone())
        .with_linger(PULL_LINGER);

    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &local_name), &local_name, true).await,
        "the pulled broadcast should be announced in the cluster"
    );

    // Nothing wants the pull any more, so it retires and its mirror goes.
    drop(guard);
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &local_name), &local_name, false).await,
        "an unwatched pull should be retired"
    );
    // The relay closes its session with the publisher.
    session_closed(
        &publisher,
        pull_id,
        "the relay kept its session with the publisher of a retired pull",
    )
    .await;

    // The same ticket dials a new session.
    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("re-pull timeout")
        .expect("re-pull");
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &local_name), &local_name, true).await,
        "pulling a retired ticket should dial the publisher again"
    );

    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A pull outlives its publisher re-publishing the broadcast, while a session holds it.
#[tokio::test]
async fn a_pull_survives_the_publisher_republishing() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, _router, broadcast, ticket) = start_publisher("republished").await;
    let local_name = ticket.to_string();

    let pull_state =
        iroh_live_relay::pull::PullState::new(bind_endpoint().await, relay.cluster.clone())
            .with_linger(PULL_LINGER);
    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");
    assert!(wait_for_broadcast(&pulled(&relay.cluster, &local_name), &local_name, true).await);

    drop(broadcast);
    let again = publish_video(&publisher, "republished");

    // The held pull serves the new broadcast, not an orphan of the old one.
    let mut served = false;
    for _ in 0..20 {
        if wait_for_broadcast(&pulled(&relay.cluster, &local_name), &local_name, true).await {
            let mirrored = routed_broadcast(
                &pulled(&relay.cluster, &local_name).origin.consume(),
                local_name.as_str(),
            )
            .await
            .expect("mirrored broadcast");
            let track = mirrored.track("catalog.json").expect("track");
            if let Ok(Ok(_)) =
                tokio::time::timeout(Duration::from_secs(2), track.subscribe(None)).await
            {
                served = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(served, "the pull did not follow the re-published broadcast");

    drop(guard);
    drop(again);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A relay link publishes the node's public broadcasts and consumes the relay's.
#[tokio::test]
async fn a_relay_link_publishes_and_consumes() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;

    let (endpoint, node) = relay_node().await;
    let link = attached(&node, &relay).await;

    // Publishing: a browser finds the node's public broadcast at the path
    // that names the node.
    let broadcast = publish_video(&node, "studio");
    let sub_origin = moq_tokio::origin::spawn();
    let _browser = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;
    let path = live_path(endpoint.id(), "studio");
    let seen = announced_at(&sub_origin, path.as_str()).await;
    first_frame(&seen, "catalog.json").await;

    // Consuming: a browser's broadcast resolves through the relay.
    let pub_origin = moq_tokio::origin::spawn();
    let browser_broadcast = pub_origin
        .route("browser-stream", origin::RouteOptions::default())
        .expect("broadcast");
    let track = browser_broadcast
        .broadcast()
        .create_track("data", None)
        .expect("track");
    let mut group = track.append_group().expect("group");
    group
        .write_frame(None, b"from-the-browser".as_ref())
        .expect("write");
    group.finish().expect("finish");
    let _browser_publisher = established(
        noq_client()
            .with_publisher(pub_origin.consume())
            .connect(relay.url()),
    )
    .await;

    let subscription =
        tokio::time::timeout(TIMEOUT, node.subscribe("browser-stream", Reach::Relays))
            .await
            .expect("subscribe timeout")
            .expect("subscribe through the relay");
    let frame = first_frame(&subscription.broadcast(), "data").await;
    assert_eq!(&frame.payload[..], b"from-the-browser");
    let serving = subscription.serving_link().expect("a serving link");
    assert_eq!(serving.kind, LinkKind::Relay);
    // Detaching withdraws what the relay taught the route table.
    let mut updates = node.origin().announced();
    let route = tokio::time::timeout(TIMEOUT, async {
        loop {
            let update = updates.next().await.expect("origin closed");
            let announce = announce_of(&update);
            if announce.prefix.as_str() == "browser-stream" {
                return announce.route.clone();
            }
        }
    })
    .await
    .expect("the relay's route never reached the table");
    assert!(route.cost().value() >= 10, "{route:?}");
    link.detach().await;
    assert_eq!(link.status().get(), RelayStatus::Detached);
    retracted(&mut updates, "browser-stream").await;

    drop(broadcast);
    node.shutdown().await;
}

/// Binds a node for the relay-link tests.
///
/// It only dials. Mount it with [`serve`] to accept sessions.
async fn relay_node() -> (iroh::Endpoint, iroh_live::Node) {
    let endpoint = bind_endpoint().await;
    let node = iroh_live::Node::new(endpoint.clone(), LiveLayout::default().config(&[]));
    (endpoint, node)
}

/// Attaches `node` to `relay` and waits for the link to connect.
async fn attached(node: &iroh_live::Node, relay: &TestRelay) -> iroh_live_node::RelayLink {
    connected(node, RelayConfig::iroh(relay.iroh_id)).await
}

/// Attaches `node` to the relay `config` names and waits for the link to connect.
async fn connected(node: &iroh_live::Node, config: RelayConfig) -> iroh_live_node::RelayLink {
    let link = node.attach_relay(config).expect("attach");
    let mut status = link.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get() != RelayStatus::Connected {
            status.updated().await.expect("link gone");
        }
    })
    .await
    .expect("the relay link never connected");
    link
}

/// Writes a counter every few milliseconds into a track `name` of `broadcast`.
fn counter(broadcast: &moq_net::broadcast::Producer, name: &str) -> AbortOnDropHandle<()> {
    let mut track = broadcast
        .create_track(
            name,
            moq_net::track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    AbortOnDropHandle::new(tokio::spawn(async move {
        for n in 0u64.. {
            if track.write_frame(None, n.to_be_bytes().to_vec()).is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }))
}

/// Waits until `origin` has a route to `path`.
async fn routed(origin: &origin::Consumer, path: &str) {
    tokio::time::timeout(TIMEOUT, routed_broadcast(origin, path))
        .await
        .unwrap_or_else(|_| panic!("{path} was never routed"))
        .expect("the origin closed");
}

/// Gives a relay's route to a path that a direct route already serves time to arrive.
///
/// Waits until the relay routes `path`, so only the hop from the relay to the
/// node is left. The node's route table announces only the best route of a
/// path, so the relay's second route there cannot be waited for. The relay
/// link is connected, and a moment is plenty on loopback.
async fn relay_route_arrives(relay: &TestRelay, path: &str) {
    routed(&relay.cluster.origin.consume(), path).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// Waits until `updates` retracts `path`.
async fn retracted(updates: &mut moq_net::announce::Consumer, path: &str) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let update = updates.next().await.expect("origin closed");
            if matches!(update, Event::Absent(_)) && announce_of(&update).prefix.as_str() == path {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{path} was never retracted"));
}

/// Reports whether `origin` routes `path` within a second.
async fn routed_soon(origin: &origin::Producer, path: &str) -> bool {
    tokio::time::timeout(
        Duration::from_secs(1),
        routed_broadcast(&origin.consume(), path),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .is_some()
}

/// A relay-served subscription carries the relay link's readings to the player.
#[tokio::test]
async fn a_relay_served_subscription_carries_link_samples() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (_publisher_endpoint, publisher) = relay_node().await;
    let _publisher_link = attached(&publisher, &relay).await;
    let _broadcast = publish_video(&publisher, "studio");

    let (_viewer_endpoint, viewer) = relay_node().await;
    let link = attached(&viewer, &relay).await;
    let ticket = LiveLayout::default()
        .ticket(publisher.id(), "studio")
        .expect("a valid broadcast name");
    let subscription =
        tokio::time::timeout(TIMEOUT, viewer.subscribe(ticket.path(), Reach::Relays))
            .await
            .expect("subscribe timeout")
            .expect("subscribe through the relay");

    // A link serves a path once a reader asks it for a track.
    first_frame(&subscription.broadcast(), "catalog.json").await;

    // The transport: the relay link serves the path, and its monitor has
    // measured the link.
    let serving = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(serving) = subscription.serving_link()
                && serving.sample.network.rtt.is_some()
            {
                return serving;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the relay link never reported a round trip");
    assert_eq!(serving.kind, LinkKind::Relay);
    assert_eq!(serving.id, link.id());
    assert!(serving.sample.network.min_rtt.is_some(), "{serving:?}");

    // The facade: the player's network signals carry the same readings.
    let player = iroh_live::media::remote_broadcast(&subscription)
        .play(PlayerConfig::default())
        .expect("play");
    let network = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(network) = player.stats().network
                && network.rtt.is_some()
            {
                return network;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the player never saw the relay link's round trip");
    assert!(network.min_rtt.is_some(), "{network:?}");

    drop(player);
    viewer.shutdown().await;
    publisher.shutdown().await;
}

/// Only granted publications reach a relay.
#[tokio::test]
async fn a_relay_gets_public_publications_only() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (endpoint, node) = relay_node().await;
    // The relay's own id in the set: a relay forwards to anyone, so it never
    // sees a `Peers` publication, even one that names it.
    let members = Watchable::new(BTreeSet::from([relay.iroh_id]));
    let public = node
        .publish(live_path(node.id(), "public"), Audience::Granted)
        .expect("publish");
    let _public = counter(public.broadcast(), "data");
    let peers = node
        .publish(
            live_path(node.id(), "peers"),
            Audience::Peers(members.clone()),
        )
        .expect("publish");
    let _peers = counter(peers.broadcast(), "data");

    let sub_origin = moq_tokio::origin::spawn();
    let _browser = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    let _public_link = attached(&node, &relay).await;
    announced_at(&sub_origin, public.path().as_str()).await;
    assert!(
        !routed_soon(&sub_origin, peers.path().as_str()).await,
        "a Peers publication reached the relay"
    );

    node.shutdown().await;
    drop(endpoint);
}

/// Shutting a node down detaches its relay links.
#[tokio::test]
async fn shutdown_detaches_relay_links() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (_endpoint, node) = relay_node().await;
    let link = attached(&node, &relay).await;
    let consume_only = node
        .attach_relay(RelayConfig {
            consume: false,
            ..RelayConfig::iroh(relay.iroh_id)
        })
        .expect("attach");

    link.detach().await;
    // The detached link feeds nothing, so a relay subscribe fails.
    let err = tokio::time::timeout(TIMEOUT, node.subscribe("anything", Reach::Relays))
        .await
        .expect("a relay subscribe with no consuming relay waited")
        .expect_err("resolved without a relay");
    assert!(matches!(err, Error::NoRoute { .. }), "{err:#}");

    let mut status = consume_only.status();
    tokio::time::timeout(TIMEOUT, node.shutdown())
        .await
        .expect("shutdown hung");
    tokio::time::timeout(TIMEOUT, async {
        while status.get() != RelayStatus::Detached {
            status.updated().await.expect("link gone");
        }
    })
    .await
    .expect("a relay link outlived its node's shutdown");
}

/// A subscription ends with its direct session, and asking again finds the relay.
///
/// It does not move over on its own, since a relay route cannot vouch for the publisher.
#[tokio::test]
async fn losing_the_direct_session_moves_a_subscription_to_the_relay() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (_alice_endpoint, alice) = relay_node().await;
    let _alice_router = serve(&alice);
    let publication = alice
        .publish(live_path(alice.id(), "cam"), Audience::Granted)
        .expect("publish");
    let _writer = counter(publication.broadcast(), "data");
    let _alice_link = attached(&alice, &relay).await;

    let (_bob_endpoint, bob) = relay_node().await;
    let subscription = tokio::time::timeout(
        TIMEOUT,
        bob.subscribe(publication.path(), Reach::Direct(alice.endpoint().id())),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe");
    let mut reader = subscribed(&subscription.broadcast()).await;
    let _bob_link = attached(&bob, &relay).await;
    relay_route_arrives(&relay, publication.path().as_str()).await;

    subscription
        .session()
        .expect("served directly")
        .close(moq_net::Error::Cancel);
    tokio::time::timeout(TIMEOUT, async {
        while let Ok(Some(_)) = reader.recv_group().await {}
    })
    .await
    .expect("the subscription outlived its direct session");

    let again = tokio::time::timeout(TIMEOUT, bob.subscribe(publication.path(), Reach::Relays))
        .await
        .expect("subscribe timeout")
        .expect("subscribe through the relay");
    subscribed(&again.broadcast()).await;
    assert_eq!(
        again.serving_link().map(|link| link.kind),
        Some(LinkKind::Relay),
        "not served by the relay"
    );

    bob.shutdown().await;
    alice.shutdown().await;
}

/// Subscribes to the counter track of `broadcast` and reads one group.
async fn subscribed(broadcast: &moq_net::broadcast::Consumer) -> moq_net::track::Subscriber {
    let mut reader = tokio::time::timeout(
        TIMEOUT,
        broadcast.track("data").expect("track").subscribe(
            moq_net::track::Subscription::default()
                .with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(5))),
        ),
    )
    .await
    .expect("track subscribe timeout")
    .expect("track subscribe");
    tokio::time::timeout(TIMEOUT, reader.recv_group())
        .await
        .expect("no group")
        .expect("track failed")
        .expect("track ended");
    reader
}

/// Returns the hop iroh-live-node derives for `id`, which anyone can compute.
fn hop_of(id: iroh::EndpointId) -> std::num::NonZeroU64 {
    let bytes: [u8; 8] = id.as_bytes()[..8].try_into().expect("32 bytes");
    let value = u64::from_le_bytes(bytes) & ((1u64 << 53) - 1);
    std::num::NonZeroU64::new(value.max(1)).expect("a valid hop")
}

/// A forgery of Alice's path under her hop never reaches Bob's direct subscription.
///
/// Not even once the direct session goes. The test relay lets anyone publish anywhere.
#[tokio::test]
async fn a_relay_cannot_splice_a_forgery_into_a_direct_subscription() {
    const FORGED: u64 = 1_000_000;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (alice_endpoint, alice) = relay_node().await;
    let _alice_router = serve(&alice);
    let publication = alice
        .publish(live_path(alice.id(), "cam"), Audience::Granted)
        .expect("publish");
    let _writer = counter(publication.broadcast(), "data");

    // Mallory, a browser-like client, publishes at Alice's path, declaring
    // Alice's hop.
    let (mallory, mallory_driver) =
        origin::Producer::new(origin::Config::new(hop_of(alice_endpoint.id())));
    let _mallory_driver = AbortOnDropHandle::new(tokio::spawn(async move {
        moq_net::time::run(mallory_driver).await;
    }));
    let forged = mallory
        .route(publication.path().as_str(), origin::RouteOptions::default())
        .expect("forged broadcast");
    let mut forged_track = forged
        .broadcast()
        .create_track(
            "data",
            moq_net::track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    let _forger = AbortOnDropHandle::new(tokio::spawn(async move {
        for n in FORGED.. {
            if forged_track
                .write_frame(None, n.to_be_bytes().to_vec())
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }));
    let _mallory_session = established(
        noq_client()
            .with_publisher(mallory.consume())
            .connect(relay.url()),
    )
    .await;

    let (_bob_endpoint, bob) = relay_node().await;
    let subscription = tokio::time::timeout(
        TIMEOUT,
        bob.subscribe(publication.path(), Reach::Direct(alice.endpoint().id())),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe");
    let mut reader = subscribed(&subscription.broadcast()).await;
    let _bob_link = attached(&bob, &relay).await;
    relay_route_arrives(&relay, publication.path().as_str()).await;

    subscription
        .session()
        .expect("served directly")
        .close(moq_net::Error::Cancel);
    let read = tokio::time::timeout(TIMEOUT, async {
        loop {
            let Ok(Some(mut group)) = reader.recv_group().await else {
                return;
            };
            if let Ok(Some(frame)) = group.read_frame().await {
                let bytes: [u8; 8] = frame.payload[..].try_into().expect("a u64");
                assert!(
                    u64::from_be_bytes(bytes) < FORGED,
                    "the forged broadcast was spliced into the subscription"
                );
            }
        }
    })
    .await;
    assert!(read.is_ok(), "the subscription outlived its direct session");

    bob.shutdown().await;
    alice.shutdown().await;
}

/// The shipped relay keeps iroh clients to their own paths, browsers to one segment.
#[tokio::test]
async fn the_shipped_relay_refuses_forged_paths() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_shipped().await;
    let sub_origin = moq_tokio::origin::spawn();
    let _viewer = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    let alice = iroh::SecretKey::generate().public();
    let (mallory_endpoint, mallory) = relay_node().await;
    let own = mallory
        .publish(
            LiveLayout::default()
                .path(mallory.id(), "cam")
                .expect("a path"),
            Audience::Granted,
        )
        .expect("publish");
    let _own = counter(own.broadcast(), "data");
    let forged_path = format!("live/{alice}/cam");
    let room_path = format!("rooms/topic/{alice}/cam");
    let forged = mallory
        .publish(forged_path.as_str(), Audience::Granted)
        .expect("publish at alice's path");
    let _forged = counter(forged.broadcast(), "data");
    let room_forged = mallory
        .publish(room_path.as_str(), Audience::Granted)
        .expect("publish at alice's room path");
    let _room_forged = counter(room_forged.broadcast(), "data");
    let _link = attached(&mallory, &relay).await;

    // A browser tries the same, and publishes a name of its own.
    let browser = moq_tokio::origin::spawn();
    let browser_forged = browser
        .route(
            format!("live/{alice}/screen").as_str(),
            origin::RouteOptions::default(),
        )
        .expect("broadcast");
    let browser_own = browser
        .route("browser-stream", origin::RouteOptions::default())
        .expect("broadcast");
    let _browser_session = established(
        noq_client()
            .with_publisher(browser.consume())
            .connect(relay.url()),
    )
    .await;

    announced_at(&sub_origin, own.path().as_str()).await;
    announced_at(&sub_origin, "browser-stream").await;
    for path in [forged_path, room_path, format!("live/{alice}/screen")] {
        assert!(
            !routed_soon(&sub_origin, &path).await,
            "the relay took a broadcast at {path} from someone it does not name"
        );
    }

    // And an iroh node reads through it, over the relay's own acceptor.
    let (_viewer_endpoint, viewer) = relay_node().await;
    let _viewer_link = attached(&viewer, &relay).await;
    let track = browser_own
        .broadcast()
        .create_track("data", None)
        .expect("track");
    let mut group = track.append_group().expect("group");
    group
        .write_frame(None, b"from-the-browser".as_ref())
        .expect("write");
    group.finish().expect("finish");
    let subscription = tokio::time::timeout(
        TIMEOUT,
        viewer.subscribe("browser-stream", iroh_live_node::Reach::Relays),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe through the relay");
    let frame = first_frame(&subscription.broadcast(), "data").await;
    assert_eq!(&frame.payload[..], b"from-the-browser");

    drop((browser_forged, browser_own));
    viewer.shutdown().await;
    mallory.shutdown().await;
    drop(mallory_endpoint);
}

/// Two pulls of one publisher share a session, which closes with the last.
#[tokio::test]
async fn a_publisher_session_outlives_all_but_its_last_pull() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, _router, first_broadcast, first) = start_publisher("first").await;
    let second_broadcast = publish_video(&publisher, "second");
    let second = LiveLayout::default()
        .ticket(pub_ep.id(), "second")
        .expect("a valid broadcast name");
    let (first_name, second_name) = (first.to_string(), second.to_string());

    let pull_ep = bind_endpoint().await;
    let pull_id = pull_ep.id();
    let pull_state = iroh_live_relay::pull::PullState::new(pull_ep, relay.cluster.clone())
        .with_linger(PULL_LINGER);
    let first_guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&first_name, &first))
        .await
        .expect("pull timeout")
        .expect("pull");
    let second_guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&second_name, &second))
        .await
        .expect("pull timeout")
        .expect("pull");
    for name in [&first_name, &second_name] {
        assert!(
            wait_for_broadcast(&pulled(&relay.cluster, name), name, true).await,
            "{name} was never mirrored"
        );
    }
    let pull_sessions = || {
        publisher
            .sessions()
            .get()
            .iter()
            .filter(|session| session.peer() == pull_id)
            .count()
    };
    assert_eq!(
        pull_sessions(),
        1,
        "two pulls of one publisher, two sessions"
    );

    drop(first_guard);
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &first_name), &first_name, false).await,
        "the first pull was not retired"
    );
    let mirrored = routed_broadcast(
        &pulled(&relay.cluster, &second_name).origin.consume(),
        second_name.as_str(),
    )
    .await
    .expect("the second pull's mirror");
    first_frame(&mirrored, "catalog.json").await;
    assert_eq!(
        pull_sessions(),
        1,
        "retiring one pull closed the session the other reads"
    );
    drop(mirrored);

    drop(second_guard);
    session_closed(&publisher, pull_id, "the session outlived the last pull").await;

    drop((first_broadcast, second_broadcast));
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A pull lets none of the publisher's other broadcasts into the cluster, not even while it dials.
#[tokio::test]
async fn a_pull_keeps_the_publishers_other_broadcasts_out() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, _router, wanted_broadcast, wanted) = start_publisher("wanted").await;
    let other_broadcast = publish_video(&publisher, "other");
    let other = live_path(publisher.id(), "other");

    // Every path the cluster announces, from before the dial on.
    let cluster = relay.cluster.origin.consume();
    let mut announced = cluster.announced();
    let pull_state =
        iroh_live_relay::pull::PullState::new(bind_endpoint().await, relay.cluster.clone())
            .with_linger(PULL_LINGER);
    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&wanted.to_string(), &wanted))
        .await
        .expect("pull timeout")
        .expect("pull");

    let mut seen = Vec::new();
    let mut settle = None;
    loop {
        let deadline = settle.unwrap_or_else(|| tokio::time::Instant::now() + TIMEOUT);
        match tokio::time::timeout_at(deadline, announced.next()).await {
            Ok(Some(event)) => {
                let path = announce_of(&event).prefix.as_str().to_owned();
                if path == wanted.path().as_str() && settle.is_none() {
                    // Give anything announced alongside it time to arrive.
                    settle = Some(tokio::time::Instant::now() + Duration::from_millis(500));
                }
                seen.push(path);
            }
            Ok(None) => panic!("the cluster closed"),
            Err(_) if settle.is_some() => break,
            Err(_) => panic!("the pulled broadcast was never announced, saw {seen:?}"),
        }
    }
    assert!(
        !seen.contains(&other),
        "the publisher's other broadcast entered the cluster: {seen:?}"
    );

    drop(guard);
    drop((wanted_broadcast, other_broadcast));
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// Binds a node with rooms that accepts both MoQ and the rooms' gossip.
async fn room_node() -> (
    iroh::Endpoint,
    iroh_live_node::Node,
    iroh_live_rooms::Rooms,
    iroh::protocol::Router,
) {
    let endpoint = bind_endpoint().await;
    let node = iroh_live_node::Node::new(
        endpoint.clone(),
        LiveLayout::default().config(&[Arc::new(iroh_live_rooms::RoomsLayout)]),
    );
    let rooms = iroh_live_rooms::Rooms::new(&node);
    let router = rooms
        .mount(node.mount(iroh::protocol::Router::builder(endpoint.clone())))
        .spawn();
    (endpoint, node, rooms, router)
}

/// A forged room broadcast that a relay routes never reaches the room.
///
/// The room reads each member over its own session, even with the forgery routed first.
#[tokio::test]
async fn a_relay_cannot_forge_a_room_members_broadcast() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (alice_endpoint, alice_node, alice_rooms, alice_router) = room_node().await;
    let (bob_endpoint, bob_node, bob_rooms, bob_router) = room_node().await;
    let room_a = alice_rooms
        .join(
            &iroh_live_rooms::RoomTicket::generate(),
            Some("alice".into()),
        )
        .await
        .expect("join");
    let topic = room_a.ticket().topic_id();
    let cam_path = format!("rooms/{topic}/{}/cam", bob_endpoint.id());

    // Mallory publishes Bob's camera into the relay before Bob is there.
    let mallory = moq_tokio::origin::spawn();
    let forged = mallory
        .route(cam_path.as_str(), origin::RouteOptions::default())
        .expect("forged broadcast");
    let mut track = forged
        .broadcast()
        .create_track(
            "data",
            moq_net::track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    let _forger = AbortOnDropHandle::new(tokio::spawn(async move {
        while track.write_frame(None, &b"forged"[..]).is_ok() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }));
    let _mallory_session = established(
        noq_client()
            .with_publisher(mallory.consume())
            .connect(relay.url()),
    )
    .await;

    // Alice consumes through the relay, and the forged route is in her table.
    let link = attached(&alice_node, &relay).await;
    routed(&alice_node.origin(), cam_path.as_str()).await;

    let room_b = bob_rooms
        .join(&room_a.ticket(), Some("bob".into()))
        .await
        .expect("join");
    let cam: moq_net::broadcast::Producer = room_b.publish("cam").expect("publish");
    let _writer = counter(&cam, "data");
    let subscription = tokio::time::timeout(TIMEOUT, room_a.subscribe(bob_endpoint.id(), "cam"))
        .await
        .expect("subscribe timeout")
        .expect("subscribe");
    let frame = first_frame(&subscription.broadcast(), "data").await;
    assert_ne!(
        &frame.payload[..],
        b"forged",
        "the relay's forgery reached the room"
    );

    room_b.leave().await;
    room_a.leave().await;
    bob_node.shutdown().await;
    alice_node.shutdown().await;
    bob_router.shutdown().await.expect("router");
    alice_router.shutdown().await.expect("router");
    drop((alice_endpoint, link));
}

/// A relay link over QUIC, through moq-tokio, publishes one node's broadcast to another.
///
/// Both nodes dial `http://`, which pins the relay's self-signed certificate.
/// `https://` and `moqt://` take the same moq-tokio path, but verify against
/// the system's roots, which a test certificate is not in.
#[tokio::test]
async fn a_relay_link_over_quic_carries_a_broadcast() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_http().await;

    let (_publisher_endpoint, publisher) = relay_node().await;
    let _publisher_link = connected(
        &publisher,
        RelayConfig {
            consume: false,
            ..RelayConfig::new(relay.http_url())
        },
    )
    .await;
    let _broadcast = publish_video(&publisher, "studio");
    let path = live_path(publisher.id(), "studio");
    routed(&relay.cluster.origin.consume(), &path).await;

    let (_viewer_endpoint, viewer) = relay_node().await;
    let link = connected(&viewer, RelayConfig::new(relay.http_url())).await;
    let subscription = tokio::time::timeout(TIMEOUT, viewer.subscribe(path, Reach::Relays))
        .await
        .expect("subscribe timeout")
        .expect("subscribe through the relay");
    first_frame(&subscription.broadcast(), "catalog.json").await;
    let serving = subscription.serving_link().expect("a serving link");
    assert_eq!(serving.kind, LinkKind::Relay);
    assert_eq!(serving.id, link.id());

    viewer.shutdown().await;
    publisher.shutdown().await;
}

/// Returns a key set of one fresh signing key.
fn signing_keys() -> KeySet {
    let key = Key::generate(Algorithm::HS256, None).expect("a key");
    KeySet {
        keys: vec![Arc::new(key)],
    }
}

/// Signs a token that lets its holder publish under `prefix/` and read everything.
fn token_for(keys: &KeySet, prefix: &str) -> String {
    let claims = Claims::default()
        .with_publish([format!("{prefix}/**").parse().expect("a pattern")])
        .with_subscribe([moq_net::Pattern::all()]);
    keys.sign(&claims).expect("signed")
}

/// Attaches `node` to `relay` with `token`.
fn attach_with_token(
    node: &iroh_live::Node,
    relay: &TestRelay,
    token: Option<String>,
) -> iroh_live_node::RelayLink {
    node.attach_relay(RelayConfig {
        token: token.map(iroh_live_node::TokenFn::fixed),
        ..RelayConfig::iroh(relay.iroh_id)
    })
    .expect("attach")
}

/// With a key, an iroh client's valid token decides what it publishes, and one without a token keeps the relay's rule.
#[tokio::test]
async fn an_iroh_client_with_a_valid_token_publishes_where_it_allows() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;

    let (_endpoint, holder) = relay_node().await;
    let granted = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _granted = counter(granted.broadcast(), "data");
    let own = holder
        .publish(
            LiveLayout::default()
                .path(holder.id(), "cam")
                .expect("a path"),
            Audience::Granted,
        )
        .expect("publish");
    let _own = counter(own.broadcast(), "data");
    let _link = attach_with_token(&holder, &relay, Some(token_for(&keys, "tokens")));
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    assert!(
        !routed_soon(&relay.cluster.origin, own.path().as_str()).await,
        "the token replaced the relay's rule, so the client's own path is outside it"
    );

    // Without a token, the relay's own rule applies.
    let (_plain_endpoint, plain) = relay_node().await;
    let plain_own = plain
        .publish(
            LiveLayout::default()
                .path(plain.id(), "cam")
                .expect("a path"),
            Audience::Granted,
        )
        .expect("publish");
    let _plain_own = counter(plain_own.broadcast(), "data");
    let plain_tokens = plain
        .publish("tokens/screen", Audience::Granted)
        .expect("publish");
    let _plain_tokens = counter(plain_tokens.broadcast(), "data");
    let _plain_link = attach_with_token(&plain, &relay, None);
    routed(&relay.cluster.origin.consume(), plain_own.path().as_str()).await;
    assert!(!routed_soon(&relay.cluster.origin, "tokens/screen").await);

    holder.shutdown().await;
    plain.shutdown().await;
}

/// With a key, an iroh client whose token the keys reject is refused, not narrowed.
#[tokio::test]
async fn an_iroh_client_with_a_bad_token_is_refused() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_keyed(signing_keys()).await;
    let (_endpoint, forger) = relay_node().await;
    let own = forger
        .publish(
            LiveLayout::default()
                .path(forger.id(), "cam")
                .expect("a path"),
            Audience::Granted,
        )
        .expect("publish");
    let _own = counter(own.broadcast(), "data");
    // Signed by a key the relay does not hold.
    let _link = attach_with_token(&forger, &relay, Some(token_for(&signing_keys(), "tokens")));
    assert!(
        !routed_soon(&relay.cluster.origin, own.path().as_str()).await,
        "a session with a forged token was admitted"
    );
    forger.shutdown().await;
}

/// With a key, a browser's signed token decides what it publishes, and a bad one is refused.
#[tokio::test]
async fn a_browser_with_a_signed_token_publishes_where_it_allows() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;

    let browser = moq_tokio::origin::spawn();
    // Two segments, which the public rule refuses.
    let _cam = browser
        .route("studio/cam", origin::RouteOptions::default())
        .expect("broadcast");
    let mut url = relay.url();
    url.query_pairs_mut()
        .append_pair("jwt", &token_for(&keys, "studio"));
    let _session = established(noq_client().with_publisher(browser.consume()).connect(url)).await;
    routed(&relay.cluster.origin.consume(), "studio/cam").await;

    // A forged token: the session must not publish, even a name the public
    // rule would allow.
    let forger = moq_tokio::origin::spawn();
    let _screen = forger
        .route("screen", origin::RouteOptions::default())
        .expect("broadcast");
    let _studio = forger
        .route("studio/screen", origin::RouteOptions::default())
        .expect("broadcast");
    let mut forged = relay.url();
    forged
        .query_pairs_mut()
        .append_pair("jwt", &token_for(&signing_keys(), "studio"));
    let forged = noq_client()
        .with_publisher(forger.consume())
        .connect(forged);
    // The client's half of the handshake may complete before the refusal.
    // Held, since dropping the connection would close the session.
    let _forged = tokio::time::timeout(TIMEOUT, forged.established()).await;
    for path in ["screen", "studio/screen"] {
        assert!(
            !routed_soon(&relay.cluster.origin, path).await,
            "a session with a forged token published {path}"
        );
    }
    drop(browser);
}

/// With a key, an iroh client's session closes when its token expires, and its broadcast leaves.
#[tokio::test]
async fn an_iroh_clients_session_closes_when_its_token_expires() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;
    let mut claims = Claims::default()
        .with_publish(["tokens/**".parse().expect("a pattern")])
        .with_subscribe([moq_net::Pattern::all()]);
    claims.expires = Some(
        (std::time::SystemTime::now() + Duration::from_secs(2))
            .try_into()
            .expect("after 1970"),
    );
    let token = keys.sign(&claims).expect("signed");

    let (_endpoint, holder) = relay_node().await;
    let cam = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _cam = counter(cam.broadcast(), "data");
    let mut updates = relay.cluster.origin.consume().announced();
    let _link = attach_with_token(&holder, &relay, Some(token));
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    // The redial presents the same expired token, which the relay refuses.
    retracted(&mut updates, "tokens/cam").await;
    assert!(!routed_soon(&relay.cluster.origin, "tokens/cam").await);

    holder.shutdown().await;
}

/// An iroh relay link asks its token function on every dial, so a redial presents a fresh token.
#[tokio::test]
async fn a_relay_link_presents_a_fresh_token_when_it_redials() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;
    // The first token expires, so the relay closes the session, and every later one lasts.
    let mut expiring = Claims::default()
        .with_publish(["tokens/**".parse().expect("a pattern")])
        .with_subscribe([moq_net::Pattern::all()]);
    expiring.expires = Some(
        (std::time::SystemTime::now() + Duration::from_secs(2))
            .try_into()
            .expect("after 1970"),
    );
    let first = keys.sign(&expiring).expect("signed");
    let fresh = token_for(&keys, "tokens");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let token = iroh_live_node::TokenFn::new(move || {
        let call = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(if call == 0 {
            first.clone()
        } else {
            fresh.clone()
        })
    });

    let (_endpoint, holder) = relay_node().await;
    let cam = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _cam = counter(cam.broadcast(), "data");
    let mut updates = relay.cluster.origin.consume().announced();
    let _link = holder
        .attach_relay(RelayConfig {
            token: Some(token),
            ..RelayConfig::iroh(relay.iroh_id)
        })
        .expect("attach");
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    retracted(&mut updates, "tokens/cam").await;
    // Back with the fresh token, which the relay verified on the redial.
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    assert!(
        calls.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "the link asked for a token once only"
    );

    holder.shutdown().await;
}
