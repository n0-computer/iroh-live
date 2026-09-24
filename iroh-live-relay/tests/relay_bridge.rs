//! Integration tests for the iroh-live relay bridging.
//!
//! These tests exercise the relay's ability to bridge broadcasts between
//! different transport backends (noq/WebTransport and iroh P2P), verifying
//! that data published on one transport is visible to subscribers on another.
//!
//! All iroh endpoints use `presets::Minimal` + a shared `MemoryLookup` instead
//! of `presets::N0` to avoid depending on real network discovery (DNS, relays),
//! which is flaky in CI.

use std::{sync::OnceLock, time::Duration};

use iroh::address_lookup::MemoryLookup;
use moq_net::{Timestamp, origin};
use moq_relay::cluster::Cluster;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
use serial_test::serial;

const TIMEOUT: Duration = Duration::from_secs(10);

static ADDRESS_LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();

/// Returns the shared MemoryLookup, creating it if needed.
fn shared_lookup() -> MemoryLookup {
    ADDRESS_LOOKUP.get_or_init(Default::default).clone()
}

/// Starts a relay (noq server + iroh endpoint + cluster) and returns handles.
///
/// Both tasks are held rather than detached, so a test that ends early, or
/// panics, takes its relay with it instead of leaving it accepting connections
/// for the rest of the run.
struct TestRelay {
    _server_task: AbortOnDropHandle<()>,
    /// Accepts iroh clients, for a relay wired as the shipped one is.
    _iroh_router: Option<iroh::protocol::Router>,
    _cluster_task: AbortOnDropHandle<()>,
    cluster: Cluster,
    noq_addr: std::net::SocketAddr,
    iroh_id: iroh::EndpointId,
}

impl TestRelay {
    /// Starts a relay that lets anyone publish anywhere, as a relay with loose
    /// admission would.
    ///
    /// Most tests here are about bridging, which does not care who may
    /// publish where; the ones about forging paths use it as the relay a node
    /// must not trust.
    ///
    /// A cluster unannounces a broadcast the moment it loses its last source,
    /// which is what the pull-lifecycle tests below observe. It used to linger
    /// for five seconds unless told otherwise; moq removed the knob along with
    /// the delay.
    async fn start() -> Self {
        Self::start_with(false).await
    }

    /// Starts a relay wired the way `iroh_live_relay::run` wires one: iroh
    /// clients publish only under their own id, browsers only at names of one
    /// segment.
    async fn start_shipped() -> Self {
        Self::start_with(true).await
    }

    async fn start_with(shipped: bool) -> Self {
        let mut quic = moq_tokio::quic::Config::default();
        quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
        let connect = moq_tokio::connect::Config::default();

        // Build the relay's iroh endpoint with Minimal preset + MemoryLookup
        // instead of presets::N0, which uses real DNS discovery. This makes
        // tests reliable in CI without network access.
        let mut alpns: Vec<Vec<u8>> = moq_net::ALPNS
            .iter()
            .map(|alpn| alpn.as_bytes().to_vec())
            .collect();
        alpns.push(web_transport_iroh::ALPN_H3.as_bytes().to_vec());

        let iroh = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .address_lookup(shared_lookup())
            .secret_key(iroh::SecretKey::generate())
            .alpns(alpns)
            .bind()
            .await
            .expect("bind relay iroh");

        shared_lookup().add_endpoint_info(iroh.addr());
        let iroh_id = iroh.id();

        let mut server_config = moq_tokio::server::Config::default();
        server_config.listen.bind = Some(moq_tokio::listen::Bind::Addr(
            "[::]:0".parse().expect("valid address"),
        ));
        server_config.listen.tls.generate = vec!["localhost".into()];
        server_config.quic = quic.clone();
        if !shipped {
            server_config.iroh = Some(iroh.clone());
        }
        let server = server_config.init().expect("init server");
        let client = connect
            .clone()
            .init(quic)
            .expect("init client")
            .with_iroh(iroh.clone());

        let auth_config = if shipped {
            iroh_live_relay::browser_auth()
        } else {
            let mut auth_config = moq_relay::auth::Config::default();
            auth_config.public = vec![moq_net::Pattern::all()];
            auth_config
        };
        let auth = auth_config
            .init("relay-bridge-test", &connect.tls)
            .expect("init auth");

        let cluster = Cluster::new(moq_relay::cluster::Options::new(Default::default()))
            .expect("init cluster")
            .with_client(client);
        let started = cluster.clone().start().await.expect("start cluster");
        let cluster_task = AbortOnDropHandle::new(tokio::spawn(async move {
            started.run().await.expect("cluster failed");
        }));
        let iroh_router =
            shipped.then(|| iroh_live_relay::IrohSessions::new(cluster.clone(), None).router(iroh));

        let mut listener = server.listen().await.expect("listen");
        let noq_addr = listener.local_addr().expect("get noq addr");
        let cluster_clone = cluster.clone();
        let server_task = AbortOnDropHandle::new(tokio::spawn(async move {
            let mut conn_id = 0u64;
            while let Some(request) = listener.accept().await {
                let conn = moq_relay::Connection::new(request, cluster_clone.clone(), auth.clone())
                    .with_id(conn_id);
                conn_id += 1;
                tokio::spawn(async move {
                    if let Err(err) = conn.run().await {
                        tracing::warn!(%err, "relay conn closed");
                    }
                });
            }
        }));

        Self {
            _server_task: server_task,
            _iroh_router: iroh_router,
            _cluster_task: cluster_task,
            cluster,
            noq_addr,
            iroh_id,
        }
    }

    /// Returns the WebTransport URL a browser would dial.
    fn url(&self) -> url::Url {
        format!("https://localhost:{}", self.noq_addr.port())
            .parse()
            .expect("valid url")
    }
}

/// Creates an origin and runs its driver for as long as the handle lives.
///
/// An origin makes no progress without its driver: announcements, route
/// resolution and closing all happen there.
fn test_origin() -> (origin::Producer, AbortOnDropHandle<()>) {
    let (origin, driver) = origin::Producer::new(origin::Config::default());
    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        let _ = moq_net::time::run(driver).await;
    }));
    (origin, task)
}

/// Builds a one-shot noq client that trusts the relay's self-signed certificate,
/// as the browser does by pinning its fingerprint.
fn noq_client() -> moq_tokio::Client {
    let mut connect = moq_tokio::connect::Config::default();
    connect.tls.insecure = Some(true);
    connect.once = Some(true);
    connect
        .init(moq_tokio::quic::Config::default())
        .expect("init client")
}

/// Waits for a dialled connection to complete its MoQ handshake.
async fn established(connection: moq_tokio::Connection) -> moq_tokio::Connection {
    tokio::time::timeout(TIMEOUT, connection.established())
        .await
        .expect("connect timeout")
        .expect("connect")
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
    let path = update.prefix.as_str().to_owned();
    let broadcast = tokio::time::timeout(TIMEOUT, consumer.routed_broadcast(path.as_str()))
        .await
        .expect("resolve timeout")
        .expect("resolve");
    (path, broadcast)
}

/// Waits until `path` is announced on `origin`, and resolves it.
async fn announced_at(origin: &origin::Producer, path: &str) -> moq_net::broadcast::Consumer {
    let consumer = origin.consume();
    tokio::time::timeout(TIMEOUT, consumer.routed_broadcast(path))
        .await
        .unwrap_or_else(|_| panic!("{path} was never announced"))
        .expect("resolve")
}

/// Publishes a generated video broadcast as `name` on `live`.
fn publish_video(live: &iroh_live::Live, name: &str) -> iroh_live::LocalBroadcast {
    use iroh_live_media::{VideoEncoding, VideoRendition, VideoSource, video};
    let broadcast = iroh_live::LocalBroadcast::new();
    let source = VideoSource::test_pattern(
        video::Size::new(320, 240),
        video::Rate::new(30, 1).expect("a valid rate"),
    );
    broadcast
        .set_video(source, VideoEncoding::single(VideoRendition::new("video")))
        .expect("set video");
    live.publish(name, &broadcast).expect("publish");
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
#[serial]
async fn noq_publish_noq_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;

    // Publisher
    let (pub_origin, _pub_driver) = test_origin();
    let broadcast = pub_origin
        .publish("test", origin::Route::default())
        .expect("create bc");
    let track = broadcast.create_track("video", None).expect("track");
    let mut group = track.append_group().expect("group");
    group
        .write_frame(Timestamp::ZERO, b"hello-noq".as_ref())
        .expect("write");
    group.finish().expect("finish");

    let _pub_session = established(
        noq_client()
            .with_publisher(pub_origin.consume())
            .connect(relay.url()),
    )
    .await;

    // Subscriber
    let (sub_origin, _sub_driver) = test_origin();
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

/// iroh publish -> relay -> iroh subscribe (using iroh-live Live API).
#[tokio::test]
#[serial]
async fn iroh_publish_iroh_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id = relay.iroh_id;

    // Publisher
    let pub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind pub");
    shared_lookup().add_endpoint_info(pub_ep.addr());
    let publisher = iroh_live::Live::builder(pub_ep.clone())
        .with_router()
        .spawn();
    let broadcast = publish_video(&publisher, "relay-test");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.moq().connect(relay_id))
        .await
        .expect("timeout")
        .expect("connect");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Subscriber
    let sub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind sub");
    shared_lookup().add_endpoint_info(sub_ep.addr());
    let subscriber = iroh_live::Live::builder(sub_ep.clone()).spawn();
    // Through the relay's session, at the path that names the publisher: the
    // same path the publisher offers on a direct session.
    let path = iroh_live::BroadcastTicket::new(pub_ep.id(), "relay-test").path();
    let sub = tokio::time::timeout(TIMEOUT, async {
        let session = subscriber.moq().connect_with(relay_id, trusted()).await?;
        let subscription = session.subscribe(path).await?;
        Ok::<_, iroh_live::moq::Error>(subscriber.remote_broadcast(&subscription))
    })
    .await
    .expect("timeout")
    .expect("subscribe");

    let player = sub
        .play(iroh_live_media::PlayerConfig::default())
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

/// noq publish -> relay -> iroh subscribe (via Live::subscribe).
/// This is the browser->CLI path that fails in the e2e Playwright test.
///
/// Uses `Live::subscribe` which wraps the full catalog + video track pipeline,
/// so this exercises the exact same code path as the real `subscribe_test` binary.
#[tokio::test]
#[serial]
async fn noq_publish_iroh_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id = relay.iroh_id;

    // ── Publisher (noq, simulating browser) ──
    // Publish a broadcast with a hang-compatible catalog and video track.
    let (pub_origin, _pub_driver) = test_origin();
    let broadcast = pub_origin
        .publish("browser-stream", origin::Route::default())
        .expect("bc");

    // hang catalog format: renditions keyed by track name
    let catalog_track = broadcast
        .create_track("catalog.json", None)
        .expect("catalog");
    let catalog_json =
        br#"{"video":{"renditions":{"video/h264":{"codec":"avc1.64001f","codedWidth":320,"codedHeight":240,"bitrate":500000,"framerate":30}}}}"#;
    let mut group = catalog_track.append_group().expect("group");
    group
        .write_frame(Timestamp::ZERO, catalog_json.as_ref())
        .expect("write");
    group.finish().expect("finish");

    let video_track = broadcast.create_track("video/h264", None).expect("video");
    let mut vgroup = video_track.append_group().expect("group");
    vgroup
        .write_frame(Timestamp::ZERO, b"keyframe-data".as_ref())
        .expect("write");
    vgroup.finish().expect("finish");

    let _pub_session = established(
        noq_client()
            .with_publisher(pub_origin.consume())
            .connect(relay.url()),
    )
    .await;

    tokio::time::sleep(Duration::from_secs(1)).await;

    // ── Subscriber (iroh via Live::subscribe) ──
    let sub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind sub");
    shared_lookup().add_endpoint_info(sub_ep.addr());
    let subscriber = iroh_live::Live::builder(sub_ep.clone()).spawn();

    // Retry subscribe a few times: the relay may need time to propagate
    // the noq publisher's announcement to the iroh side.
    let mut last_err = None;
    for attempt in 0..3 {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let session = subscriber.moq().connect_with(relay_id, trusted()).await?;
            let subscription = session.subscribe("browser-stream").await?;
            Ok::<_, iroh_live::moq::Error>(subscriber.remote_broadcast(&subscription))
        })
        .await;

        match result {
            Ok(Ok(sub)) => {
                // Subscribing proves the route; the catalog arriving and
                // parsing proves the bridge carried the broadcast itself.
                let mut catalog = sub.catalog();
                let parsed = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Some(parsed) = catalog.get() {
                            return Some(parsed);
                        }
                        if catalog.updated().await.is_err() {
                            return None;
                        }
                    }
                })
                .await;
                let Ok(Some(parsed)) = parsed else {
                    tracing::warn!(attempt, "subscribed, but no catalog arrived; retrying");
                    last_err = Some("no catalog arrived".to_string());
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                };
                // The one rendition the noq side wrote, parsed on the far
                // side of the bridge.
                let video = parsed.video();
                assert_eq!(video.len(), 1, "the bridged catalog: {video:?}");
                assert_eq!(video[0].name, "video/h264");
                assert_eq!(video[0].height(), Some(240));
                tracing::info!(attempt, "subscribed to browser-stream via iroh");
                // Success: clean up and return.
                drop(sub);
                drop(_pub_session);
                sub_ep.close().await;
                return;
            }
            Ok(Err(e)) => {
                tracing::warn!(attempt, %e, "subscribe attempt failed, retrying");
                last_err = Some(format!("{e:#}"));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(_) => {
                tracing::warn!(attempt, "subscribe attempt timed out, retrying");
                last_err = Some("timeout".into());
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    panic!(
        "noq->iroh subscribe failed after 3 attempts. Last error: {}",
        last_err.unwrap_or_default()
    );
}

/// Pull mode: remote iroh publisher -> relay pulls via ticket -> noq subscriber.
///
/// A publisher runs independently, not connected to the relay. The relay
/// resolves its broadcast from a ticket and mirrors it into the cluster under
/// the ticket's string, where a noq (browser) subscriber finds it and reads it.
#[tokio::test]
#[serial]
async fn pull_remote_broadcast_via_ticket() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, broadcast, ticket) = start_publisher("remote-stream").await;

    let local_name = ticket.to_string();
    let pull_state =
        iroh_live_relay::pull::PullState::new(pull_endpoint().await, relay.cluster.clone())
            .with_linger(PULL_LINGER);
    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");

    // ── Subscriber (noq, simulating browser) ──
    let (sub_origin, _sub_driver) = test_origin();
    let _sub_session = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    // The pulled broadcast is announced under the full ticket string.
    let bc = announced_at(&sub_origin, &local_name).await;
    let _frame = first_frame(&bc, "catalog.json").await;
    tracing::info!("pull mode test: received catalog from pulled broadcast");

    drop(_sub_session);
    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// iroh publish -> relay -> noq subscribe.
/// This is the CLI->browser path (works in Playwright).
#[tokio::test]
#[serial]
async fn iroh_publish_noq_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id = relay.iroh_id;

    // Publisher (iroh via iroh-live)
    let pub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind pub");
    shared_lookup().add_endpoint_info(pub_ep.addr());
    let publisher = iroh_live::Live::builder(pub_ep.clone())
        .with_router()
        .spawn();
    let broadcast = publish_video(&publisher, "cli-stream");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.moq().connect(relay_id))
        .await
        .expect("timeout")
        .expect("connect");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Subscriber (noq)
    let (sub_origin, _sub_driver) = test_origin();
    let _sub_session = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    let named = iroh_live::BroadcastTicket::new(pub_ep.id(), "cli-stream").path();
    announced_at(&sub_origin, named.as_str()).await;

    tracing::info!("noq subscriber received cli-stream announcement");

    drop(_pub_session);
    drop(_sub_session);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// Dials a relay as a direct session whose peer may publish anything here.
///
/// A live node's own grant keeps a peer to `live/<its id>/`, and a relay
/// forwards everyone's broadcasts.
fn trusted() -> iroh_moq::ConnectOptions {
    iroh_moq::ConnectOptions::default().with_grant(iroh_moq::Grant::everything())
}

/// How long a pull may linger unwatched in the pull-lifecycle tests. Short
/// enough to keep them quick, long enough to survive a slow CI scheduler.
const PULL_LINGER: Duration = Duration::from_millis(200);

/// Publishes a generated 320x240 pattern on `broadcast`, as one rendition.
/// Starts a standalone iroh publisher (not connected to the relay) with a video
/// track, and returns it with a ticket naming its broadcast.
async fn start_publisher(
    name: &str,
) -> (
    iroh::Endpoint,
    iroh_live::Live,
    iroh_live_media::LocalBroadcast,
    iroh_live::BroadcastTicket,
) {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind pub");
    shared_lookup().add_endpoint_info(endpoint.addr());

    let live = iroh_live::Live::builder(endpoint.clone())
        .with_router()
        .spawn();
    let broadcast = publish_video(&live, name);
    let ticket = iroh_live::BroadcastTicket::new(endpoint.id(), name);
    (endpoint, live, broadcast, ticket)
}

/// Binds the iroh endpoint a relay dials tickets over.
async fn pull_endpoint() -> iroh::Endpoint {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind pull");
    shared_lookup().add_endpoint_info(endpoint.addr());
    endpoint
}

/// Polls the cluster until `name` is routable (or no longer is), returning
/// whether it got there before [`TIMEOUT`].
///
/// The mirrored broadcast is announced for exactly as long as the pulled session
/// that feeds it is alive, so this is how a test observes that session being
/// dropped without reaching into the relay's internals. A route is what counts:
/// `request_broadcast` resolves optimistically for any covered path, so it
/// cannot tell a live mirror from a stale one.
async fn wait_for_broadcast(cluster: &Cluster, name: &str, present: bool) -> bool {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let consumer = cluster.origin.consume();
    loop {
        let found = tokio::time::timeout(Duration::from_millis(50), consumer.routed(name))
            .await
            .ok()
            .flatten()
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

/// A pull is announced under the name the client asked for, not the ticket's
/// canonical spelling.
///
/// A subscriber is only ever announced the exact path it subscribed to, so the
/// two have to agree. `BroadcastTicket` parses both `iroh-live:<id>/<name>` and the
/// bare `<id>/<name>`, and the bare form is what a person ends up pasting, so
/// mirroring under `ticket.to_string()` served a broadcast nobody had asked
/// for: the browser connected, waited, and was announced nothing, with the
/// relay's own log reporting a successful pull.
#[tokio::test]
#[serial]
async fn a_pull_is_announced_under_the_name_that_was_asked_for() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, broadcast, ticket) = start_publisher("spelling").await;

    let canonical = ticket.to_string();
    let bare = canonical
        .strip_prefix("iroh-live:")
        .expect("a ticket serializes with its scheme")
        .to_owned();
    assert_ne!(bare, canonical);

    let pull_state =
        iroh_live_relay::pull::PullState::new(pull_endpoint().await, relay.cluster.clone())
            .with_linger(PULL_LINGER);

    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&bare, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");
    assert!(
        wait_for_broadcast(&relay.cluster, &bare, true).await,
        "the pull was announced somewhere other than the name that was asked for",
    );

    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A pulled session is owned by nothing in the cluster, so it has to be retired
/// deliberately: once the local session that named the ticket disconnects and
/// nothing is reading the mirrored broadcast, the connection to the publisher is
/// dropped, and pulling the same ticket again dials a fresh one.
///
/// Without that, a relay accumulates one QUIC connection per ticket ever pulled,
/// for as long as each publisher stays up.
#[tokio::test]
#[serial]
async fn pull_retires_an_unwatched_session() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, broadcast, ticket) = start_publisher("retired-stream").await;
    let local_name = ticket.to_string();

    let pull_ep = pull_endpoint().await;
    let pull_id = pull_ep.id();
    let pull_state = iroh_live_relay::pull::PullState::new(pull_ep, relay.cluster.clone())
        .with_linger(PULL_LINGER);

    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");
    assert!(
        wait_for_broadcast(&relay.cluster, &local_name, true).await,
        "the pulled broadcast should be announced in the cluster"
    );

    // The only session that named the ticket is gone and nothing is reading the
    // mirrored broadcast, so the pull has nothing left to serve. Dropping its
    // session takes the mirrored broadcast down with it.
    drop(guard);
    assert!(
        wait_for_broadcast(&relay.cluster, &local_name, false).await,
        "an unwatched pull should be retired"
    );
    // And the relay lets go of the publisher: its session closes, which the
    // publisher sees.
    let mut sessions = publisher.moq().sessions();
    tokio::time::timeout(TIMEOUT, async {
        use n0_watcher::Watcher;
        while sessions
            .get()
            .iter()
            .any(|session| session.remote_id() == pull_id)
        {
            sessions.updated().await.expect("publisher gone");
        }
    })
    .await
    .expect("the relay kept its session with the publisher of a retired pull");

    // The retired entry must not be handed out again: the same ticket dials a
    // new session rather than joining one that is already closed.
    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("re-pull timeout")
        .expect("re-pull");
    assert!(
        wait_for_broadcast(&relay.cluster, &local_name, true).await,
        "pulling a retired ticket should dial the publisher again"
    );

    drop(guard);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A subscriber that reached the mirrored broadcast over some other session
/// holds no pull guard, so the guard count on its own would retire a pull
/// somebody is watching. Demand on the mirrored broadcast is the second signal
/// that keeps it alive, and its ending is what finally retires the pull.
#[tokio::test]
#[serial]
async fn pull_survives_a_reader_holding_no_guard() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, broadcast, ticket) = start_publisher("watched-stream").await;
    let local_name = ticket.to_string();

    let pull_state =
        iroh_live_relay::pull::PullState::new(pull_endpoint().await, relay.cluster.clone())
            .with_linger(PULL_LINGER);

    let guard = tokio::time::timeout(TIMEOUT, pull_state.pull(&local_name, &ticket))
        .await
        .expect("pull timeout")
        .expect("pull");
    assert!(
        wait_for_broadcast(&relay.cluster, &local_name, true).await,
        "the pulled broadcast should be announced in the cluster"
    );

    // Read the mirrored broadcast the way a subscriber session does, without
    // going anywhere near the pull state.
    // Subscribed rather than only holding the track, as a real session does.
    let mirrored = relay
        .cluster
        .origin
        .consume()
        .routed_broadcast(local_name.as_str())
        .await
        .expect("mirrored broadcast");
    let reader = tokio::time::timeout(
        TIMEOUT,
        mirrored
            .track("catalog.json")
            .expect("track")
            .subscribe(None),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe");

    drop(guard);
    tokio::time::sleep(PULL_LINGER * 5).await;
    assert!(
        wait_for_broadcast(&relay.cluster, &local_name, true).await,
        "a pull with a reader must outlive the last guard"
    );

    drop(reader);
    assert!(
        wait_for_broadcast(&relay.cluster, &local_name, false).await,
        "the last reader leaving should retire the pull"
    );

    drop(mirrored);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A node attached to the relay through a relay link publishes into it and
/// consumes from it: its public broadcast reaches a browser at the path that
/// names the node, and a browser's broadcast resolves on the node through the
/// relay, priced above a direct route.
#[tokio::test]
#[serial]
async fn a_relay_link_publishes_and_consumes() {
    use iroh_moq::{LinkKind, Moq, Reach, RelayConfig, RelayStatus};
    use n0_watcher::Watcher;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;

    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind node");
    shared_lookup().add_endpoint_info(endpoint.addr());
    let live = iroh_live::Live::builder(endpoint.clone()).spawn();
    let moq: &Moq = live.moq();

    let url = format!("iroh://{}/", relay.iroh_id).parse().expect("url");
    let link = moq.attach_relay(RelayConfig::new(url)).expect("attach");
    let mut status = link.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get() != RelayStatus::Connected {
            status.updated().await.expect("link gone");
        }
    })
    .await
    .expect("the relay link never connected");

    // Publishing: a browser finds the node's public broadcast at the path
    // that names the node.
    let broadcast = publish_video(&live, "studio");
    let (sub_origin, _sub_driver) = test_origin();
    let _browser = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;
    let path = iroh_live::BroadcastTicket::new(endpoint.id(), "studio").path();
    let seen = announced_at(&sub_origin, path.as_str()).await;
    first_frame(&seen, "catalog.json").await;

    // Consuming: a browser's broadcast resolves through the relay.
    let (pub_origin, _pub_driver) = test_origin();
    let browser_broadcast = pub_origin
        .publish("browser-stream", origin::Route::default())
        .expect("broadcast");
    let track = browser_broadcast.create_track("data", None).expect("track");
    let mut group = track.append_group().expect("group");
    group
        .write_frame(Timestamp::ZERO, b"from-the-browser".as_ref())
        .expect("write");
    group.finish().expect("finish");
    let _browser_publisher = established(
        noq_client()
            .with_publisher(pub_origin.consume())
            .connect(relay.url()),
    )
    .await;

    let subscription =
        tokio::time::timeout(TIMEOUT, moq.subscribe("browser-stream", Reach::Relays))
            .await
            .expect("subscribe timeout")
            .expect("subscribe through the relay");
    let frame = first_frame(&subscription.as_moq(), "data").await;
    assert_eq!(&frame.payload[..], b"from-the-browser");
    let routes = moq.routes("browser-stream").get();
    assert!(
        routes.iter().any(
            |route| route.kind == LinkKind::Relay && route.cost >= iroh_moq::DEFAULT_RELAY_COST
        ),
        "{routes:?}"
    );

    // Detaching withdraws what the relay taught the route table.
    link.detach().await;
    assert_eq!(link.status().get(), RelayStatus::Detached);
    let mut routes = moq.routes("browser-stream");
    tokio::time::timeout(TIMEOUT, async {
        while !routes.get().is_empty() {
            routes.updated().await.expect("node gone");
        }
    })
    .await
    .expect("the relay's routes outlived the link");

    drop(broadcast);
    live.shutdown().await;
}

/// Binds a node for the relay-link tests, accepting sessions if `router`.
async fn relay_node(router: bool) -> (iroh::Endpoint, iroh_live::Live) {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind node");
    shared_lookup().add_endpoint_info(endpoint.addr());
    let mut builder = iroh_live::Live::builder(endpoint.clone());
    if router {
        builder = builder.with_router();
    }
    (endpoint, builder.spawn())
}

/// Attaches `live` to `relay` and waits for the link to connect.
async fn attached(
    live: &iroh_live::Live,
    relay: &TestRelay,
    offer: iroh_moq::RelayOffer,
) -> iroh_moq::RelayLink {
    use n0_watcher::Watcher;
    let url = format!("iroh://{}/", relay.iroh_id).parse().expect("url");
    let link = live
        .moq()
        .attach_relay(iroh_moq::RelayConfig::new(url).with_offer(offer))
        .expect("attach");
    let mut status = link.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get() != iroh_moq::RelayStatus::Connected {
            status.updated().await.expect("link gone");
        }
    })
    .await
    .expect("the relay link never connected");
    link
}

/// A broadcast with one track that writes a counter every few milliseconds.
fn counter(name: &str) -> (moq_net::broadcast::Producer, AbortOnDropHandle<()>) {
    let broadcast = moq_net::broadcast::Info::new().produce();
    let mut track = broadcast
        .create_track(
            name,
            moq_net::track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    let writer = AbortOnDropHandle::new(tokio::spawn(async move {
        for n in 0u64.. {
            if track
                .write_frame(Timestamp::now(), n.to_be_bytes().to_vec())
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }));
    (broadcast, writer)
}

/// Reports whether `origin` routes `path` within a second.
async fn routed_soon(origin: &origin::Producer, path: &str) -> bool {
    tokio::time::timeout(Duration::from_secs(1), origin.consume().routed(path))
        .await
        .ok()
        .flatten()
        .is_some()
}

/// A subscription a relay serves carries the relay link's readings, and a
/// player adapts on them.
///
/// Every relay link runs a connection monitor like a direct session's, and the
/// facade reads whichever link serves the subscription, so a player behind a
/// relay sees a round trip rather than holding its first rendition on no
/// information at all.
#[tokio::test]
#[serial]
async fn a_relay_served_subscription_carries_link_samples() {
    use iroh_live_media::PlayerConfig;
    use iroh_moq::{LinkKind, Reach, RelayOffer};

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (_publisher_endpoint, publisher) = relay_node(false).await;
    let _publisher_link = attached(&publisher, &relay, RelayOffer::Public).await;
    let _broadcast = publish_video(&publisher, "studio");

    let (_viewer_endpoint, viewer) = relay_node(false).await;
    let link = attached(&viewer, &relay, RelayOffer::Nothing).await;
    let ticket = publisher.ticket("studio");
    let subscription = tokio::time::timeout(
        TIMEOUT,
        viewer.moq().subscribe(ticket.path(), Reach::Relays),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe through the relay");

    // The transport: the relay link serves the path, and its monitor has
    // measured the link.
    let serving = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(serving) = subscription.link()
                && serving.sample.rtt.is_some()
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
    assert!(serving.sample.min_rtt.is_some(), "{serving:?}");
    assert!(link.link().rtt.is_some(), "{:?}", link.link());

    // The facade: the player's network signals carry the same readings.
    let player = viewer
        .remote_broadcast(&subscription)
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

/// Only public publications go to a relay, and only while the link offers
/// them: a `Peers` or `Manual` one never reaches it, and a link that offers
/// nothing publishes nothing.
#[tokio::test]
#[serial]
async fn a_relay_gets_public_publications_only() {
    use std::collections::BTreeSet;

    use iroh_moq::{Audience, RelayOffer};
    use n0_watcher::Watchable;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (endpoint, live) = relay_node(false).await;
    let (public, _public) = counter("data");
    let (peers, _peers) = counter("data");
    let (manual, _manual) = counter("data");
    let members = Watchable::new(BTreeSet::from([relay.iroh_id]));
    let public = live
        .moq()
        .publish(live.ticket("public").path(), &public, Audience::Everyone)
        .expect("publish");
    let peers = live
        .moq()
        .publish(
            live.ticket("peers").path(),
            &peers,
            Audience::Peers(members.watch()),
        )
        .expect("publish");
    let manual = live
        .moq()
        .publish(live.ticket("manual").path(), &manual, Audience::Manual)
        .expect("publish");

    let (sub_origin, _sub_driver) = test_origin();
    let _browser = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    let nothing = attached(&live, &relay, RelayOffer::Nothing).await;
    assert!(
        !routed_soon(&sub_origin, public.path().as_str()).await,
        "a link that offers nothing published a broadcast"
    );
    nothing.detach().await;

    let _public_link = attached(&live, &relay, RelayOffer::Public).await;
    announced_at(&sub_origin, public.path().as_str()).await;
    for private in [&peers, &manual] {
        assert!(
            !routed_soon(&sub_origin, private.path().as_str()).await,
            "{} reached the relay",
            private.path()
        );
    }

    live.shutdown().await;
    drop(endpoint);
}

/// Shutting a node down reports its relay links detached, and a subscribe
/// that wants a relay fails once none is attached.
#[tokio::test]
#[serial]
async fn shutdown_detaches_relay_links() {
    use iroh_moq::{Error, Reach, RelayOffer, RelayStatus};
    use n0_watcher::Watcher;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (_endpoint, live) = relay_node(false).await;
    let link = attached(&live, &relay, RelayOffer::Public).await;
    let consume_only = live
        .moq()
        .attach_relay(iroh_moq::RelayConfig::new(link.url().clone()).with_consume(false))
        .expect("attach");

    link.detach().await;
    // The link left feeds the table nothing, so a relay subscribe has nowhere
    // to wait.
    let err = tokio::time::timeout(TIMEOUT, live.moq().subscribe("anything", Reach::Relays))
        .await
        .expect("a relay subscribe with no consuming relay waited")
        .expect_err("resolved without a relay");
    assert!(matches!(err, Error::NoRoute { .. }), "{err:#}");

    let mut status = consume_only.status();
    tokio::time::timeout(TIMEOUT, live.shutdown())
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

/// A subscription served by a direct session ends when that session goes, and
/// asking again resolves the path through the relay.
///
/// It does not move over on its own: a relay route cannot vouch that it comes
/// from the publisher the direct session authenticated, so the node keeps the
/// two apart (see `a_relay_cannot_splice_a_forgery_into_a_direct_subscription`).
#[tokio::test]
#[serial]
async fn losing_the_direct_session_moves_a_subscription_to_the_relay() {
    use iroh_moq::{Audience, LinkKind, Reach, RelayOffer};
    use n0_watcher::Watcher;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (_alice_endpoint, alice) = relay_node(true).await;
    let (broadcast, _writer) = counter("data");
    let publication = alice
        .moq()
        .publish(alice.ticket("cam").path(), &broadcast, Audience::Everyone)
        .expect("publish");
    let _alice_link = attached(&alice, &relay, RelayOffer::Public).await;

    let (_bob_endpoint, bob) = relay_node(false).await;
    let subscription = tokio::time::timeout(
        TIMEOUT,
        bob.moq()
            .subscribe(publication.path(), Reach::Direct(alice.endpoint().id())),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe");
    let mut reader = subscribed(&subscription.as_moq()).await;
    let _bob_link = attached(&bob, &relay, RelayOffer::Nothing).await;
    let mut routes = bob.moq().routes(publication.path());
    tokio::time::timeout(TIMEOUT, async {
        while !routes
            .get()
            .iter()
            .any(|route| route.kind == LinkKind::Relay)
        {
            routes.updated().await.expect("node gone");
        }
    })
    .await
    .expect("the relay never routed the publication");

    subscription
        .session()
        .expect("served directly")
        .close("testing failover");
    tokio::time::timeout(TIMEOUT, async {
        while let Ok(Some(_)) = reader.recv_group().await {}
    })
    .await
    .expect("the subscription outlived its direct session");

    let again = tokio::time::timeout(
        TIMEOUT,
        bob.moq().subscribe(publication.path(), Reach::Relays),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe through the relay");
    subscribed(&again.as_moq()).await;
    let routes = bob.moq().routes(publication.path()).get();
    assert!(
        routes
            .iter()
            .any(|route| route.kind == LinkKind::Relay && route.active),
        "{routes:?}"
    );

    bob.shutdown().await;
    alice.shutdown().await;
}

/// Subscribes to the counter track of `broadcast` and reads one group.
async fn subscribed(broadcast: &moq_net::broadcast::Consumer) -> moq_net::track::Subscriber {
    let mut reader = tokio::time::timeout(
        TIMEOUT,
        broadcast.track("data").expect("track").subscribe(
            moq_net::track::Subscription::default().with_max_age(Duration::from_secs(5)),
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

/// The hop a node with endpoint id `id` announces under, as `iroh-moq` derives
/// it: public, so anyone can compute it.
fn hop_of(id: iroh::EndpointId) -> moq_net::Hop {
    let bytes: [u8; 8] = id.as_bytes()[..8].try_into().expect("32 bytes");
    let value = u64::from_le_bytes(bytes) & ((1u64 << 53) - 1);
    moq_net::Hop::new(value.max(1)).expect("a valid hop")
}

/// A peer that publishes Alice's path into a relay under Alice's own hop is
/// never spliced into a subscription Bob holds over his direct session with
/// Alice, not even once that session goes.
///
/// The test relay lets anyone publish anywhere, as a relay with loose
/// admission would; the node must hold on its own.
#[tokio::test]
#[serial]
async fn a_relay_cannot_splice_a_forgery_into_a_direct_subscription() {
    use iroh_moq::{Audience, LinkKind, Reach, RelayOffer};
    use n0_watcher::Watcher;

    const FORGED: u64 = 1_000_000;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (alice_endpoint, alice) = relay_node(true).await;
    let (broadcast, _writer) = counter("data");
    let publication = alice
        .moq()
        .publish(alice.ticket("cam").path(), &broadcast, Audience::Everyone)
        .expect("publish");

    // Mallory, a browser-like client, publishes at Alice's path, declaring
    // Alice's hop.
    let (mallory, mallory_driver) =
        origin::Producer::new(origin::Config::new(hop_of(alice_endpoint.id())));
    let _mallory_driver = AbortOnDropHandle::new(tokio::spawn(async move {
        moq_net::time::run(mallory_driver).await;
    }));
    let forged = mallory
        .publish(publication.path().as_str(), origin::Route::default())
        .expect("forged broadcast");
    let mut forged_track = forged
        .create_track(
            "data",
            moq_net::track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    let _forger = AbortOnDropHandle::new(tokio::spawn(async move {
        for n in FORGED.. {
            if forged_track
                .write_frame(Timestamp::now(), n.to_be_bytes().to_vec())
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

    let (_bob_endpoint, bob) = relay_node(false).await;
    let subscription = tokio::time::timeout(
        TIMEOUT,
        bob.moq()
            .subscribe(publication.path(), Reach::Direct(alice.endpoint().id())),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe");
    let mut reader = subscribed(&subscription.as_moq()).await;
    let _bob_link = attached(&bob, &relay, RelayOffer::Nothing).await;
    let mut routes = bob.moq().routes(publication.path());
    tokio::time::timeout(TIMEOUT, async {
        while !routes
            .get()
            .iter()
            .any(|route| route.kind == LinkKind::Relay)
        {
            routes.updated().await.expect("node gone");
        }
    })
    .await
    .expect("the relay never routed the forged path");

    subscription
        .session()
        .expect("served directly")
        .close("the forger's chance");
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

/// The shipped relay keeps every publisher to the paths that name it: an iroh
/// client cannot publish under another client's id, and a browser cannot
/// publish into `live/` or `rooms/` at all, while each still publishes what is
/// its own.
#[tokio::test]
#[serial]
async fn the_shipped_relay_refuses_forged_paths() {
    use iroh_moq::{Audience, RelayOffer};

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start_shipped().await;
    let (sub_origin, _sub_driver) = test_origin();
    let _viewer = established(
        noq_client()
            .with_subscriber(sub_origin.clone())
            .connect(relay.url()),
    )
    .await;

    let alice = iroh::SecretKey::generate().public();
    let (mallory_endpoint, mallory) = relay_node(false).await;
    let (own, _own) = counter("data");
    let (forged, _forged) = counter("data");
    let (room_forged, _room_forged) = counter("data");
    let own = mallory
        .moq()
        .publish(mallory.ticket("cam").path(), &own, Audience::Everyone)
        .expect("publish");
    let forged_path = format!("live/{alice}/cam");
    let room_path = format!("rooms/topic/{alice}/cam");
    let _forged = mallory
        .moq()
        .publish(forged_path.as_str(), &forged, Audience::Everyone)
        .expect("publish at alice's path");
    let _room_forged = mallory
        .moq()
        .publish(room_path.as_str(), &room_forged, Audience::Everyone)
        .expect("publish at alice's room path");
    let _link = attached(&mallory, &relay, RelayOffer::Public).await;

    // A browser tries the same, and publishes a name of its own.
    let (browser, _browser_driver) = test_origin();
    let browser_forged = browser
        .publish(
            format!("live/{alice}/screen").as_str(),
            origin::Route::default(),
        )
        .expect("broadcast");
    let browser_own = browser
        .publish("browser-stream", origin::Route::default())
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
    let (_viewer_endpoint, viewer) = relay_node(false).await;
    let _viewer_link = attached(&viewer, &relay, RelayOffer::Nothing).await;
    let track = browser_own.create_track("data", None).expect("track");
    let mut group = track.append_group().expect("group");
    group
        .write_frame(Timestamp::ZERO, b"from-the-browser".as_ref())
        .expect("write");
    group.finish().expect("finish");
    let subscription = tokio::time::timeout(
        TIMEOUT,
        viewer
            .moq()
            .subscribe("browser-stream", iroh_moq::Reach::Relays),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe through the relay");
    let frame = first_frame(&subscription.as_moq(), "data").await;
    assert_eq!(&frame.payload[..], b"from-the-browser");

    drop((browser_forged, browser_own));
    viewer.shutdown().await;
    mallory.shutdown().await;
    drop(mallory_endpoint);
}

/// Two pulls of one publisher share the relay's session with it: retiring
/// one leaves the other reading, and the session closes only with the last.
#[tokio::test]
#[serial]
async fn a_publisher_session_outlives_all_but_its_last_pull() {
    use n0_watcher::Watcher;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (pub_ep, publisher, first_broadcast, first) = start_publisher("first").await;
    let second_broadcast = publish_video(&publisher, "second");
    let second = iroh_live::BroadcastTicket::new(pub_ep.id(), "second");
    let (first_name, second_name) = (first.to_string(), second.to_string());

    let pull_ep = pull_endpoint().await;
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
            wait_for_broadcast(&relay.cluster, name, true).await,
            "{name} was never mirrored"
        );
    }
    let pull_sessions = || {
        publisher
            .moq()
            .sessions()
            .get()
            .iter()
            .filter(|session| session.remote_id() == pull_id)
            .count()
    };
    assert_eq!(
        pull_sessions(),
        1,
        "two pulls of one publisher, two sessions"
    );

    drop(first_guard);
    assert!(
        wait_for_broadcast(&relay.cluster, &first_name, false).await,
        "the first pull was not retired"
    );
    let mirrored = relay
        .cluster
        .origin
        .consume()
        .routed_broadcast(second_name.as_str())
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
    let mut sessions = publisher.moq().sessions();
    tokio::time::timeout(TIMEOUT, async {
        while sessions
            .get()
            .iter()
            .any(|session| session.remote_id() == pull_id)
        {
            sessions.updated().await.expect("publisher gone");
        }
    })
    .await
    .expect("the session outlived the last pull");

    drop((first_broadcast, second_broadcast));
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// A node with rooms, accepting both MoQ and the rooms' gossip.
async fn room_node() -> (
    iroh::Endpoint,
    iroh_moq::Moq,
    iroh_live_rooms::Rooms,
    iroh::protocol::Router,
) {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind node");
    shared_lookup().add_endpoint_info(endpoint.addr());
    let moq = iroh_moq::Moq::new(endpoint.clone(), iroh_live::moq_config());
    let rooms = iroh_live_rooms::Rooms::new(&moq);
    let mut router = iroh::protocol::Router::builder(endpoint.clone());
    for alpn in iroh_moq::alpns() {
        router = router.accept(alpn, moq.clone());
    }
    let router = router
        .accept(iroh_live_rooms::ALPN, rooms.protocol_handler())
        .spawn();
    (endpoint, moq, rooms, router)
}

/// A room reads a member's broadcast over the session with that member, so a
/// forgery of it that a relay routes into the member's own path never
/// reaches the room, even though the forged route sits in the route table
/// before the member joins.
#[tokio::test]
#[serial]
async fn a_relay_cannot_forge_a_room_members_broadcast() {
    use iroh_moq::{LinkKind, RelayConfig, RelayOffer, RelayStatus};
    use n0_watcher::Watcher;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (alice_endpoint, alice_moq, alice_rooms, alice_router) = room_node().await;
    let (bob_endpoint, bob_moq, bob_rooms, bob_router) = room_node().await;
    let room_a = alice_rooms
        .join(
            &iroh_live_rooms::RoomTicket::generate(),
            iroh_live_rooms::RoomConfig::default().with_display_name("alice"),
        )
        .await
        .expect("join");
    let topic = room_a.ticket().topic_id();
    let cam_path = format!("rooms/{topic}/{}/cam", bob_endpoint.id());

    // Mallory publishes Bob's camera into the relay before Bob is there.
    let (mallory, _mallory_driver) = test_origin();
    let forged = mallory
        .publish(cam_path.as_str(), origin::Route::default())
        .expect("forged broadcast");
    let mut track = forged
        .create_track(
            "data",
            moq_net::track::Info::default().with_max_age(Duration::from_secs(5)),
        )
        .expect("track");
    let _forger = AbortOnDropHandle::new(tokio::spawn(async move {
        while track.write_frame(Timestamp::now(), &b"forged"[..]).is_ok() {
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
    let url = format!("iroh://{}/", relay.iroh_id).parse().expect("url");
    let link = alice_moq
        .attach_relay(RelayConfig::new(url).with_offer(RelayOffer::Nothing))
        .expect("attach");
    let mut status = link.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get() != RelayStatus::Connected {
            status.updated().await.expect("link gone");
        }
    })
    .await
    .expect("the relay link never connected");
    let mut routes = alice_moq.routes(cam_path.as_str());
    tokio::time::timeout(TIMEOUT, async {
        while !routes
            .get()
            .iter()
            .any(|route| route.kind == LinkKind::Relay)
        {
            routes.updated().await.expect("node gone");
        }
    })
    .await
    .expect("the forged camera never reached alice's table");

    let room_b = bob_rooms
        .join(
            &room_a.ticket(),
            iroh_live_rooms::RoomConfig::default().with_display_name("bob"),
        )
        .await
        .expect("join");
    let (cam, _writer) = counter("data");
    room_b.publish("cam", &cam).expect("publish");
    let subscription = tokio::time::timeout(TIMEOUT, room_a.subscribe(bob_endpoint.id(), "cam"))
        .await
        .expect("subscribe timeout")
        .expect("subscribe");
    let frame = first_frame(&subscription.as_moq(), "data").await;
    assert_ne!(
        &frame.payload[..],
        b"forged",
        "the relay's forgery reached the room"
    );

    room_b.leave().await;
    room_a.leave().await;
    bob_moq.shutdown().await;
    alice_moq.shutdown().await;
    bob_router.shutdown().await.expect("router");
    alice_router.shutdown().await.expect("router");
    drop((alice_endpoint, link));
}
