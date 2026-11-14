//! The relay's bridging, pulls and admission, over real connections.
//!
//! Endpoints use `presets::Minimal` and a shared `MemoryLookup`, so no test
//! needs network discovery. Each test binds its own relay and endpoints, on
//! random ports and with fresh keys, and each endpoint has its own entry in
//! the lookup. The tests therefore share nothing and run in parallel.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use iroh::protocol::AcceptError;
use iroh_live::{
    layout::LiveLayout,
    media::{PlayerConfig, SubscriptionExt, VideoEncoding, VideoRendition, VideoSource, video},
};
use iroh_live_node::{
    Audience, Error, LinkKind, Reach, RelayConfig, RelayStatus,
    test_util::{TIMEOUT, TestNode, announcement, endpoint, lookup, retracted, step},
};
use iroh_live_relay::pull::{Access, Client};
use moq_auth::{Algorithm, Claims, Key, KeySet};
use moq_ext_access::policy::Verify;
use moq_net::origin;
use moq_relay::cluster::Cluster;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tracing::{debug, info};

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
    /// The file the browser admission reads its JWT keys from, if it verifies JWTs.
    keys_file: Option<KeysFile>,
}

/// A JWT key file in the temp dir, removed on drop.
struct KeysFile(std::path::PathBuf);

impl KeysFile {
    /// Writes `keys` to a new file.
    fn new(keys: &KeySet) -> Self {
        let path = std::env::temp_dir().join(format!(
            "iroh-live-relay-test-keys-{}.json",
            iroh::SecretKey::generate().public()
        ));
        keys.to_file(&path).expect("writes the keys");
        Self(path)
    }

    /// Replaces the keys in the file, as an operator rotating them would.
    fn rotate(&self, keys: &KeySet) {
        keys.to_file(&self.0).expect("writes the keys");
    }
}

impl Drop for KeysFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

impl TestRelay {
    /// Starts a relay that lets anyone publish anywhere.
    ///
    /// The forgery tests use it as the relay a node must not trust.
    async fn start() -> Self {
        Self::start_with(false, None, None, None, iroh::SecretKey::generate()).await
    }

    /// Starts a relay with the admission `iroh_live_relay::run` uses.
    async fn start_shipped() -> Self {
        Self::start_with(true, None, None, None, iroh::SecretKey::generate()).await
    }

    /// Starts a relay as [`start_shipped`](Self::start_shipped) does, which verifies JWTs with `keys`.
    ///
    /// As `iroh_live_relay::run` does with `--auth-key`.
    async fn start_keyed(keys: KeySet) -> Self {
        Self::start_with(true, None, None, Some(keys), iroh::SecretKey::generate()).await
    }

    /// Starts a relay as [`start_keyed`](Self::start_keyed) does, with HTTP.
    ///
    /// The relay is reachable through [`http_url`](Self::http_url).
    async fn start_keyed_http(keys: KeySet) -> Self {
        Self::start_with(
            true,
            None,
            Some(free_port()),
            Some(keys),
            iroh::SecretKey::generate(),
        )
        .await
    }

    /// Starts a relay as [`start_shipped`](Self::start_shipped) does, which also pulls tickets.
    ///
    /// A session that names a ticket in its path pulls the ticket's broadcast,
    /// token or not, as `--open-pulls` does.
    async fn start_pulling() -> Self {
        Self::start_with(
            true,
            Some(Access::Open),
            None,
            None,
            iroh::SecretKey::generate(),
        )
        .await
    }

    /// Starts a relay as [`start_keyed`](Self::start_keyed) does, which pulls for a token holder.
    ///
    /// As `iroh_live_relay::run` does with `--auth-key` and no `--open-pulls`.
    async fn start_keyed_pulling(keys: KeySet) -> Self {
        Self::start_with(
            true,
            Some(Access::Authorized),
            None,
            Some(keys),
            iroh::SecretKey::generate(),
        )
        .await
    }

    /// Starts a relay with the admission of the shipped relay if `shipped`, else an open one.
    ///
    /// Starts a relay that lets anyone publish anywhere, and serves its
    /// certificate's fingerprint over HTTP on the port of its QUIC listener.
    ///
    /// A client dials it as `http://localhost:<port>`: moq-tokio fetches the
    /// fingerprint there and pins the self-signed certificate.
    async fn start_http() -> Self {
        Self::start_with(
            false,
            None,
            Some(free_port()),
            None,
            iroh::SecretKey::generate(),
        )
        .await
    }

    /// Starts a relay with the given options.
    ///
    /// With `pulls`, the shipped admission pulls the tickets sessions name, for the sessions it lets.
    /// With `port`, QUIC and the web listener bind that port, else QUIC binds any.
    /// With `keys`, the shipped admission and the iroh sessions verify JWTs.
    /// The relay's iroh endpoint id is the one of `secret_key`.
    async fn start_with(
        shipped: bool,
        pulls: Option<Access>,
        port: Option<u16>,
        keys: Option<KeySet>,
        secret_key: iroh::SecretKey,
    ) -> Self {
        let alpns = moq_transport_iroh::alpns()
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();

        let iroh = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .address_lookup(lookup().clone())
            .secret_key(secret_key)
            .alpns(alpns)
            .bind()
            .await
            .expect("bind relay iroh");

        lookup().add_endpoint_info(iroh.addr());
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
        let keys_file = keys.as_ref().map(KeysFile::new);
        let mut relay = moq_relay::Relay::load(config)
            .await
            .expect("load relay")
            .with_signals(false);
        let cluster = relay.cluster().clone();
        let noq_addr = relay.quic_addr().expect("a quic listener");

        // moq-tokio's server has no iroh backend, so iroh clients always come
        // in through a handler: the shipped admission, or one that admits all.
        let (iroh_router, admit_task) = if shipped {
            let pulls = pulls.map(|access| {
                iroh_live_relay::pull::Pulls::new(iroh.clone(), cluster.clone())
                    .with_linger(PULL_LINGER)
                    .with_access(access)
            });
            // As the shipped relay does: a browser session that names a
            // ticket reads the pulled broadcast at that name.
            let admissions = relay.admissions().expect("no auth source configured");
            let source = keys_file
                .as_ref()
                .map(|file| moq_auth::serve::Keys::Set(file.0.clone()));
            let admit_task = AbortOnDropHandle::new(tokio::spawn(iroh_live_relay::admit(
                admissions,
                pulls.clone(),
                source,
            )));
            let mut sessions = iroh_live_relay::IrohSessions::new(cluster.clone(), pulls);
            if let Some(file) = &keys_file {
                // As `iroh_live_relay::run` does: the key file, read again when it changes.
                let verifier = iroh_live_relay::KeyFile::load(&file.0).expect("reads the keys");
                sessions = sessions
                    .with_policy(
                        Verify::new(verifier.clone())
                            .with_fallback(iroh_live_relay::relay_policy()),
                    )
                    .with_pull_policy(Verify::new(verifier));
            }
            let router = sessions
                .mount(iroh::protocol::Router::builder(iroh))
                .spawn();
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
            keys_file,
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

/// A protocol handler that lets every iroh client publish and subscribe anywhere.
///
/// The relay the forgery tests use as one a node must not trust.
#[derive(derive_more::Debug, Clone)]
struct OpenSessions(#[debug(skip)] Cluster);

impl iroh::protocol::ProtocolHandler for OpenSessions {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        let transport =
            moq_transport_iroh::Transport::new(connection).map_err(AcceptError::from_err)?;
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
        debug!(error = %err, "iroh session ended");
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
    step("connect timeout", connection.established())
        .await
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
    let path = announcement(&update).0.prefix.as_str().to_owned();
    let broadcast = step(
        "resolve timeout",
        routed_broadcast(&consumer, path.as_str()),
    )
    .await
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
    let mut subscriber = step("subscribe timeout", track.subscribe(None))
        .await
        .expect("subscribe");
    let mut group = step("group timeout", subscriber.recv_group())
        .await
        .expect("group err")
        .expect("group closed");
    step("frame timeout", group.read_frame())
        .await
        .expect("frame err")
        .expect("frame closed")
}

/// One end of a connection to the relay.
#[derive(Debug, Clone, Copy)]
enum End {
    /// A moq-tokio client over noq, as a browser dials.
    Noq,
    /// An iroh-live node.
    Iroh,
}

/// The hang catalog a noq publisher writes: one 320x240 H.264 rendition.
const NOQ_CATALOG: &[u8] = br#"{"video":{"renditions":{"video/h264":{"codec":"avc1.64001f","codedWidth":320,"codedHeight":240,"bitrate":500000,"framerate":30}}}}"#;

/// A broadcast crosses the relay from either kind of publisher to either kind of subscriber.
#[tokio::test]
async fn the_relay_bridges_noq_and_iroh_both_ways() {
    let _ = tracing_subscriber::fmt::try_init();
    for publisher in [End::Noq, End::Iroh] {
        for subscriber in [End::Noq, End::Iroh] {
            info!(?publisher, ?subscriber, "bridging through a new relay");
            bridge(publisher, subscriber).await;
        }
    }
}

/// Publishes into a new relay from `publisher`, and reads the broadcast back from `subscriber`.
async fn bridge(publisher: End, subscriber: End) {
    let relay = TestRelay::start().await;
    let (path, published) = publish_into(&relay, publisher).await;
    routed(&relay.cluster.origin.consume(), &path).await;

    match subscriber {
        End::Noq => {
            let sub_origin = moq_tokio::origin::spawn();
            let _session = established(
                noq_client()
                    .with_subscriber(sub_origin.clone())
                    .connect(relay.url()),
            )
            .await;
            let (announced, broadcast) = next_announced(&sub_origin, &path).await;
            assert_eq!(announced, path);
            let catalog = first_frame(&broadcast, "catalog.json").await;
            if let End::Noq = publisher {
                assert_eq!(&catalog.payload[..], NOQ_CATALOG);
            }
        }
        End::Iroh => {
            let sub_ep = endpoint().await;
            let node = iroh_live::Node::new(sub_ep.clone(), trusting());
            let broadcast = step("subscribe timeout", async {
                let session = node.connect(relay.iroh_id).await?;
                let subscription = session.subscribe(path.as_str()).await?;
                Ok::<_, iroh_live::node::Error>(subscription.remote_broadcast())
            })
            .await
            .expect("subscribe");

            // The catalog arriving and parsing proves the bridge carried the
            // broadcast itself, not only its route.
            let mut catalog = broadcast.catalog();
            let parsed = step("no catalog arrived", catalog.wait_for(Option::is_some))
                .await
                .expect("the broadcast ended")
                .clone()
                .expect("a catalog");
            let video = &parsed.video.renditions;
            assert_eq!(video.len(), 1, "the bridged catalog: {video:?}");
            match publisher {
                End::Noq => assert_eq!(video["video/h264"].coded_height, Some(240)),
                End::Iroh => {
                    let player = broadcast.play(PlayerConfig::default()).expect("play");
                    let frame = step("timeout", player.video_frames().next())
                        .await
                        .expect("closed");
                    let size = frame.size();
                    assert!(size.width > 0 && size.height > 0);
                }
            }
            drop(broadcast);
            node.shutdown().await;
            sub_ep.close().await;
        }
    }
    drop(published);
}

/// Publishes a broadcast into `relay` from a client of kind `end`.
///
/// Returns the broadcast's path, and what keeps it published.
async fn publish_into(relay: &TestRelay, end: End) -> (String, Box<dyn std::any::Any>) {
    match end {
        End::Noq => {
            let origin = moq_tokio::origin::spawn();
            let broadcast = origin
                .route("browser-stream", origin::RouteOptions::default())
                .expect("broadcast");
            // The track producers stay alive with the broadcast, or the tracks end.
            let tracks: Vec<_> = [
                ("catalog.json", NOQ_CATALOG),
                ("video/h264", b"keyframe-data".as_slice()),
            ]
            .into_iter()
            .map(|(name, payload)| {
                let track = broadcast
                    .broadcast()
                    .create_track(name, None)
                    .expect("track");
                let mut group = track.append_group().expect("group");
                group.write_frame(None, payload).expect("write");
                group.finish().expect("finish");
                track
            })
            .collect();
            let session = established(
                noq_client()
                    .with_publisher(origin.consume())
                    .connect(relay.url()),
            )
            .await;
            (
                "browser-stream".to_owned(),
                Box::new((origin, broadcast, tracks, session)),
            )
        }
        End::Iroh => {
            let node = TestNode::spawn(LiveLayout::default().config()).await;
            let broadcast = publish_video(&node.node, "relay-test");
            let session = step("timeout", node.node.connect(relay.iroh_id))
                .await
                .expect("connect");
            // The path that names the publisher, as on a direct session.
            let path = live_path(node.id(), "relay-test");
            (path, Box::new((node, broadcast, session)))
        }
    }
}

/// A noq subscriber reads a broadcast the relay pulls from a ticket.
#[tokio::test]
async fn pull_remote_broadcast_via_ticket() {
    let _ = tracing_subscriber::fmt::try_init();
    // Shipped, since its admission mounts the ticket a browser dials.
    let relay = TestRelay::start_shipped().await;
    let (publisher, broadcast, ticket) = start_publisher("remote-stream").await;

    let local_name = ticket.to_string();
    let pulls = iroh_live_relay::pull::Pulls::new(endpoint().await, relay.cluster.clone())
        .with_linger(PULL_LINGER);
    let guard = step(
        "pull timeout",
        pulls.pull(Client::Unknown, &local_name, &ticket),
    )
    .await
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
}

/// A relay link over iroh requests the path of its URL, so the relay pulls the ticket it names.
#[tokio::test]
async fn an_iroh_relay_url_requests_its_path() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_pulling().await;
    let (publisher, broadcast, ticket) = start_publisher("by-path").await;
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
}

/// A browser that dials a ticket holds a pull of it until its session ends.
#[tokio::test]
async fn a_browser_that_dials_a_ticket_holds_its_pull() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_pulling().await;
    let (publisher, broadcast, ticket) = start_publisher("dialed").await;
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

/// Starts an iroh publisher with a video broadcast, and returns a ticket to it.
async fn start_publisher(
    name: &str,
) -> (
    TestNode,
    iroh_live::media::LocalBroadcast,
    iroh_live::BroadcastTicket,
) {
    let node = TestNode::spawn(LiveLayout::default().config()).await;
    let broadcast = publish_video(&node.node, name);
    let ticket = LiveLayout::default()
        .ticket(node.id(), name)
        .expect("a valid broadcast name");
    (node, broadcast, ticket)
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
    let (publisher, broadcast, ticket) = start_publisher("spelling").await;

    let canonical = ticket.to_string();
    // The scheme is case-insensitive, so this is another spelling of the ticket.
    let bare = canonical
        .strip_prefix("ilv1:")
        .map(|rest| format!("ILV1:{rest}"))
        .expect("a ticket serializes with its scheme");
    assert_ne!(bare, canonical);

    let pulls = iroh_live_relay::pull::Pulls::new(endpoint().await, relay.cluster.clone())
        .with_linger(PULL_LINGER);

    let guard = step("pull timeout", pulls.pull(Client::Unknown, &bare, &ticket))
        .await
        .expect("pull");
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &bare), &bare, true).await,
        "the pull was announced somewhere other than the name that was asked for",
    );

    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
}

/// An unwatched pull retires and closes its connection, and the next pull dials anew.
#[tokio::test]
async fn pull_retires_an_unwatched_session() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (publisher, broadcast, ticket) = start_publisher("retired-stream").await;
    let local_name = ticket.to_string();

    let pull_ep = endpoint().await;
    let pull_id = pull_ep.id();
    let pulls =
        iroh_live_relay::pull::Pulls::new(pull_ep, relay.cluster.clone()).with_linger(PULL_LINGER);

    let guard = step(
        "pull timeout",
        pulls.pull(Client::Unknown, &local_name, &ticket),
    )
    .await
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
        &publisher.node,
        pull_id,
        "the relay kept its session with the publisher of a retired pull",
    )
    .await;

    // The same ticket dials a new session.
    let guard = step(
        "re-pull timeout",
        pulls.pull(Client::Unknown, &local_name, &ticket),
    )
    .await
    .expect("re-pull");
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &local_name), &local_name, true).await,
        "pulling a retired ticket should dial the publisher again"
    );

    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
}

/// A pull outlives its publisher re-publishing the broadcast, while a session holds it.
#[tokio::test]
async fn a_pull_survives_the_publisher_republishing() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (publisher, broadcast, ticket) = start_publisher("republished").await;
    let local_name = ticket.to_string();

    let pulls = iroh_live_relay::pull::Pulls::new(endpoint().await, relay.cluster.clone())
        .with_linger(PULL_LINGER);
    let guard = step(
        "pull timeout",
        pulls.pull(Client::Unknown, &local_name, &ticket),
    )
    .await
    .expect("pull");
    assert!(wait_for_broadcast(&pulled(&relay.cluster, &local_name), &local_name, true).await);

    drop(broadcast);
    let again = publish_video(&publisher.node, "republished");

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

    let subscription = step(
        "subscribe timeout",
        node.subscribe("browser-stream", Reach::Relays),
    )
    .await
    .expect("subscribe through the relay");
    let frame = first_frame(&subscription.broadcast(), "data").await;
    assert_eq!(&frame.payload[..], b"from-the-browser");
    let serving = subscription.serving_link().expect("a serving link");
    assert_eq!(serving.kind, LinkKind::Relay);
    // Detaching withdraws what the relay taught the route table.
    let mut updates = node.origin().announced();
    let route = step("the relay's route never reached the table", async {
        loop {
            let update = updates.next().await.expect("origin closed");
            let announce = announcement(&update).0;
            if announce.prefix.as_str() == "browser-stream" {
                return announce.route.clone();
            }
        }
    })
    .await;
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
    let endpoint = endpoint().await;
    let node = iroh_live::Node::new(endpoint.clone(), LiveLayout::default().config());
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
    step("the relay link never connected", async {
        while status.get() != RelayStatus::Connected {
            status.updated().await.expect("link gone");
        }
    })
    .await;
    link
}

/// Writes a counter from `first` every few milliseconds into the track `data` of `broadcast`.
fn counter(broadcast: &moq_net::broadcast::Producer, first: u64) -> AbortOnDropHandle<()> {
    let mut track = broadcast
        .create_track(
            "data",
            moq_net::track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    AbortOnDropHandle::new(tokio::spawn(async move {
        for n in first.. {
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
    let subscription = step(
        "subscribe timeout",
        viewer.subscribe(ticket.path(), Reach::Relays),
    )
    .await
    .expect("subscribe through the relay");

    // A link serves a path once a reader asks it for a track.
    first_frame(&subscription.broadcast(), "catalog.json").await;

    // The transport: the relay link serves the path, and its monitor has
    // measured the link.
    let serving = step("the relay link never reported a round trip", async {
        loop {
            if let Some(serving) = subscription.serving_link()
                && serving.sample.rtt.is_some()
            {
                return serving;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert_eq!(serving.kind, LinkKind::Relay);
    assert_eq!(serving.id, link.id());
    assert!(serving.sample.min_rtt.is_some(), "{serving:?}");

    // The facade: the player's network signals carry the same readings.
    let player = subscription
        .remote_broadcast()
        .play(PlayerConfig::default())
        .expect("play");
    let network = step("the player never saw the relay link's round trip", async {
        loop {
            if let Some(network) = player.stats().network
                && network.rtt.is_some()
            {
                return network;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
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
    let _public = counter(public.broadcast(), 0);
    let peers = node
        .publish(
            live_path(node.id(), "peers"),
            Audience::Peers(members.clone()),
        )
        .expect("publish");
    let _peers = counter(peers.broadcast(), 0);

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
        .attach_relay(RelayConfig::iroh(relay.iroh_id).with_consume(false))
        .expect("attach");

    link.detach().await;
    // The detached link feeds nothing, so a relay subscribe fails.
    let err = step(
        "a relay subscribe with no consuming relay waited",
        node.subscribe("anything", Reach::Relays),
    )
    .await
    .expect_err("resolved without a relay");
    assert!(matches!(err, Error::NoRoute { .. }), "{err:#}");

    let mut status = consume_only.status();
    step("shutdown hung", node.shutdown()).await;
    step("a relay link outlived its node's shutdown", async {
        while status.get() != RelayStatus::Detached {
            status.updated().await.expect("link gone");
        }
    })
    .await;
}

/// A subscription ends with its direct session, and asking again finds the relay.
///
/// It does not move over on its own, since a relay route cannot vouch for the publisher.
#[tokio::test]
async fn losing_the_direct_session_moves_a_subscription_to_the_relay() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let TestNode {
        node: alice,
        router: _alice_router,
        ..
    } = TestNode::spawn(LiveLayout::default().config()).await;
    let publication = alice
        .publish(live_path(alice.id(), "cam"), Audience::Granted)
        .expect("publish");
    let _writer = counter(publication.broadcast(), 0);
    let _alice_link = attached(&alice, &relay).await;

    let (_bob_endpoint, bob) = relay_node().await;
    let subscription = step(
        "subscribe timeout",
        bob.subscribe(publication.path(), Reach::Direct(alice.endpoint().id())),
    )
    .await
    .expect("subscribe");
    let mut reader = subscribed(&subscription.broadcast()).await;
    let _bob_link = attached(&bob, &relay).await;
    relay_route_arrives(&relay, publication.path().as_str()).await;

    subscription
        .session()
        .expect("served directly")
        .close(moq_net::Error::Cancel);
    step("the subscription outlived its direct session", async {
        while let Ok(Some(_)) = reader.recv_group().await {}
    })
    .await;

    let again = step(
        "subscribe timeout",
        bob.subscribe(publication.path(), Reach::Relays),
    )
    .await
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
    let mut reader = step(
        "track subscribe timeout",
        broadcast.track("data").expect("track").subscribe(
            moq_net::track::Subscription::default()
                .with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(5))),
        ),
    )
    .await
    .expect("track subscribe");
    step("no group", reader.recv_group())
        .await
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
    let TestNode {
        endpoint: alice_endpoint,
        node: alice,
        router: _alice_router,
    } = TestNode::spawn(LiveLayout::default().config()).await;
    let publication = alice
        .publish(live_path(alice.id(), "cam"), Audience::Granted)
        .expect("publish");
    let _writer = counter(publication.broadcast(), 0);

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
    let _forger = counter(forged.broadcast(), FORGED);
    let _mallory_session = established(
        noq_client()
            .with_publisher(mallory.consume())
            .connect(relay.url()),
    )
    .await;

    let (_bob_endpoint, bob) = relay_node().await;
    let subscription = step(
        "subscribe timeout",
        bob.subscribe(publication.path(), Reach::Direct(alice.endpoint().id())),
    )
    .await
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
    let _own = counter(own.broadcast(), 0);
    let forged_path = format!("live/{alice}/cam");
    let room_path = format!("rooms/topic/{alice}/cam");
    let forged = mallory
        .publish(forged_path.as_str(), Audience::Granted)
        .expect("publish at alice's path");
    let _forged = counter(forged.broadcast(), 0);
    let room_forged = mallory
        .publish(room_path.as_str(), Audience::Granted)
        .expect("publish at alice's room path");
    let _room_forged = counter(room_forged.broadcast(), 0);
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
    let subscription = step(
        "subscribe timeout",
        viewer.subscribe("browser-stream", iroh_live_node::Reach::Relays),
    )
    .await
    .expect("subscribe through the relay");
    let frame = first_frame(&subscription.broadcast(), "data").await;
    assert_eq!(&frame.payload[..], b"from-the-browser");

    drop((browser_forged, browser_own));
    viewer.shutdown().await;
    mallory.shutdown().await;
    drop(mallory_endpoint);
}

/// A browser that dials an iroh publisher's path cannot publish beneath it.
///
/// A grant without a root is rooted at the dialed path, so the browser's
/// one-segment rule must be reduced to that path, not granted as is.
#[tokio::test]
async fn a_browser_cannot_publish_under_a_dialed_iroh_path() {
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
    let mut sessions = Vec::new();
    let mut held = Vec::new();
    for dialed in [format!("live/{alice}"), format!("rooms/topic/{alice}")] {
        let browser = moq_tokio::origin::spawn();
        held.push(
            browser
                .route("cam", origin::RouteOptions::default())
                .expect("broadcast"),
        );
        let url = relay.url().join(&dialed).expect("a url");
        sessions
            .push(established(noq_client().with_publisher(browser.consume()).connect(url)).await);
    }
    // A browser at the root still publishes a name of its own.
    let browser = moq_tokio::origin::spawn();
    held.push(
        browser
            .route("hello", origin::RouteOptions::default())
            .expect("broadcast"),
    );
    sessions.push(
        established(
            noq_client()
                .with_publisher(browser.consume())
                .connect(relay.url()),
        )
        .await,
    );

    announced_at(&sub_origin, "hello").await;
    for path in [
        format!("live/{alice}/cam"),
        format!("rooms/topic/{alice}/cam"),
    ] {
        assert!(
            !routed_soon(&sub_origin, &path).await,
            "a browser published at {path}"
        );
    }
    drop((sessions, held));
}

/// Two pulls of one publisher share a session, which closes with the last.
#[tokio::test]
async fn a_publisher_session_outlives_all_but_its_last_pull() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (publisher, first_broadcast, first) = start_publisher("first").await;
    let second_broadcast = publish_video(&publisher.node, "second");
    let second = LiveLayout::default()
        .ticket(publisher.id(), "second")
        .expect("a valid broadcast name");
    let (first_name, second_name) = (first.to_string(), second.to_string());

    let pull_ep = endpoint().await;
    let pull_id = pull_ep.id();
    let pulls =
        iroh_live_relay::pull::Pulls::new(pull_ep, relay.cluster.clone()).with_linger(PULL_LINGER);
    let first_guard = step(
        "pull timeout",
        pulls.pull(Client::Unknown, &first_name, &first),
    )
    .await
    .expect("pull");
    let second_guard = step(
        "pull timeout",
        pulls.pull(Client::Unknown, &second_name, &second),
    )
    .await
    .expect("pull");
    for name in [&first_name, &second_name] {
        assert!(
            wait_for_broadcast(&pulled(&relay.cluster, name), name, true).await,
            "{name} was never mirrored"
        );
    }
    let pull_sessions = || {
        publisher
            .node
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
    session_closed(
        &publisher.node,
        pull_id,
        "the session outlived the last pull",
    )
    .await;

    drop((first_broadcast, second_broadcast));
    publisher.shutdown().await;
}

/// A pull lets none of the publisher's other broadcasts into the cluster, not even while it dials.
#[tokio::test]
async fn a_pull_keeps_the_publishers_other_broadcasts_out() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (publisher, wanted_broadcast, wanted) = start_publisher("wanted").await;
    let other_broadcast = publish_video(&publisher.node, "other");
    let other = live_path(publisher.id(), "other");

    // Every path the cluster announces, from before the dial on.
    let cluster = relay.cluster.origin.consume();
    let mut announced = cluster.announced();
    let pulls = iroh_live_relay::pull::Pulls::new(endpoint().await, relay.cluster.clone())
        .with_linger(PULL_LINGER);
    let guard = step(
        "pull timeout",
        pulls.pull(Client::Unknown, &wanted.to_string(), &wanted),
    )
    .await
    .expect("pull");

    let mut seen = Vec::new();
    let mut settle = None;
    loop {
        let deadline = settle.unwrap_or_else(|| tokio::time::Instant::now() + TIMEOUT);
        match tokio::time::timeout_at(deadline, announced.next()).await {
            Ok(Some(event)) => {
                let path = announcement(&event).0.prefix.as_str().to_owned();
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
}

/// Binds a node with rooms that accepts both MoQ and the rooms' gossip.
async fn room_node() -> (
    iroh::Endpoint,
    iroh_live_node::Node,
    iroh_live_rooms::Rooms,
    iroh::protocol::Router,
) {
    let endpoint = endpoint().await;
    let node = iroh_live_node::Node::new(
        endpoint.clone(),
        iroh_live::layout::config((LiveLayout::default(), iroh_live_rooms::RoomsLayout)),
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
    let _writer = counter(&cam, 0);
    let subscription = step(
        "subscribe timeout",
        room_a.subscribe(bob_endpoint.id(), "cam"),
    )
    .await
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
        RelayConfig::new(relay.http_url()).with_consume(false),
    )
    .await;
    let _broadcast = publish_video(&publisher, "studio");
    let path = live_path(publisher.id(), "studio");
    routed(&relay.cluster.origin.consume(), &path).await;

    let (_viewer_endpoint, viewer) = relay_node().await;
    let link = connected(&viewer, RelayConfig::new(relay.http_url())).await;
    let subscription = step("subscribe timeout", viewer.subscribe(path, Reach::Relays))
        .await
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
    let mut config = RelayConfig::iroh(relay.iroh_id);
    if let Some(token) = token {
        config = config.with_token(token.parse().expect("not empty"));
    }
    node.attach_relay(config).expect("attach")
}

/// Signs a token for `tokens/**` that expires `after` from now.
fn expiring_token(keys: &KeySet, after: Duration) -> String {
    let mut claims = Claims::default()
        .with_publish(["tokens/**".parse().expect("a pattern")])
        .with_subscribe([moq_net::Pattern::all()]);
    claims.expires = Some(
        (std::time::SystemTime::now() + after)
            .try_into()
            .expect("after 1970"),
    );
    keys.sign(&claims).expect("signed")
}

/// A token in an `iroh://` URL's query is presented, as it is on an https link.
#[tokio::test]
async fn an_iroh_url_presents_the_token_in_its_query() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;

    let (_endpoint, holder) = relay_node().await;
    let granted = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _granted = counter(granted.broadcast(), 0);
    let url = format!(
        "iroh://{}/?jwt={}",
        relay.iroh_id,
        token_for(&keys, "tokens")
    )
    .parse()
    .expect("a url");
    let _link = holder.attach_relay(RelayConfig::new(url)).expect("attach");
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;

    holder.shutdown().await;
}

/// With a key, an iroh client's valid token decides what it publishes.
///
/// A client without a token keeps the relay's rule.
#[tokio::test]
async fn an_iroh_client_with_a_valid_token_publishes_where_it_allows() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;

    let (_endpoint, holder) = relay_node().await;
    let granted = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _granted = counter(granted.broadcast(), 0);
    let own = holder
        .publish(
            LiveLayout::default()
                .path(holder.id(), "cam")
                .expect("a path"),
            Audience::Granted,
        )
        .expect("publish");
    let _own = counter(own.broadcast(), 0);
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
    let _plain_own = counter(plain_own.broadcast(), 0);
    let plain_tokens = plain
        .publish("tokens/screen", Audience::Granted)
        .expect("publish");
    let _plain_tokens = counter(plain_tokens.broadcast(), 0);
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
    let _own = counter(own.broadcast(), 0);
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

/// With a key file, a browser's token verifies against the keys the file holds now.
#[tokio::test]
async fn a_rotated_key_file_takes_effect_for_browsers() {
    let _ = tracing_subscriber::fmt::try_init();
    let old = signing_keys();
    let relay = TestRelay::start_keyed(old.clone()).await;
    let rotated = signing_keys();
    relay.keys_file.as_ref().expect("keyed").rotate(&rotated);

    // A browser that publishes `studio/<name>` with a token signed by `keys`.
    let publish = |keys: &KeySet, name: &str| {
        let browser = moq_tokio::origin::spawn();
        let route = browser
            .route(
                format!("studio/{name}").as_str(),
                origin::RouteOptions::default(),
            )
            .expect("broadcast");
        let mut url = relay.url();
        url.query_pairs_mut()
            .append_pair("jwt", &token_for(keys, "studio"));
        let connection = noq_client().with_publisher(browser.consume()).connect(url);
        (browser, route, connection)
    };
    let (_browser, _route, connection) = publish(&rotated, "new");
    let _session = established(connection).await;
    routed(&relay.cluster.origin.consume(), "studio/new").await;

    let (_browser, _route, connection) = publish(&old, "old");
    // Held, since dropping the connection would close the session.
    let _old = tokio::time::timeout(TIMEOUT, connection.established()).await;
    assert!(
        !routed_soon(&relay.cluster.origin, "studio/old").await,
        "a token of the rotated-out key was admitted"
    );
}

/// With a key file, an iroh client's token verifies against the keys the file holds now.
#[tokio::test]
async fn a_rotated_key_file_takes_effect_for_iroh_clients() {
    let _ = tracing_subscriber::fmt::try_init();
    let old = signing_keys();
    let relay = TestRelay::start_keyed(old.clone()).await;

    // Admitted before the rotation, with the old key.
    let (_before_endpoint, before) = relay_node().await;
    let early = before
        .publish("tokens/early", Audience::Granted)
        .expect("publish");
    let _early = counter(early.broadcast(), 0);
    let _before_link = attach_with_token(&before, &relay, Some(token_for(&old, "tokens")));
    routed(&relay.cluster.origin.consume(), "tokens/early").await;

    let rotated = signing_keys();
    relay.keys_file.as_ref().expect("keyed").rotate(&rotated);

    let (_new_endpoint, new) = relay_node().await;
    let fresh = new
        .publish("tokens/new", Audience::Granted)
        .expect("publish");
    let _fresh = counter(fresh.broadcast(), 0);
    let _new_link = attach_with_token(&new, &relay, Some(token_for(&rotated, "tokens")));
    routed(&relay.cluster.origin.consume(), "tokens/new").await;

    let (_old_endpoint, stale) = relay_node().await;
    let late = stale
        .publish("tokens/old", Audience::Granted)
        .expect("publish");
    let _late = counter(late.broadcast(), 0);
    let _old_link = attach_with_token(&stale, &relay, Some(token_for(&old, "tokens")));
    assert!(
        !routed_soon(&relay.cluster.origin, "tokens/old").await,
        "a token of the rotated-out key was admitted"
    );

    before.shutdown().await;
    new.shutdown().await;
    stale.shutdown().await;
}

/// With a key, an iroh client's session closes when its token expires, and its broadcast leaves.
#[tokio::test]
async fn an_iroh_clients_session_closes_when_its_token_expires() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;
    let token = expiring_token(&keys, Duration::from_secs(2));

    let (_endpoint, holder) = relay_node().await;
    let cam = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _cam = counter(cam.broadcast(), 0);
    let mut updates = relay.cluster.origin.consume().announced();
    let _link = attach_with_token(&holder, &relay, Some(token));
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    // The redial presents the same expired token, which the relay refuses.
    retracted(&mut updates, "tokens/cam").await;
    assert!(!routed_soon(&relay.cluster.origin, "tokens/cam").await);

    holder.shutdown().await;
}

/// An iroh relay link presents the tokens set on it.
///
/// The redial after the first token expired therefore gets back in.
#[tokio::test]
async fn a_relay_link_presents_a_fresh_token_when_it_redials() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed(keys.clone()).await;

    let (_endpoint, holder) = relay_node().await;
    let cam = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _cam = counter(cam.broadcast(), 0);
    let mut updates = relay.cluster.origin.consume().announced();
    // The first token expires, so the relay closes the session.
    let first = expiring_token(&keys, Duration::from_secs(2));
    let link = attach_with_token(&holder, &relay, Some(first));
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    let fresh = token_for(&keys, "tokens").parse().expect("not empty");
    link.tokens().set(vec![fresh]);
    retracted(&mut updates, "tokens/cam").await;
    // Back with the fresh token, which the relay verified on the redial.
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;

    holder.shutdown().await;
}

/// An https relay link presents the tokens set on it, so its route outlives the first token.
#[tokio::test]
async fn an_https_relay_link_presents_a_fresh_token() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed_http(keys.clone()).await;

    let (_endpoint, holder) = relay_node().await;
    let cam = holder
        .publish("tokens/cam", Audience::Granted)
        .expect("publish");
    let _cam = counter(cam.broadcast(), 0);
    let first = expiring_token(&keys, Duration::from_secs(2));
    let link = connected(
        &holder,
        RelayConfig::new(relay.http_url()).with_token(first.parse().expect("not empty")),
    )
    .await;
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    let fresh = token_for(&keys, "tokens").parse().expect("not empty");
    link.tokens().set(vec![fresh]);
    // Past the first token's expiry, the relay still routes the broadcast.
    tokio::time::sleep(Duration::from_secs(3)).await;
    routed(&relay.cluster.origin.consume(), "tokens/cam").await;
    assert_eq!(link.status().get(), RelayStatus::Connected);

    holder.shutdown().await;
}

/// An iroh relay link to a relay that is down at attach keeps redialing until the relay is up.
#[tokio::test]
async fn a_relay_link_waits_for_a_relay_that_is_down_at_attach() {
    let _ = tracing_subscriber::fmt::try_init();
    let secret_key = iroh::SecretKey::generate();
    let (_endpoint, holder) = relay_node().await;
    let cam = holder
        .publish(
            LiveLayout::default()
                .path(holder.id(), "cam")
                .expect("a path"),
            Audience::Granted,
        )
        .expect("publish");
    let _cam = counter(cam.broadcast(), 0);
    let link = holder
        .attach_relay(RelayConfig::iroh(secret_key.public()))
        .expect("attach");
    let mut status = link.status();
    step("the first dial fails", async {
        while status.get() != RelayStatus::Reconnecting {
            status.updated().await.expect("link gone");
        }
    })
    .await;

    let relay = TestRelay::start_with(true, None, None, None, secret_key).await;
    step("the relay link connects once the relay is up", async {
        while status.get() != RelayStatus::Connected {
            status.updated().await.expect("link gone");
        }
    })
    .await;
    routed(&relay.cluster.origin.consume(), cam.path().as_str()).await;

    holder.shutdown().await;
}

/// With a key, a browser without a token keeps the public rule: names of one segment only.
#[tokio::test]
async fn a_browser_without_a_token_keeps_the_public_rule_under_a_key() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_keyed(signing_keys()).await;

    let browser = moq_tokio::origin::spawn();
    let _screen = browser
        .route("screen", origin::RouteOptions::default())
        .expect("broadcast");
    let _studio = browser
        .route("studio/cam", origin::RouteOptions::default())
        .expect("broadcast");
    let _session = established(
        noq_client()
            .with_publisher(browser.consume())
            .connect(relay.url()),
    )
    .await;
    routed(&relay.cluster.origin.consume(), "screen").await;
    assert!(
        !routed_soon(&relay.cluster.origin, "studio/cam").await,
        "a session without a token published two segments"
    );
    drop(browser);
}

/// Signs a token that lets its holder read under `prefix/` and publish nothing.
fn reader_token(keys: &KeySet, prefix: &str) -> String {
    let claims =
        Claims::default().with_subscribe([format!("{prefix}/**").parse().expect("a pattern")]);
    keys.sign(&claims).expect("signed")
}

/// Dials `relay` as a browser at the ticket `name`, with `token` if set, and holds the connection.
///
/// The client's half of the handshake may complete before a refusal, so this
/// does not tell an admitted session from a refused one. `None` if the
/// handshake failed or timed out.
async fn browser_at(
    relay: &TestRelay,
    name: &str,
    token: Option<&str>,
) -> Option<moq_tokio::Connection> {
    let mut url = relay.url();
    url.set_path(name);
    if let Some(token) = token {
        url.query_pairs_mut().append_pair("jwt", token);
    }
    let connection = noq_client()
        .with_subscriber(moq_tokio::origin::spawn())
        .connect(url);
    tokio::time::timeout(TIMEOUT, connection.established())
        .await
        .ok()
        .and_then(Result::ok)
}

/// With a key, a browser pulls a ticket only with a token that grants reading the ticket's path.
#[tokio::test]
async fn a_browser_pulls_under_a_key_only_with_a_token_for_the_path() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed_pulling(keys.clone()).await;
    let (publisher, broadcast, ticket) = start_publisher("guarded").await;
    let name = ticket.to_string();
    let path = ticket.path().to_string();

    let _anonymous = browser_at(&relay, &name, None).await;
    let _elsewhere = browser_at(&relay, &name, Some(&reader_token(&keys, "other"))).await;
    let _forged = browser_at(&relay, &name, Some(&reader_token(&signing_keys(), "live"))).await;
    assert!(
        !routed_soon(&relay.cluster.origin, &path).await,
        "the relay pulled for a session without a token for the path"
    );

    // A token for `live/**` reaches the ticket's path, though not the name dialed.
    let _reader = browser_at(&relay, &name, Some(&reader_token(&keys, "live"))).await;
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &name), &name, true).await,
        "the relay did not pull for a token holder"
    );

    drop(broadcast);
    publisher.shutdown().await;
}

/// With a key, an iroh client pulls a ticket only with a token that grants reading the ticket's path.
#[tokio::test]
async fn an_iroh_client_pulls_under_a_key_only_with_a_token() {
    let _ = tracing_subscriber::fmt::try_init();
    let keys = signing_keys();
    let relay = TestRelay::start_keyed_pulling(keys.clone()).await;
    let (publisher, broadcast, ticket) = start_publisher("guarded").await;
    let name = ticket.to_string();
    let path = ticket.path().to_string();

    let (_anonymous_endpoint, anonymous) = relay_node().await;
    let url = format!("iroh://{}/{name}", relay.iroh_id)
        .parse()
        .expect("a valid URL");
    let _anonymous = anonymous
        .attach_relay(RelayConfig::new(url))
        .expect("attach");
    assert!(
        !routed_soon(&relay.cluster.origin, &path).await,
        "the relay pulled for an iroh client without a token"
    );

    // The token admits the session at the dialed name, and grants the ticket's path.
    let (_holder_endpoint, holder) = relay_node().await;
    let url = format!("iroh://{}/{name}", relay.iroh_id)
        .parse()
        .expect("a valid URL");
    let everything = Claims::default().with_subscribe([moq_net::Pattern::all()]);
    let token = keys.sign(&everything).expect("signed");
    let config = RelayConfig::new(url).with_token(token.parse().expect("not empty"));
    let _holder = holder.attach_relay(config).expect("attach");
    assert!(
        wait_for_broadcast(&pulled(&relay.cluster, &name), &name, true).await,
        "the relay did not pull for a token holder"
    );

    anonymous.shutdown().await;
    holder.shutdown().await;
    drop(broadcast);
    publisher.shutdown().await;
}

/// Without a key, an iroh client pulls nothing, whatever token it presents.
///
/// The relay's own policy admits the session and ignores the token, so the
/// token must not count as one that grants the pull.
#[tokio::test]
async fn an_iroh_client_pulls_nothing_from_a_relay_without_a_key() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_with(
        true,
        Some(Access::Authorized),
        None,
        None,
        iroh::SecretKey::generate(),
    )
    .await;
    let (publisher, broadcast, ticket) = start_publisher("guarded").await;
    let name = ticket.to_string();

    let (_endpoint, client) = relay_node().await;
    let url = format!("iroh://{}/{name}?jwt=anything", relay.iroh_id)
        .parse()
        .expect("a valid URL");
    let _link = client.attach_relay(RelayConfig::new(url)).expect("attach");
    assert!(
        !routed_soon(&relay.cluster.origin, ticket.path().as_str()).await,
        "the relay pulled for an unverified token"
    );

    client.shutdown().await;
    drop(broadcast);
    publisher.shutdown().await;
}
