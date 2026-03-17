//! Integration tests for the iroh-live relay bridging.
//!
//! These tests exercise the relay's ability to bridge broadcasts between
//! different transport backends (noq/WebTransport and iroh P2P), verifying
//! that data published on one transport is visible to subscribers on another.

use std::time::Duration;

use moq_native::moq_lite::{Origin, Track};
use serial_test::serial;

const TIMEOUT: Duration = Duration::from_secs(10);

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
        server_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);

        let mut client_config = moq_native::ClientConfig::default();
        client_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);

        let mut iroh_config = moq_native::IrohEndpointConfig::default();
        iroh_config.enabled = Some(true);

        let server = server_config.init().expect("init server");
        let client = client_config.init().expect("init client");
        let iroh = iroh_config.bind().await.expect("bind iroh");

        let iroh_id = iroh.as_ref().map(|ep| ep.id().to_string());

        let (mut server, client) = (
            server.with_iroh(iroh.clone()),
            client.with_iroh(iroh.clone()),
        );
        let noq_addr = server.local_addr().expect("get noq addr");

        let auth_config = moq_relay::AuthConfig {
            public: Some(String::new()),
            ..Default::default()
        };
        let auth = auth_config.init().await.expect("init auth");

        let cluster = moq_relay::Cluster::new(moq_relay::ClusterConfig::default(), client);
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
    let pub_origin = Origin::produce();
    let mut broadcast = pub_origin.create_broadcast("test").expect("create bc");
    let mut track = broadcast.create_track(Track::new("video")).expect("track");
    let mut group = track.append_group().expect("group");
    group.write_frame(b"hello-noq".as_ref()).expect("write");
    group.finish().expect("finish");

    let mut pub_cfg = moq_native::ClientConfig::default();
    pub_cfg.tls.disable_verify = Some(true);
    pub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let pub_client = pub_cfg.init().expect("init pub");
    let pub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let pub_client = pub_client.with_publish(pub_origin.consume());
    let _pub_session = tokio::time::timeout(TIMEOUT, pub_client.connect(pub_url))
        .await
        .expect("timeout")
        .expect("connect");

    // Subscriber
    let sub_origin = Origin::produce();
    let mut announcements = sub_origin.consume();
    let mut sub_cfg = moq_native::ClientConfig::default();
    sub_cfg.tls.disable_verify = Some(true);
    sub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let sub_client = sub_cfg.init().expect("init sub");
    let sub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let sub_client = sub_client.with_consume(sub_origin);
    let _sub_session = tokio::time::timeout(TIMEOUT, sub_client.connect(sub_url))
        .await
        .expect("timeout")
        .expect("connect");

    let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
        .await
        .expect("timeout")
        .expect("closed");
    assert_eq!(path.as_str(), "test");
    let bc = bc.expect("announce");
    let mut track_sub = bc.subscribe_track(&Track::new("video")).expect("sub");
    let mut group_sub = tokio::time::timeout(TIMEOUT, track_sub.next_group())
        .await
        .expect("timeout")
        .expect("err")
        .expect("closed");
    let frame = tokio::time::timeout(TIMEOUT, group_sub.read_frame())
        .await
        .expect("timeout")
        .expect("err")
        .expect("closed");
    assert_eq!(&*frame, b"hello-noq");

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
    let pub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::generate(&mut rand::rng()))
        .bind()
        .await
        .expect("bind pub");
    let publisher = iroh_live::Live::builder(pub_ep.clone()).spawn_with_router();
    let broadcast = moq_media::publish::LocalBroadcast::new();
    tokio::task::yield_now().await;
    let source = moq_media::test_util::TestVideoSource::new(320, 240).with_fps(30.0);
    broadcast
        .video()
        .set(
            source,
            moq_media::codec::VideoCodec::best_available().expect("no codec"),
            [moq_media::format::VideoPreset::P180],
        )
        .expect("set video");
    publisher
        .publish("relay-test", &broadcast)
        .await
        .expect("publish");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.transport().connect(relay_id))
        .await
        .expect("timeout")
        .expect("connect");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Subscriber
    let sub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::generate(&mut rand::rng()))
        .bind()
        .await
        .expect("bind sub");
    let subscriber = iroh_live::Live::builder(sub_ep.clone()).spawn();
    let (_session, remote) =
        tokio::time::timeout(TIMEOUT, subscriber.subscribe(relay_id, "relay-test"))
            .await
            .expect("timeout")
            .expect("subscribe");

    assert!(remote.has_video());
    let mut video = remote.video().expect("video track");
    let frame = tokio::time::timeout(Duration::from_secs(10), video.next_frame())
        .await
        .expect("timeout")
        .expect("closed");
    assert!(frame.width() > 0 && frame.height() > 0);

    drop(video);
    drop(remote);
    drop(_session);
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
    let pub_origin = Origin::produce();
    let mut broadcast = pub_origin.create_broadcast("browser-stream").expect("bc");

    // hang catalog format: renditions keyed by track name
    let mut catalog_track = broadcast
        .create_track(Track::new("catalog.json"))
        .expect("catalog");
    let catalog_json =
        br#"{"video":{"renditions":{"video/h264":{"codec":"avc1.64001f","codedWidth":320,"codedHeight":240,"bitrate":500000,"framerate":30}}}}"#;
    let mut group = catalog_track.append_group().expect("group");
    group.write_frame(catalog_json.as_ref()).expect("write");
    group.finish().expect("finish");

    let mut video_track = broadcast
        .create_track(Track::new("video/h264"))
        .expect("video");
    let mut vgroup = video_track.append_group().expect("group");
    vgroup
        .write_frame(b"keyframe-data".as_ref())
        .expect("write");
    vgroup.finish().expect("finish");

    let mut pub_cfg = moq_native::ClientConfig::default();
    pub_cfg.tls.disable_verify = Some(true);
    pub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let pub_client = pub_cfg.init().expect("init pub");
    let pub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let pub_client = pub_client.with_publish(pub_origin.consume());
    let _pub_session = tokio::time::timeout(TIMEOUT, pub_client.connect(pub_url))
        .await
        .expect("timeout")
        .expect("connect");

    tokio::time::sleep(Duration::from_secs(1)).await;

    // ── Subscriber (iroh via Live::subscribe) ──
    let sub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::generate(&mut rand::rng()))
        .bind()
        .await
        .expect("bind sub");
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
            Ok(Ok((_session, remote))) => {
                tracing::info!(
                    attempt,
                    has_video = remote.has_video(),
                    has_audio = remote.has_audio(),
                    "subscribed to browser-stream via iroh"
                );
                // Success — clean up and return.
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
#[tokio::test]
#[serial]
async fn pull_remote_broadcast_via_ticket() {
    let _ = tracing_subscriber::fmt::try_init();
    let relay = TestRelay::start().await;

    // ── Publisher (standalone iroh, NOT connected to relay) ──
    let pub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::generate(&mut rand::rng()))
        .bind()
        .await
        .expect("bind pub");
    let publisher = iroh_live::Live::builder(pub_ep.clone()).spawn_with_router();
    let broadcast = moq_media::publish::LocalBroadcast::new();
    tokio::task::yield_now().await;
    let source = moq_media::test_util::TestVideoSource::new(320, 240).with_fps(30.0);
    broadcast
        .video()
        .set(
            source,
            moq_media::codec::VideoCodec::best_available().expect("no codec"),
            [moq_media::format::VideoPreset::P180],
        )
        .expect("set video");
    publisher
        .publish("remote-stream", &broadcast)
        .await
        .expect("publish");

    // Give publisher time to start producing frames.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create a ticket for this publisher.
    let ticket = iroh_live::ticket::LiveTicket::new(pub_ep.addr(), "remote-stream");

    // ── Pull: relay connects to publisher and injects broadcast ──
    // This simulates what the relay's pull module does. Uses a separate
    // iroh 0.97 endpoint (the relay's moq-native uses iroh 0.96).
    let pull_ep = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
        .await
        .expect("bind pull");
    let pull_live = iroh_live::Live::new(pull_ep);
    let mut pull_session = tokio::time::timeout(
        TIMEOUT,
        pull_live.transport().connect(ticket.endpoint.clone()),
    )
    .await
    .expect("pull connect timeout")
    .expect("pull connect");

    let consumer = tokio::time::timeout(TIMEOUT, pull_session.subscribe(&ticket.broadcast_name))
        .await
        .expect("pull subscribe timeout")
        .expect("pull subscribe");

    // Inject into the relay's cluster.
    relay
        .cluster
        .primary
        .publish_broadcast(&ticket.broadcast_name, consumer);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // ── Subscriber (noq, simulating browser) ──
    let sub_origin = Origin::produce();
    let mut announcements = sub_origin.consume();
    let mut sub_cfg = moq_native::ClientConfig::default();
    sub_cfg.tls.disable_verify = Some(true);
    sub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let sub_client = sub_cfg.init().expect("init sub");
    let sub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let sub_client = sub_client.with_consume(sub_origin);
    let _sub_session = tokio::time::timeout(TIMEOUT, sub_client.connect(sub_url))
        .await
        .expect("timeout")
        .expect("connect");

    // Should see the pulled broadcast announced.
    let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
        .await
        .expect("announce timeout — pull mode may not work")
        .expect("closed");
    assert_eq!(path.as_str(), "remote-stream");
    let bc = bc.expect("announce");

    // Subscribe to a track and verify data arrives.
    let catalog_track = bc
        .subscribe_track(&Track::new("catalog.json"))
        .expect("catalog sub");
    let mut group = tokio::time::timeout(TIMEOUT, catalog_track.next_group())
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
    let pub_ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::generate(&mut rand::rng()))
        .bind()
        .await
        .expect("bind pub");
    let publisher = iroh_live::Live::builder(pub_ep.clone()).spawn_with_router();
    let broadcast = moq_media::publish::LocalBroadcast::new();
    tokio::task::yield_now().await;
    let source = moq_media::test_util::TestVideoSource::new(320, 240).with_fps(30.0);
    broadcast
        .video()
        .set(
            source,
            moq_media::codec::VideoCodec::best_available().expect("no codec"),
            [moq_media::format::VideoPreset::P180],
        )
        .expect("set video");
    publisher
        .publish("cli-stream", &broadcast)
        .await
        .expect("publish");

    let _pub_session = tokio::time::timeout(TIMEOUT, publisher.transport().connect(relay_id))
        .await
        .expect("timeout")
        .expect("connect");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Subscriber (noq)
    let sub_origin = Origin::produce();
    let mut announcements = sub_origin.consume();
    let mut sub_cfg = moq_native::ClientConfig::default();
    sub_cfg.tls.disable_verify = Some(true);
    sub_cfg.backend = Some(moq_native::QuicBackend::Noq);
    let sub_client = sub_cfg.init().expect("init sub");
    let sub_url: url::Url = format!("https://localhost:{}", relay.noq_addr.port())
        .parse()
        .unwrap();
    let sub_client = sub_client.with_consume(sub_origin);
    let _sub_session = tokio::time::timeout(TIMEOUT, sub_client.connect(sub_url))
        .await
        .expect("timeout")
        .expect("connect");

    let (path, bc) = tokio::time::timeout(TIMEOUT, announcements.announced())
        .await
        .expect("announce timeout — iroh→noq bridging may not work")
        .expect("closed");
    assert_eq!(path.as_str(), "cli-stream");
    let _bc = bc.expect("announce");
    tracing::info!("noq subscriber received cli-stream announcement");

    drop(_pub_session);
    drop(_sub_session);
    drop(broadcast);
    publisher.shutdown().await;
    pub_ep.close().await;
    relay.server_handle.abort();
}
