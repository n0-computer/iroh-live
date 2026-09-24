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
    _cluster_task: AbortOnDropHandle<()>,
    cluster: Cluster,
    noq_addr: std::net::SocketAddr,
    iroh_id: iroh::EndpointId,
}

impl TestRelay {
    /// Starts a relay wired the way `iroh_live_relay::run` wires one.
    ///
    /// A cluster unannounces a broadcast the moment it loses its last source,
    /// which is what the pull-lifecycle tests below observe. It used to linger
    /// for five seconds unless told otherwise; moq removed the knob along with
    /// the delay.
    async fn start() -> Self {
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
        server_config.iroh = Some(iroh.clone());
        let server = server_config.init().expect("init server");
        let client = connect
            .clone()
            .init(quic)
            .expect("init client")
            .with_iroh(iroh);

        let mut auth_config = moq_relay::auth::Config::default();
        auth_config.public = vec![moq_net::Pattern::all()];
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
    let broadcast =
        iroh_live::LocalBroadcast::new(moq_net::broadcast::Info::new().produce()).expect("create");
    broadcast
        .video()
        .set(iroh_live_media::test_source::video(
            iroh_live_media::video::Size::new(320, 240),
            30,
        ))
        .expect("set video");
    live.publish(name, broadcast.consume()).expect("publish");
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
        let session = subscriber.moq().connect(relay_id).await?;
        let subscription = session.subscribe(path).await?;
        subscriber.remote_broadcast(&subscription).await
    })
    .await
    .expect("timeout")
    .expect("subscribe");

    assert!(sub.has_video());
    let video = tokio::time::timeout(TIMEOUT, sub.video())
        .await
        .expect("timeout")
        .expect("video track");
    let frame = tokio::time::timeout(Duration::from_secs(10), video.frames().recv())
        .await
        .expect("timeout")
        .expect("closed");
    let size = frame.size();
    assert!(size.width > 0 && size.height > 0);

    drop(video);
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
            let session = subscriber.moq().connect(relay_id).await?;
            let subscription = session.subscribe("browser-stream").await?;
            subscriber.remote_broadcast(&subscription).await
        })
        .await;

        match result {
            Ok(Ok(sub)) => {
                tracing::info!(
                    attempt,
                    has_video = sub.has_video(),
                    has_audio = sub.has_audio(),
                    "subscribed to browser-stream via iroh"
                );
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

    // At the path that names the publisher, and, for one release, at the bare
    // name a browser on the older layout asks for.
    let named = iroh_live::BroadcastTicket::new(pub_ep.id(), "cli-stream").path();
    announced_at(&sub_origin, named.as_str()).await;
    announced_at(&sub_origin, "cli-stream").await;

    tracing::info!("noq subscriber received cli-stream announcement");

    drop(_pub_session);
    drop(_sub_session);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
}

/// How long a pull may linger unwatched in the pull-lifecycle tests. Short
/// enough to keep them quick, long enough to survive a slow CI scheduler.
const PULL_LINGER: Duration = Duration::from_millis(200);

/// Starts a standalone iroh publisher (not connected to the relay) with a video
/// track, and returns it with a ticket naming its broadcast.
async fn start_publisher(
    name: &str,
) -> (
    iroh::Endpoint,
    iroh_live::Live,
    iroh_live_media::publish::LocalBroadcast,
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
    use iroh_moq::{LinkKind, LinkStatus, Moq, Reach, RelayConfig};
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
        while status.get() != LinkStatus::Connected {
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
    assert_eq!(link.status().get(), LinkStatus::Detached);
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
        while status.get() != iroh_moq::LinkStatus::Connected {
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
        .publish("public", &public, Audience::Everyone)
        .expect("publish");
    let peers = live
        .moq()
        .publish("peers", &peers, Audience::Peers(members.watch()))
        .expect("publish");
    let manual = live
        .moq()
        .publish("manual", &manual, Audience::Manual)
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
    use iroh_moq::{Error, LinkStatus, Reach, RelayOffer};
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
        while status.get() != LinkStatus::Detached {
            status.updated().await.expect("link gone");
        }
    })
    .await
    .expect("a relay link outlived its node's shutdown");
}

/// A subscription follows its publisher from a direct session to a relay when
/// the direct session goes, without ending: both routes start at the same
/// publisher, so moq re-splices at a group boundary.
#[tokio::test]
#[serial]
async fn a_subscription_fails_over_from_direct_to_the_relay() {
    use iroh_moq::{Audience, LinkKind, Reach, RelayOffer};
    use n0_watcher::Watcher;

    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let (_alice_endpoint, alice) = relay_node(true).await;
    let (broadcast, _writer) = counter("data");
    let publication = alice
        .moq()
        .publish("cam", &broadcast, Audience::Everyone)
        .expect("publish");
    let _alice_link = attached(&alice, &relay, RelayOffer::Public).await;

    let (_bob_endpoint, bob) = relay_node(false).await;
    let subscription = tokio::time::timeout(
        TIMEOUT,
        bob.moq().subscribe(publication.path(), Reach::Direct),
    )
    .await
    .expect("subscribe timeout")
    .expect("subscribe");
    let mut reader = tokio::time::timeout(
        TIMEOUT,
        subscription
            .as_moq()
            .track("data")
            .expect("track")
            .subscribe(
                moq_net::track::Subscription::default().with_max_age(Duration::from_secs(5)),
            ),
    )
    .await
    .expect("track subscribe timeout")
    .expect("track subscribe");
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

    let session = subscription.session().expect("served directly");
    session.close("testing failover");
    // Groups keep coming, well past the ones in flight when the session went.
    for _ in 0..100 {
        tokio::time::timeout(TIMEOUT, reader.recv_group())
            .await
            .expect("the subscription stalled after the direct session went")
            .expect("track failed")
            .expect("the subscription ended with the direct session");
    }
    assert!(!subscription.as_moq().is_closed());
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
