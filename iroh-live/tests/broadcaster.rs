//! Integration tests for [`Broadcaster`] multi-target fan-out.
//!
//! A [`Broadcaster`] attaches one [`BroadcastProducer`] to every
//! session named by its [`SourceSetHandle`]. Each "target" source
//! is a peer that the broadcaster dials and pushes the broadcast
//! announce to. The target accepts the inbound session and can
//! subscribe to the announced broadcast on it.
//!
//! Mutating the source set adds or removes target sessions; the
//! announce ends on a session only when the source is removed (or
//! the broadcaster is dropped).

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup};
use iroh_live::{Live, SourceSet, TransportSource};
use moq_media::{
    codec::VideoCodec,
    format::VideoPreset,
    publish::{LocalBroadcast, VideoInput},
    test_util::TestVideoSource,
};
use n0_tracing_test::traced_test;

const TIMEOUT: Duration = Duration::from_secs(15);
const TEST_VIDEO_CODEC: VideoCodec = VideoCodec::H264;

async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("bind endpoint");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

fn live_with_router(ep: Endpoint) -> Live {
    Live::builder(ep).with_router().spawn()
}

fn broadcast_with_video() -> LocalBroadcast {
    let broadcast = LocalBroadcast::new();
    let source = TestVideoSource::new(320, 240).with_fps(15.0);
    broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P180],
        ))
        .expect("set video");
    broadcast
}

/// Subscribes to a target's incoming session stream and returns
/// the receiver. Must be called BEFORE the broadcaster initiates
/// connections so no inbound session is missed by the underlying
/// broadcast channel.
fn watch_incoming(target: &Live) -> iroh_moq::IncomingSessionStream {
    target.transport().incoming_sessions()
}

/// Drains the next inbound session from `incoming` originated by
/// `from`, accepts it, and subscribes to `name`.
async fn accept_and_subscribe(
    incoming: &mut iroh_moq::IncomingSessionStream,
    from: iroh::EndpointId,
    name: &str,
) -> moq_lite::BroadcastConsumer {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), incoming.next()).await {
            Ok(Some(session)) if session.remote_id() == from => {
                let mut accepted = session.accept();
                let consumer = tokio::time::timeout(TIMEOUT, accepted.subscribe(name))
                    .await
                    .expect("subscribe timeout")
                    .expect("subscribe failed");
                return consumer;
            }
            Ok(Some(other)) => {
                other.reject();
            }
            Ok(None) => panic!("incoming sessions stream ended"),
            Err(_) => continue,
        }
    }
    panic!("timed out waiting for inbound session from {from:?}");
}

/// The broadcaster opens a session to one target and publishes the
/// broadcast there. The target accepts the session and subscribes
/// to the announced broadcast.
#[tokio::test]
#[traced_test]
async fn broadcaster_pushes_to_single_target() {
    let target = live_with_router(endpoint().await);
    let broadcaster_live = live_with_router(endpoint().await);

    // Subscribe to incoming sessions BEFORE starting the broadcaster
    // so no inbound session is missed.
    let mut incoming = watch_incoming(&target);

    let broadcast = broadcast_with_video();
    let producer = broadcast.producer();
    let set: SourceSet = TransportSource::direct(target.endpoint().addr()).into();
    let _bc = broadcaster_live.broadcaster("broadcast", producer, set);

    let _consumer =
        accept_and_subscribe(&mut incoming, broadcaster_live.endpoint().id(), "broadcast").await;

    drop(broadcast);
    target.shutdown().await;
    broadcaster_live.shutdown().await;
}

/// The broadcaster fans the same broadcast out to two targets. Both
/// see it on their inbound session.
#[tokio::test]
#[traced_test]
async fn broadcaster_pushes_to_two_targets() {
    let target_a = live_with_router(endpoint().await);
    let target_b = live_with_router(endpoint().await);
    let broadcaster_live = live_with_router(endpoint().await);

    let mut incoming_a = watch_incoming(&target_a);
    let mut incoming_b = watch_incoming(&target_b);

    let broadcast = broadcast_with_video();
    let producer = broadcast.producer();
    let mut set = SourceSet::new();
    set.push(TransportSource::direct(target_a.endpoint().addr()));
    set.push(TransportSource::direct(target_b.endpoint().addr()));
    let _bc = broadcaster_live.broadcaster("broadcast", producer, set);

    let _a = accept_and_subscribe(
        &mut incoming_a,
        broadcaster_live.endpoint().id(),
        "broadcast",
    )
    .await;
    let _b = accept_and_subscribe(
        &mut incoming_b,
        broadcaster_live.endpoint().id(),
        "broadcast",
    )
    .await;

    drop(broadcast);
    target_a.shutdown().await;
    target_b.shutdown().await;
    broadcaster_live.shutdown().await;
}

/// Adding a source to the broadcaster's set at runtime opens a
/// session to that target.
#[tokio::test]
#[traced_test]
async fn broadcaster_adds_source_at_runtime() {
    let target_a = live_with_router(endpoint().await);
    let broadcaster_live = live_with_router(endpoint().await);
    let mut incoming_a = watch_incoming(&target_a);

    let broadcast = broadcast_with_video();
    let producer = broadcast.producer();
    let set: SourceSet = TransportSource::direct(target_a.endpoint().addr()).into();
    let bc = broadcaster_live.broadcaster("broadcast", producer, set);

    let _a = accept_and_subscribe(
        &mut incoming_a,
        broadcaster_live.endpoint().id(),
        "broadcast",
    )
    .await;

    // Spin up a second target only after the first one is wired
    // through.
    let target_b = live_with_router(endpoint().await);
    let mut incoming_b = watch_incoming(&target_b);
    bc.add_source(TransportSource::direct(target_b.endpoint().addr()));
    let _b = accept_and_subscribe(
        &mut incoming_b,
        broadcaster_live.endpoint().id(),
        "broadcast",
    )
    .await;

    drop(broadcast);
    target_a.shutdown().await;
    target_b.shutdown().await;
    broadcaster_live.shutdown().await;
}
