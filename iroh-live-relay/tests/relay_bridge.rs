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
use moq_net::{Origin, Path, Timestamp, broadcast};
use moq_relay::{PublicConfig, PublicDetailed};
use serial_test::serial;

const TIMEOUT: Duration = Duration::from_secs(10);

static ADDRESS_LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();

/// Returns the shared MemoryLookup, creating it if needed.
fn shared_lookup() -> MemoryLookup {
    ADDRESS_LOOKUP.get_or_init(Default::default).clone()
}

/// Starts a relay (noq server + iroh endpoint + cluster) and returns handles.
struct TestRelay {
    server_handle: tokio::task::JoinHandle<()>,
    cluster: moq_relay::Cluster,
    noq_addr: std::net::SocketAddr,
    iroh_id: Option<String>,
}

impl TestRelay {
    async fn start() -> Self {
        let mut server_config = moq_native::ServerConfig::default();
        server_config.bind = Some("[::]:0".parse().unwrap());
        server_config.backend = Some(moq_native::QuicBackend::Noq);
        server_config.tls.generate = vec!["localhost".into()];
        server_config.quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);

        let mut client_config = moq_native::ClientConfig::default();
        client_config.quic.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);
        let client_tls = client_config.tls.clone();

        // Build the relay's iroh endpoint with Minimal preset + MemoryLookup
        // instead of IrohEndpointConfig (which uses presets::N0 and real DNS
        // discovery). This makes tests reliable in CI without network access.
        let mut alpns: Vec<Vec<u8>> = moq_net::ALPNS
            .iter()
            .map(|alpn| alpn.as_bytes().to_vec())
            .collect();
        alpns.push(b"h3".to_vec());

        let iroh = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .address_lookup(shared_lookup())
            .secret_key(iroh::SecretKey::generate())
            .alpns(alpns)
            .bind()
            .await
            .expect("bind relay iroh");

        shared_lookup().add_endpoint_info(iroh.addr());
        let iroh_id = Some(iroh.id().to_string());

        let server = server_config.init().expect("init server");
        let client = client_config.init().expect("init client");

        let mut server = server.with_iroh(iroh.clone());
        let client = client.with_iroh(iroh);
        let noq_addr = server.local_addr().expect("get noq addr");

        let mut auth_config = moq_relay::AuthConfig::default();
        let prefixes = vec!["".to_string()];
        auth_config.public = Some(PublicConfig::Detailed(PublicDetailed {
            subscribe: prefixes.clone(),
            publish: prefixes,
            api: None,
        }));
        let auth = auth_config.init(&client_tls).await.expect("init auth");

        let cluster = moq_relay::Cluster::new(moq_relay::ClusterConfig::default())
            .expect("init cluster")
            .with_client(client);
        let cluster_handle = cluster.clone();
        tokio::spawn(async move {
            cluster_handle.run().await.expect("cluster failed");
        });

        let auth_clone = auth;
        let cluster_clone = cluster.clone();
        let server_handle = tokio::spawn(async move {
            let mut conn_id = 0u64;
            while let Some(request) = server.accept().await {
                let conn = moq_relay::Connection {
                    id: conn_id,
                    request,
                    cluster: cluster_clone.clone(),
                    auth: auth_clone.clone(),
                };
                conn_id += 1;
                tokio::spawn(async move {
                    if let Err(err) = conn.run().await {
                        tracing::warn!(%err, "relay conn closed");
                    }
                });
            }
        });

        Self {
            server_handle,
            cluster,
            noq_addr,
            iroh_id,
        }
    }
}

/// Baseline: noq publish → relay → noq subscribe.
#[tokio::test]
#[serial]
async fn noq_publish_noq_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;

    // Publisher
    let pub_origin = Origin::random().produce();
    let mut broadcast = pub_origin
        .create_broadcast("test", broadcast::Route::announced())
        .expect("create bc");
    let mut track = broadcast.create_track("video", None).expect("track");
    let mut group = track.append_group().expect("group");
    group
        .write_frame(Timestamp::ZERO, b"hello-noq".as_ref())
        .expect("write");
    group.finish().expect("finish");

    let mut pub_cfg = moq_native::ClientConfig::default();
    pub_cfg.tls.disable_verify = Some(true);
    pub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let pub_client = pub_cfg.init().expect("init pub");
    let pub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let pub_client = pub_client.with_publisher(pub_origin.consume());
    let _pub_session = tokio::time::timeout(TIMEOUT, pub_client.connect(pub_url))
        .await
        .expect("timeout")
        .expect("connect");

    // Subscriber
    let sub_origin = Origin::random().produce();
    let mut announcements = sub_origin.consume().announced();
    let mut sub_cfg = moq_native::ClientConfig::default();
    sub_cfg.tls.disable_verify = Some(true);
    sub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let sub_client = sub_cfg.init().expect("init sub");
    let sub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let sub_client = sub_client.with_subscriber(sub_origin);
    let _sub_session = tokio::time::timeout(TIMEOUT, sub_client.connect(sub_url))
        .await
        .expect("timeout")
        .expect("connect");

    let update = tokio::time::timeout(TIMEOUT, announcements.next())
        .await
        .expect("timeout")
        .expect("closed");
    assert_eq!(update.path.as_str(), "test");
    let bc = update.broadcast.expect("announce");
    let track_sub = bc.track("video").expect("sub");
    let mut track_sub = tokio::time::timeout(TIMEOUT, track_sub.subscribe(None))
        .await
        .expect("timeout")
        .expect("subscribe");
    let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.recv_group())
        .await
        .expect("timeout")
        .expect("err")
        .expect("closed");
    let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
        .await
        .expect("timeout")
        .expect("err")
        .expect("closed");
    assert_eq!(&frame.payload[..], b"hello-noq");

    relay.server_handle.abort();
}

/// iroh publish → relay → iroh subscribe (using iroh-live Live API).
#[tokio::test]
#[serial]
async fn iroh_publish_iroh_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id: iroh::EndpointId = relay.iroh_id.as_ref().expect("no iroh").parse().unwrap();

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
    let broadcast = publisher.publish("relay-test").expect("publish");
    broadcast
        .video()
        .set(moq_media::test_source::video(
            moq_media::video::Size::new(320, 240),
            30,
        ))
        .expect("set video");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.transport().connect(relay_id))
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
    let sub = tokio::time::timeout(TIMEOUT, subscriber.subscribe(relay_id, "relay-test"))
        .await
        .expect("timeout")
        .expect("subscribe");

    assert!(sub.broadcast().has_video());
    let video = tokio::time::timeout(TIMEOUT, sub.broadcast().video())
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
    relay.server_handle.abort();
}

/// noq publish → relay → iroh subscribe (via Live::subscribe).
/// This is the browser→CLI path that fails in the e2e Playwright test.
///
/// Uses `Live::subscribe` which wraps the full catalog + video track pipeline,
/// so this exercises the exact same code path as the real `subscribe_test` binary.
#[tokio::test]
#[serial]
async fn noq_publish_iroh_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id: iroh::EndpointId = relay.iroh_id.as_ref().expect("no iroh").parse().unwrap();

    // ── Publisher (noq, simulating browser) ──
    // Publish a broadcast with a hang-compatible catalog and video track.
    let pub_origin = Origin::random().produce();
    let mut broadcast = pub_origin
        .create_broadcast("browser-stream", broadcast::Route::announced())
        .expect("bc");

    // hang catalog format: renditions keyed by track name
    let mut catalog_track = broadcast
        .create_track("catalog.json", None)
        .expect("catalog");
    let catalog_json =
        br#"{"video":{"renditions":{"video/h264":{"codec":"avc1.64001f","codedWidth":320,"codedHeight":240,"bitrate":500000,"framerate":30}}}}"#;
    let mut group = catalog_track.append_group().expect("group");
    group
        .write_frame(Timestamp::ZERO, catalog_json.as_ref())
        .expect("write");
    group.finish().expect("finish");

    let mut video_track = broadcast.create_track("video/h264", None).expect("video");
    let mut vgroup = video_track.append_group().expect("group");
    vgroup
        .write_frame(Timestamp::ZERO, b"keyframe-data".as_ref())
        .expect("write");
    vgroup.finish().expect("finish");

    let mut pub_cfg = moq_native::ClientConfig::default();
    pub_cfg.tls.disable_verify = Some(true);
    pub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let pub_client = pub_cfg.init().expect("init pub");
    let pub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let pub_client = pub_client.with_publisher(pub_origin.consume());
    let _pub_session = tokio::time::timeout(TIMEOUT, pub_client.connect(pub_url))
        .await
        .expect("timeout")
        .expect("connect");

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

    // Retry subscribe a few times — the relay may need time to propagate
    // the noq publisher's announcement to the iroh side.
    let mut last_err = None;
    for attempt in 0..3 {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            subscriber.subscribe(relay_id, "browser-stream"),
        )
        .await;

        match result {
            Ok(Ok(sub)) => {
                tracing::info!(
                    attempt,
                    has_video = sub.broadcast().has_video(),
                    has_audio = sub.broadcast().has_audio(),
                    "subscribed to browser-stream via iroh"
                );
                // Success — clean up and return.
                drop(sub);
                drop(_pub_session);
                sub_ep.close().await;
                relay.server_handle.abort();
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
        "noq→iroh subscribe failed after 3 attempts. Last error: {}",
        last_err.unwrap_or_default()
    );
}

/// Pull mode: remote iroh publisher → relay pulls via ticket → noq subscriber.
///
/// This tests the relay's pull mode: a publisher is running independently
/// (not connected to the relay). The relay connects to it via an iroh-live
/// ticket, subscribes to its broadcast, and makes it available to noq
/// (browser) subscribers.
///
/// Mirrors what `iroh_live_relay::pull::PullState` does internally (that
/// module is crate-private, so this test drives the same moq-net APIs
/// directly): a MoQ session dialed with a subscriber origin scoped to the
/// ticket's one broadcast and re-rooted to its local name.
#[tokio::test]
#[serial]
async fn pull_remote_broadcast_via_ticket() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;

    // ── Publisher (standalone iroh, NOT connected to relay) ──
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
    let broadcast = publisher.publish("remote-stream").expect("publish");
    broadcast
        .video()
        .set(moq_media::test_source::video(
            moq_media::video::Size::new(320, 240),
            30,
        ))
        .expect("set video");

    // Give publisher time to start producing frames.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create a ticket for this publisher.
    let ticket = iroh_live::ticket::LiveTicket::new(pub_ep.addr(), "remote-stream");

    // -- Pull: relay connects to publisher and mirrors the broadcast --
    let pull_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind pull");
    shared_lookup().add_endpoint_info(pull_ep.addr());

    let local_name = ticket.to_string();
    let prefix = local_name
        .split_once('/')
        .map_or(local_name.as_str(), |(prefix, _)| prefix);
    let subscriber = relay
        .cluster
        .origin
        .with_root(prefix)
        .and_then(|origin| origin.scope(&[Path::new(&ticket.broadcast_name)]))
        .expect("scope pull origin");

    let connection = tokio::time::timeout(
        TIMEOUT,
        pull_ep.connect(ticket.endpoint.clone(), iroh_moq::ALPN),
    )
    .await
    .expect("pull connect timeout")
    .expect("pull connect");
    let transport = web_transport_iroh::Session::raw(connection);
    let (pull_session, pull_driver) = tokio::time::timeout(
        TIMEOUT,
        moq_net::Client::new()
            .with_subscriber(subscriber)
            .connect(transport),
    )
    .await
    .expect("pull handshake timeout")
    .expect("pull handshake");
    tokio::spawn(pull_driver);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // ── Subscriber (noq, simulating browser) ──
    let sub_origin = Origin::random().produce();
    let mut announcements = sub_origin.consume().announced();
    let mut sub_cfg = moq_native::ClientConfig::default();
    sub_cfg.tls.disable_verify = Some(true);
    sub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let sub_client = sub_cfg.init().expect("init sub");
    let sub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let sub_client = sub_client.with_subscriber(sub_origin);
    let _sub_session = tokio::time::timeout(TIMEOUT, sub_client.connect(sub_url))
        .await
        .expect("timeout")
        .expect("connect");

    // Should see the pulled broadcast announced.
    let update = tokio::time::timeout(TIMEOUT, announcements.next())
        .await
        .expect("announce timeout — pull mode may not work")
        .expect("closed");
    // The pulled broadcast is published under the full ticket string.
    assert!(
        update.path.as_str().starts_with("iroh-live:"),
        "expected ticket-shaped name, got: {}",
        update.path
    );
    let bc = update.broadcast.expect("announce");

    // Subscribe to a track and verify data arrives.
    let catalog_track = bc.track("catalog.json").expect("catalog sub");
    let mut catalog_track = tokio::time::timeout(TIMEOUT, catalog_track.subscribe(None))
        .await
        .expect("catalog subscribe timeout")
        .expect("catalog subscribe");
    let mut group = tokio::time::timeout(TIMEOUT, catalog_track.recv_group())
        .await
        .expect("catalog group timeout")
        .expect("catalog group err")
        .expect("catalog group closed");
    let _frame = tokio::time::timeout(TIMEOUT, group.read_frame())
        .await
        .expect("catalog frame timeout")
        .expect("catalog frame err")
        .expect("catalog frame closed");
    tracing::info!("pull mode test: received catalog from pulled broadcast");

    // Cleanup.
    drop(_sub_session);
    drop(pull_session);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
    relay.server_handle.abort();
}

/// iroh publish → relay → noq subscribe.
/// This is the CLI→browser path (works in Playwright).
#[tokio::test]
#[serial]
async fn iroh_publish_noq_subscribe() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;
    let relay_id: iroh::EndpointId = relay.iroh_id.as_ref().expect("no iroh").parse().unwrap();

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
    let broadcast = publisher.publish("cli-stream").expect("publish");
    broadcast
        .video()
        .set(moq_media::test_source::video(
            moq_media::video::Size::new(320, 240),
            30,
        ))
        .expect("set video");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.transport().connect(relay_id))
        .await
        .expect("timeout")
        .expect("connect");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Subscriber (noq)
    let sub_origin = Origin::random().produce();
    let mut announcements = sub_origin.consume().announced();
    let mut sub_cfg = moq_native::ClientConfig::default();
    sub_cfg.tls.disable_verify = Some(true);
    sub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let sub_client = sub_cfg.init().expect("init sub");
    let sub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let sub_client = sub_client.with_subscriber(sub_origin);
    let _sub_session = tokio::time::timeout(TIMEOUT, sub_client.connect(sub_url))
        .await
        .expect("timeout")
        .expect("connect");

    let update = tokio::time::timeout(TIMEOUT, announcements.next())
        .await
        .expect("announce timeout — iroh→noq bridging may not work")
        .expect("closed");
    assert_eq!(update.path.as_str(), "cli-stream");

    tracing::info!("noq subscriber received cli-stream announcement");

    drop(_pub_session);
    drop(_sub_session);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
    relay.server_handle.abort();
}
