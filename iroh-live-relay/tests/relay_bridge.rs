//! The relay's bridging, pulls and admission, over real connections.
//!
//! Endpoints use `presets::Minimal` and a shared `MemoryLookup`, so no test
//! needs network discovery.

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

/// A relay: noq server, iroh endpoint and cluster.
///
/// Its tasks end with it, so a test that panics takes its relay along.
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
    /// Starts a relay that lets anyone publish anywhere.
    ///
    /// The forgery tests use it as the relay a node must not trust.
    async fn start() -> Self {
        Self::start_with(false).await
    }

    /// Starts a relay with the admission `iroh_live_relay::run` uses.
    async fn start_shipped() -> Self {
        Self::start_with(true).await
    }

    async fn start_with(shipped: bool) -> Self {
        let mut quic = moq_tokio::quic::Config::default();
        quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
        let connect = moq_tokio::connect::Config::default();

        let alpns = iroh_moq::alpns().into_iter().map(<[u8]>::to_vec).collect();

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

/// Builds a noq client that trusts the relay's self-signed certificate.
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
    let pub_origin = moq_tokio::origin::spawn();
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

/// noq publish -> relay -> iroh subscribe through `Live::subscribe`.
#[tokio::test]
#[serial]
async fn noq_publish_iroh_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id = relay.iroh_id;

    // Publisher: noq, standing in for a browser, with a hang catalog and a
    // video track.
    let pub_origin = moq_tokio::origin::spawn();
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

    // Subscriber: iroh, through `Live::subscribe`.
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
                let video = &parsed.video.renditions;
                assert_eq!(video.len(), 1, "the bridged catalog: {video:?}");
                assert_eq!(video["video/h264"].coded_height, Some(240));
                tracing::info!(attempt, "subscribed to browser-stream via iroh");
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

/// A noq subscriber reads a broadcast the relay pulls from a ticket.
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

    // Subscriber: noq, standing in for a browser.
    let sub_origin = moq_tokio::origin::spawn();
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
    let sub_origin = moq_tokio::origin::spawn();
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
    iroh_moq::ConnectOptions {
        grant: Some(iroh_moq::Grant::everything()),
        ..Default::default()
    }
}

/// How long an unwatched pull lingers in these tests.
const PULL_LINGER: Duration = Duration::from_millis(200);

/// Starts an iroh publisher with a video broadcast, and returns a ticket to it.
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

/// Waits until `name` is routable in the cluster, or no longer is, within [`TIMEOUT`].
///
/// A route is what counts: `request_broadcast` resolves any covered path, live or not.
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

/// A pull is announced under the name the client asked for.
///
/// A subscriber is announced only the exact path it subscribed to.
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

/// An unwatched pull retires and closes its connection, and the next pull dials anew.
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

    // Nothing wants the pull any more, so it retires and its mirror goes.
    drop(guard);
    assert!(
        wait_for_broadcast(&relay.cluster, &local_name, false).await,
        "an unwatched pull should be retired"
    );
    // The relay closes its session with the publisher.
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

    // The same ticket dials a new session.
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

/// A reader that holds no pull guard keeps the pull alive through demand.
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

    // Read the mirror as a subscriber session does, holding no pull guard.
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

/// A relay link publishes the node's public broadcasts and consumes the relay's.
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
    let sub_origin = moq_tokio::origin::spawn();
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
    let pub_origin = moq_tokio::origin::spawn();
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
    let serving = subscription.link().expect("a serving link");
    assert_eq!(serving.kind, LinkKind::Relay);
    // Detaching withdraws what the relay taught the route table.
    let mut updates = moq.origin().announced();
    let route = tokio::time::timeout(TIMEOUT, async {
        loop {
            let update = updates.next().await.expect("origin closed");
            if update.prefix.as_str() == "browser-stream" {
                return update.route;
            }
        }
    })
    .await
    .expect("the relay's route never reached the table");
    assert!(route.cost.warm >= iroh_moq::DEFAULT_RELAY_COST, "{route:?}");
    link.detach().await;
    assert_eq!(link.status().get(), RelayStatus::Detached);
    retracted(&mut updates, "browser-stream").await;

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
        .attach_relay(iroh_moq::RelayConfig {
            offer,
            ..iroh_moq::RelayConfig::new(url)
        })
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

/// Waits until `origin` has a route to `path`.
async fn routed(origin: &origin::Consumer, path: &str) {
    tokio::time::timeout(TIMEOUT, origin.routed(path))
        .await
        .unwrap_or_else(|_| panic!("{path} was never routed"))
        .expect("the origin closed");
}

/// Waits until `updates` retracts `path`.
async fn retracted(updates: &mut moq_net::announce::Consumer, path: &str) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let update = updates.next().await.expect("origin closed");
            if update.prefix.as_str() == path && !update.kind.is_active() {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{path} was never retracted"));
}

/// Reports whether `origin` routes `path` within a second.
async fn routed_soon(origin: &origin::Producer, path: &str) -> bool {
    tokio::time::timeout(Duration::from_secs(1), origin.consume().routed(path))
        .await
        .ok()
        .flatten()
        .is_some()
}

/// A relay-served subscription carries the relay link's readings to the player.
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

/// Only public publications reach a relay, and only on a link that offers them.
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

    let sub_origin = moq_tokio::origin::spawn();
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

/// Shutting a node down detaches its relay links.
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
        .attach_relay(iroh_moq::RelayConfig {
            consume: false,
            ..iroh_moq::RelayConfig::new(link.url().clone())
        })
        .expect("attach");

    link.detach().await;
    // The detached link feeds nothing, so a relay subscribe fails.
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

/// A subscription ends with its direct session, and asking again finds the relay.
///
/// It does not move over on its own, since a relay route cannot vouch for the publisher.
#[tokio::test]
#[serial]
async fn losing_the_direct_session_moves_a_subscription_to_the_relay() {
    use iroh_moq::{Audience, LinkKind, Reach, RelayOffer};

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
    let bob_link = attached(&bob, &relay, RelayOffer::Nothing).await;
    routed(&bob_link.origin(), publication.path().as_str()).await;

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
    assert_eq!(
        again.link().map(|link| link.kind),
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

/// The hop iroh-moq derives for `id`, which anyone can compute.
fn hop_of(id: iroh::EndpointId) -> moq_net::Hop {
    let bytes: [u8; 8] = id.as_bytes()[..8].try_into().expect("32 bytes");
    let value = u64::from_le_bytes(bytes) & ((1u64 << 53) - 1);
    moq_net::Hop::new(value.max(1)).expect("a valid hop")
}

/// A forgery of Alice's path under her hop never reaches Bob's direct subscription.
///
/// Not even once the direct session goes. The test relay lets anyone publish anywhere.
#[tokio::test]
#[serial]
async fn a_relay_cannot_splice_a_forgery_into_a_direct_subscription() {
    use iroh_moq::{Audience, Reach, RelayOffer};

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
    let bob_link = attached(&bob, &relay, RelayOffer::Nothing).await;
    routed(&bob_link.origin(), publication.path().as_str()).await;

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

/// The shipped relay keeps iroh clients to their own paths, browsers to one segment.
#[tokio::test]
#[serial]
async fn the_shipped_relay_refuses_forged_paths() {
    use iroh_moq::{Audience, RelayOffer};

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
    let browser = moq_tokio::origin::spawn();
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

/// Two pulls of one publisher share a session, which closes with the last.
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
    let router = moq
        .mount(iroh::protocol::Router::builder(endpoint.clone()))
        .accept(iroh_live_rooms::ALPN, rooms.protocol_handler())
        .spawn();
    (endpoint, moq, rooms, router)
}

/// A forged room broadcast that a relay routes never reaches the room.
///
/// The room reads each member over its own session, even with the forgery routed first.
#[tokio::test]
#[serial]
async fn a_relay_cannot_forge_a_room_members_broadcast() {
    use iroh_moq::{RelayConfig, RelayOffer, RelayStatus};
    use n0_watcher::Watcher;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (alice_endpoint, alice_moq, alice_rooms, alice_router) = room_node().await;
    let (bob_endpoint, bob_moq, bob_rooms, bob_router) = room_node().await;
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
        .attach_relay(RelayConfig {
            offer: RelayOffer::Nothing,
            ..RelayConfig::new(url)
        })
        .expect("attach");
    let mut status = link.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get() != RelayStatus::Connected {
            status.updated().await.expect("link gone");
        }
    })
    .await
    .expect("the relay link never connected");
    routed(&alice_moq.origin(), cam_path.as_str()).await;

    let room_b = bob_rooms
        .join(&room_a.ticket(), Some("bob".into()))
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
