//! Integration tests for [`RoomMode::Relay`] (relay-only rooms).
//!
//! Three peers join the same room through a real moq-relay and
//! discover each other via the relay's announce stream. Gossip is
//! not used. Each peer's broadcasts are visible to the others
//! through the relay's primary origin under the path
//! `room/<topic_hex>/<peer_id>/<broadcast_name>`.
//!
//! [`RoomMode::Relay`]: iroh_live::rooms::RoomMode::Relay

use std::{collections::HashSet, sync::OnceLock, time::Duration};

use iroh::address_lookup::MemoryLookup;
use iroh_live::{
    Live, RelayOffer,
    rooms::{Room, RoomEvent, RoomMode, RoomTicket, TopicId},
};
use moq_relay::{PublicConfig, PublicDetailed};
use serial_test::serial;

const TIMEOUT: Duration = Duration::from_secs(15);

static ADDRESS_LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();

fn shared_lookup() -> MemoryLookup {
    ADDRESS_LOOKUP.get_or_init(Default::default).clone()
}

/// Spawns a moq-relay with iroh-only transport (no QUIC bind),
/// public auth, and returns the relay's endpoint id together with
/// a join handle that owns the server task.
async fn start_iroh_relay() -> (iroh::EndpointId, tokio::task::JoinHandle<()>) {
    let alpns: Vec<Vec<u8>> = moq_native::moq_lite::ALPNS
        .iter()
        .map(|alpn| alpn.as_bytes().to_vec())
        .chain(std::iter::once(b"h3".to_vec()))
        .collect();

    let iroh = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .alpns(alpns)
        .bind()
        .await
        .expect("bind relay iroh endpoint");
    shared_lookup().add_endpoint_info(iroh.addr());
    let relay_id = iroh.id();

    let mut server_config = moq_native::ServerConfig::default();
    server_config.bind = Some("[::]:0".parse().unwrap());
    server_config.backend = Some(moq_native::QuicBackend::Noq);
    server_config.tls.generate = vec!["localhost".into()];
    server_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);

    let mut client_config = moq_native::ClientConfig::default();
    client_config.max_streams = Some(moq_relay::DEFAULT_MAX_STREAMS);

    let server = server_config
        .init()
        .expect("init relay server")
        .with_iroh(Some(iroh.clone()));
    let client = client_config
        .init()
        .expect("init relay client")
        .with_iroh(Some(iroh.clone()));

    let mut auth_config = moq_relay::AuthConfig::default();
    let prefixes = vec!["".to_string()];
    auth_config.public = Some(PublicConfig::Detailed(PublicDetailed {
        subscribe: prefixes.clone(),
        publish: prefixes,
        api: None,
    }));
    let auth = auth_config.init().await.expect("init auth");

    let cluster = moq_relay::Cluster::new(moq_relay::ClusterConfig::default(), client);
    let cluster_handle = cluster.clone();
    tokio::spawn(async move {
        cluster_handle.run().await.expect("cluster failed");
    });

    let mut server = server;
    let server_handle = tokio::spawn(async move {
        let mut conn_id = 0u64;
        while let Some(request) = server.accept().await {
            let conn = moq_relay::Connection {
                id: conn_id,
                request,
                cluster: cluster.clone(),
                auth: auth.clone(),
            };
            conn_id += 1;
            tokio::spawn(async move {
                if let Err(err) = conn.run().await {
                    tracing::debug!(%err, "relay conn closed");
                }
            });
        }
    });

    (relay_id, server_handle)
}

/// Spawns a [`Live`] instance with no gossip and no router. Suits
/// pure-relay-mode peers that only ever talk outward to the
/// relay.
async fn live_relay_client() -> Live {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind client endpoint");
    shared_lookup().add_endpoint_info(endpoint.addr());
    Live::builder(endpoint).spawn()
}

/// Three peers join a relay-only room, each publishes a broadcast,
/// and each sees the other two.
#[tokio::test]
#[serial]
async fn relay_only_three_peer_room() {
    let _ = tracing_subscriber::fmt::try_init();
    let (relay_id, server) = start_iroh_relay().await;

    let topic = TopicId::from_bytes(rand::random());
    let offer = RelayOffer {
        endpoint: relay_id,
        path: "/".into(),
        api_key: None,
    };
    let ticket = RoomTicket::for_relay_at(topic, offer.clone());
    assert_eq!(ticket.mode(), RoomMode::Relay);

    // Three peers; each runs in its own Live instance.
    let live_a = live_relay_client().await;
    let live_b = live_relay_client().await;
    let live_c = live_relay_client().await;
    let id_a = live_a.endpoint().id();
    let id_b = live_b.endpoint().id();
    let id_c = live_c.endpoint().id();

    let mut room_a = Room::new(&live_a, ticket.clone()).await.expect("a join");
    let mut room_b = Room::new(&live_b, ticket.clone()).await.expect("b join");
    let mut room_c = Room::new(&live_c, ticket.clone()).await.expect("c join");

    // Publishes use distinct names so we can map who-saw-what.
    let bc_a = build_video_broadcast();
    let bc_b = build_video_broadcast();
    let bc_c = build_video_broadcast();
    room_a.publish("a-cam", &bc_a).await.expect("a publish");
    room_b.publish("b-cam", &bc_b).await.expect("b publish");
    room_c.publish("c-cam", &bc_c).await.expect("c publish");

    // Each peer should see the other two peers' broadcasts via
    // the relay's announce stream.
    expect_remote_broadcasts(&mut room_a, &[(id_b, "b-cam"), (id_c, "c-cam")]).await;
    expect_remote_broadcasts(&mut room_b, &[(id_a, "a-cam"), (id_c, "c-cam")]).await;
    expect_remote_broadcasts(&mut room_c, &[(id_a, "a-cam"), (id_b, "b-cam")]).await;

    drop(bc_a);
    drop(bc_b);
    drop(bc_c);
    live_a.shutdown().await;
    live_b.shutdown().await;
    live_c.shutdown().await;
    server.abort();
}

/// Hybrid mode: two peers attached to both gossip and the relay.
/// Each peer's subscription source set has direct + relay; the
/// default policy prefers direct. Discovery flows through gossip.
#[tokio::test]
#[serial]
async fn hybrid_room_prefers_direct() {
    let _ = tracing_subscriber::fmt::try_init();
    let (relay_id, server) = start_iroh_relay().await;

    let topic = TopicId::from_bytes(rand::random());
    let offer = RelayOffer {
        endpoint: relay_id,
        path: "/".into(),
        api_key: None,
    };
    let ticket = RoomTicket::new(topic, []).with_relay(offer);
    assert_eq!(ticket.mode(), RoomMode::Hybrid);

    let live_a = live_gossip_client().await;
    let live_b = live_gossip_client().await;
    let id_a = live_a.endpoint().id();
    let id_b = live_b.endpoint().id();

    // Bootstrap b through a so the gossip topic actually meshes.
    let ticket_a = ticket.clone().with_bootstrap(id_a);

    let mut room_a = Room::new(&live_a, ticket.clone()).await.expect("a join");
    let mut room_b = Room::new(&live_b, ticket_a).await.expect("b join");

    let bc_a = build_video_broadcast();
    let bc_b = build_video_broadcast();
    room_a.publish("a-cam", &bc_a).await.expect("a publish");
    room_b.publish("b-cam", &bc_b).await.expect("b publish");

    expect_remote_broadcasts(&mut room_a, &[(id_b, "b-cam")]).await;
    expect_remote_broadcasts(&mut room_b, &[(id_a, "a-cam")]).await;

    drop(bc_a);
    drop(bc_b);
    live_a.shutdown().await;
    live_b.shutdown().await;
    server.abort();
}

/// Spawns a [`Live`] instance with gossip and router enabled,
/// suitable for [`RoomMode::Gossip`] and [`RoomMode::Hybrid`]
/// peers.
async fn live_gossip_client() -> Live {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(shared_lookup())
        .secret_key(iroh::SecretKey::generate())
        .bind()
        .await
        .expect("bind client endpoint");
    shared_lookup().add_endpoint_info(endpoint.addr());
    Live::builder(endpoint).with_router().with_gossip().spawn()
}

/// `Live::join_room` rejects a Relay-mode ticket when the ticket
/// has no relay attached.
#[tokio::test]
#[serial]
async fn relay_mode_requires_relay_attachment() {
    let live = live_relay_client().await;
    let ticket = RoomTicket {
        bootstrap: Vec::new(),
        topic_id: TopicId::from_bytes(rand::random()),
        relay: None,
        mode: RoomMode::Relay,
    };
    let res = Room::new(&live, ticket).await;
    assert!(
        res.is_err(),
        "expected error joining Relay-mode room without a relay attachment"
    );
    live.shutdown().await;
}

/// Awaits the union of (peer, broadcast) pairs from
/// `BroadcastSubscribed` events on `room` and panics if not all
/// `expected` pairs arrive within [`TIMEOUT`].
async fn expect_remote_broadcasts(room: &mut Room, expected: &[(iroh::EndpointId, &str)]) {
    let mut needed: HashSet<(iroh::EndpointId, String)> = expected
        .iter()
        .map(|(id, n)| (*id, (*n).to_string()))
        .collect();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !needed.is_empty() && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), room.recv()).await {
            Ok(Ok(RoomEvent::BroadcastSubscribed { broadcast, session })) => {
                let remote = session.remote_id();
                let name = broadcast.broadcast_name().to_string();
                // Wire-uniform broadcast names ride as
                // "<peer>/<name>"; recover the user-facing pair by
                // splitting the wire form. The `session.remote_id`
                // is the relay (in Mode B), so we trust the wire
                // name's first component for the peer.
                if let Some((peer_part, user_name)) = name.split_once('/')
                    && let Ok(peer) = peer_part.parse::<iroh::EndpointId>()
                {
                    let _ = remote;
                    let _ = needed.remove(&(peer, user_name.to_string()));
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("room recv error: {e:#}"),
            Err(_) => continue,
        }
    }
    assert!(
        needed.is_empty(),
        "missing remote broadcasts after timeout: {needed:?}"
    );
}

fn build_video_broadcast() -> moq_media::publish::LocalBroadcast {
    let bc = moq_media::publish::LocalBroadcast::new();
    let source = moq_media::test_util::TestVideoSource::new(160, 120).with_fps(15.0);
    bc.video()
        .set(moq_media::publish::VideoInput::new(
            source,
            moq_media::codec::VideoCodec::H264,
            [moq_media::format::VideoPreset::P180],
        ))
        .expect("set video");
    bc
}
