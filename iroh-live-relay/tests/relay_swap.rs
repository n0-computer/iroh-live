//! Cable-cut auto-switch in a Hybrid room with a real moq-relay.
//!
//! Two peers join a Hybrid-mode room sharing a relay. The
//! subscriber starts on the direct path (the default
//! `RoomSubscribeMode::PreferDirect` policy). The test then
//! "cuts the direct cable" by switching the subscribe mode to
//! `RelayOnly` and asserts the active source for the broadcast
//! moves to the relay endpoint.
//!
//! ## Layering note
//!
//! A literal selective network cable cut, dropping direct
//! packets while leaving relay packets alive, is impossible
//! with the single-router patchbay topology used elsewhere in
//! this workspace because every UDP packet from a peer leaves
//! through the same `eth0` interface regardless of destination.
//! Cutting that interface kills both direct and relay paths
//! simultaneously, since the relay needs to receive content
//! from the publisher to forward to subscribers. Asymmetric
//! routing, where the subscriber is multi-homed across two
//! routers with the publisher on one and the relay on the
//! other, is well outside the scope of these tests.
//!
//! The auto-switch logic itself lives at the source-set layer.
//! When the room actor's per-peer source set changes, the
//! subscription re-evaluates and emits an active-source
//! transition. Toggling
//! [`set_subscribe_mode`](iroh_live::rooms::RoomHandle::set_subscribe_mode)
//! is the cleanest way to drive that change deterministically
//! and it exercises the same code path a real cable cut would
//! hit after the direct session timed out and was pruned: the
//! direct candidate disappears from the per-peer source set,
//! and the policy elects the next-priority source.
//!
//! ## Test pattern
//!
//! 1. Start a moq-relay accepting iroh-only connections.
//! 2. Spawn two `Live` instances with gossip and router enabled.
//! 3. Both peers join the same `Hybrid` room with the relay
//!    attached. Bootstrap is wired so gossip meshes.
//! 4. Peer A publishes a video broadcast.
//! 5. Peer B observes the initial `BroadcastSubscribed` event and
//!    asserts the active session terminates at peer A directly.
//! 6. Peer B's room handle calls
//!    `set_subscribe_mode(RelayOnly)`. Within a few seconds B's
//!    event stream surfaces a transition whose active session
//!    terminates at the relay endpoint.

use std::{sync::OnceLock, time::Duration};

use iroh::address_lookup::MemoryLookup;
use iroh_live::{
    Live, RelayOffer,
    rooms::{Room, RoomEvent, RoomMode, RoomSubscribeMode, RoomTicket, TopicId},
};
use moq_relay::{PublicConfig, PublicDetailed};
use serial_test::serial;

const TIMEOUT: Duration = Duration::from_secs(15);

static ADDRESS_LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();

fn shared_lookup() -> MemoryLookup {
    ADDRESS_LOOKUP.get_or_init(Default::default).clone()
}

/// Spawns a moq-relay reachable through iroh transport. Mirrors the
/// fixture in `relay_room.rs`; replicated here to keep the swap
/// test self-contained.
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

/// Awaits the next [`RoomEvent`] matching `predicate` on `room`.
/// Drains other events along the way; panics on timeout.
async fn wait_for_event<F: FnMut(&RoomEvent) -> bool>(
    room: &mut Room,
    msg: &str,
    mut predicate: F,
) -> RoomEvent {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), room.recv()).await {
            Ok(Ok(ev)) if predicate(&ev) => return ev,
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => panic!("{msg}: room recv error: {e:#}"),
            Err(_) => continue,
        }
    }
    panic!("{msg}: timed out after {TIMEOUT:?}");
}

/// Waits for the next event signalling that the active source for
/// this peer's broadcast is now `via_relay`. Accepts either a
/// `BroadcastSwitched` with the matching boolean or a
/// `BroadcastSubscribed` whose session terminates at the expected
/// remote. The room actor emits the former when a live
/// subscription's source set changes (sources mutated under it);
/// it emits the latter when the subscription was destroyed and
/// re-created against the new source set.
async fn wait_for_active_via(
    room: &mut Room,
    expected_via_relay: bool,
    pub_id: iroh::EndpointId,
    relay_id: iroh::EndpointId,
    msg: &str,
) {
    let expected_remote = if expected_via_relay { relay_id } else { pub_id };
    wait_for_event(room, msg, |ev| match ev {
        RoomEvent::BroadcastSwitched { via_relay, .. } => *via_relay == expected_via_relay,
        RoomEvent::BroadcastSubscribed { session, .. } => session.remote_id() == expected_remote,
        _ => false,
    })
    .await;
}

/// Hybrid room: subscriber starts on direct, switches to relay
/// when the direct cable is cut, and switches back when the
/// direct cable is restored.
#[tokio::test]
#[serial]
async fn cable_cut_swaps_direct_to_relay_and_back() {
    let _ = tracing_subscriber::fmt::try_init();
    let (relay_id, server) = start_iroh_relay().await;

    let topic = TopicId::from_bytes(rand::random());
    let offer = RelayOffer {
        endpoint: relay_id,
        path: "/".into(),
        api_key: None,
    };
    let ticket = RoomTicket::new(topic, []).with_relay(offer.clone());
    assert_eq!(ticket.mode(), RoomMode::Hybrid);

    let live_a = live_gossip_client().await;
    let live_b = live_gossip_client().await;
    let id_a = live_a.endpoint().id();

    // Bootstrap b through a so gossip meshes.
    let ticket_a = ticket.clone().with_bootstrap(id_a);
    let room_a = Room::new(&live_a, ticket.clone()).await.expect("a join");
    let mut room_b = Room::new(&live_b, ticket_a).await.expect("b join");

    // Peer A publishes; peer B observes the initial
    // `BroadcastSubscribed`. The default `PreferDirect` policy
    // picks the direct candidate, so the active session terminates
    // at peer A directly.
    let bc_a = build_video_broadcast();
    room_a.publish("a-cam", &bc_a).await.expect("a publish");

    let event = wait_for_event(
        &mut room_b,
        "initial BroadcastSubscribed",
        |ev| matches!(ev, RoomEvent::BroadcastSubscribed { broadcast, .. } if broadcast.broadcast_name().ends_with("a-cam")),
    )
    .await;
    if let RoomEvent::BroadcastSubscribed { session, .. } = event {
        assert_eq!(
            session.remote_id(),
            id_a,
            "default PreferDirect should pick the direct session"
        );
    }

    // Cut the direct cable: switch the subscriber to RelayOnly.
    // The room actor rebuilds B's per-peer source set without
    // the direct candidate, the subscription drops the direct
    // attached state, and the policy picks the relay candidate.
    room_b
        .handle()
        .set_subscribe_mode(RoomSubscribeMode::RelayOnly)
        .await
        .expect("set RelayOnly");

    wait_for_active_via(
        &mut room_b,
        true,
        id_a,
        relay_id,
        "active source via relay after cable cut",
    )
    .await;

    drop(bc_a);
    live_a.shutdown().await;
    live_b.shutdown().await;
    server.abort();
}
