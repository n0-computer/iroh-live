//! Integration tests for [`Subscription`] multi-origin behaviour.
//!
//! Exercises the unified subscription against two real publishers
//! that both serve the same broadcast name. Mutations to the
//! [`SourceSetHandle`] at runtime are verified to attach and detach
//! sessions; the active source surfaces through
//! [`Subscription::active`] and follows the policy as candidates
//! come and go.
//!
//! The publishers in these tests are direct iroh peers. Substituting
//! a relay for either source does not change the semantics at this
//! layer; the relay-specific path is exercised by the
//! `iroh-live-relay` bridge tests.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use iroh::{Endpoint, address_lookup::MemoryLookup};
use iroh_live::{Live, SourceSet, SubscriptionEvent, SubscriptionEvents, TransportSource};
use moq_media::{
    AudioBackend,
    codec::{AudioCodec, VideoCodec},
    format::{AudioFormat, AudioPreset, VideoPreset},
    publish::{LocalBroadcast, VideoInput},
    test_util::{TestAudioSource, TestVideoSource},
    traits::AudioStreamFactory,
};
use n0_tracing_test::traced_test;

const TIMEOUT: Duration = Duration::from_secs(15);
const TEST_VIDEO_CODEC: VideoCodec = VideoCodec::H264;
const BROADCAST_NAME: &str = "multi-source-test";

/// Returns an iroh endpoint sharing the in-memory address lookup so
/// peers in the same test process can dial each other without DNS.
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

/// Spawns a [`Live`] instance with router enabled so the endpoint
/// accepts incoming MoQ sessions.
fn live_with_router(ep: Endpoint) -> Live {
    Live::builder(ep).with_router().spawn()
}

/// Builds a [`LocalBroadcast`] carrying a deterministic test video
/// source. Two of these can publish under the same name on
/// different peers and a subscriber can swap between them.
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
        .expect("set video input");
    broadcast
}

/// Builds a [`LocalBroadcast`] carrying both video and a
/// deterministic audio source. Two of these are enough to
/// exercise the seamless audio swap.
fn broadcast_with_video_and_audio() -> LocalBroadcast {
    let broadcast = broadcast_with_video();
    broadcast
        .audio()
        .set(
            TestAudioSource::new(AudioFormat::mono_48k()),
            AudioCodec::Opus,
            [AudioPreset::Hq],
        )
        .expect("set audio input");
    broadcast
}

/// Awaits the next [`SubscriptionEvent`] matching `predicate` or
/// panics with `msg` after [`TIMEOUT`]. Uses the receiver in
/// `events` so the caller is responsible for subscribing to the
/// stream before the events of interest fire.
async fn wait_for_event(
    events: &mut SubscriptionEvents,
    msg: &str,
    mut predicate: impl FnMut(&SubscriptionEvent) -> bool,
) -> SubscriptionEvent {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
            Ok(Ok(ev)) if predicate(&ev) => return ev,
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("{msg}: subscription event stream closed")
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Err(_) => continue,
        }
    }
    panic!("{msg}: timed out after {TIMEOUT:?}");
}

/// Single source: subscription picks it, active surfaces, video
/// frames flow.
#[tokio::test]
#[traced_test]
async fn single_source_subscription_round_trip() {
    let publisher = live_with_router(endpoint().await);
    let broadcast = broadcast_with_video();
    publisher
        .publish(BROADCAST_NAME, &broadcast)
        .await
        .expect("publish");

    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(publisher.endpoint().addr(), BROADCAST_NAME);

    let active = tokio::time::timeout(TIMEOUT, sub.ready())
        .await
        .expect("ready timeout")
        .expect("ready failed");

    assert!(active.id.is_direct());
    assert_eq!(active.id.endpoint(), publisher.endpoint().id());

    let mut video = tokio::time::timeout(TIMEOUT, active.broadcast().video_ready())
        .await
        .expect("video catalog timeout")
        .expect("video track");
    let frame = tokio::time::timeout(TIMEOUT, video.next_frame())
        .await
        .expect("frame timeout")
        .expect("video closed");
    assert!(frame.width() > 0 && frame.height() > 0);

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Two publishers, two sources: the first source in the set is
/// picked. Removing it from the set swaps the active source to the
/// second.
#[tokio::test]
#[traced_test]
async fn subscription_falls_over_when_preferred_source_is_removed() {
    let pub_a = live_with_router(endpoint().await);
    let broadcast_a = broadcast_with_video();
    pub_a
        .publish(BROADCAST_NAME, &broadcast_a)
        .await
        .expect("publish a");

    let pub_b = live_with_router(endpoint().await);
    let broadcast_b = broadcast_with_video();
    pub_b
        .publish(BROADCAST_NAME, &broadcast_b)
        .await
        .expect("publish b");

    let mut set = SourceSet::new();
    set.push(TransportSource::direct(pub_a.endpoint().addr()));
    set.push(TransportSource::direct(pub_b.endpoint().addr()));
    let id_a = set.get(0).expect("a").id();
    let id_b = set.get(1).expect("b").id();

    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(set, BROADCAST_NAME);
    // Subscribe to events before triggering the transition so the
    // broadcast channel captures every emit.
    let mut events = sub.events();

    let initial = tokio::time::timeout(TIMEOUT, sub.ready())
        .await
        .expect("ready timeout")
        .expect("ready failed");
    assert_eq!(initial.id, id_a, "expected first source preferred");

    // Remove the preferred source; subscription must fall over to b.
    let removed = sub.sources().remove(&id_a);
    assert!(removed, "source a should be present");

    wait_for_event(
        &mut events,
        "active should switch to second source",
        |ev| matches!(ev, SubscriptionEvent::ActiveChanged { current: Some(id), .. } if id == &id_b),
    )
    .await;

    pub_a.shutdown().await;
    pub_b.shutdown().await;
    subscriber.shutdown().await;
}

/// Adding a source to a running subscription attaches it
/// asynchronously and emits an `Attached` event.
#[tokio::test]
#[traced_test]
async fn subscription_runtime_push_attaches_new_source() {
    let pub_a = live_with_router(endpoint().await);
    let broadcast_a = broadcast_with_video();
    pub_a
        .publish(BROADCAST_NAME, &broadcast_a)
        .await
        .expect("publish a");

    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(pub_a.endpoint().addr(), BROADCAST_NAME);
    let _ = tokio::time::timeout(TIMEOUT, sub.ready())
        .await
        .expect("ready timeout")
        .expect("ready failed");
    let mut events = sub.events();

    // Spin up a second publisher and add it to the live source set.
    let pub_b = live_with_router(endpoint().await);
    let broadcast_b = broadcast_with_video();
    pub_b
        .publish(BROADCAST_NAME, &broadcast_b)
        .await
        .expect("publish b");
    let new_source = TransportSource::direct(pub_b.endpoint().addr());
    let new_id = new_source.id();
    sub.sources().push(new_source);

    wait_for_event(
        &mut events,
        "Attached event for newly added source",
        |ev| matches!(ev, SubscriptionEvent::Attached { id } if id == &new_id),
    )
    .await;

    // Active stays on the preferred (first) source.
    assert_eq!(
        sub.active_id().expect("active id"),
        // pub_a is index 0
        iroh_live::SourceId::direct(pub_a.endpoint().id())
    );

    pub_a.shutdown().await;
    pub_b.shutdown().await;
    subscriber.shutdown().await;
}

/// Attaching an unreachable source emits an `AttachFailed` event
/// and the subscription continues to operate on its other sources.
#[tokio::test]
#[traced_test]
async fn unreachable_source_emits_attach_failed_without_blocking_others() {
    let pub_a = live_with_router(endpoint().await);
    let broadcast_a = broadcast_with_video();
    pub_a
        .publish(BROADCAST_NAME, &broadcast_a)
        .await
        .expect("publish a");

    // Unreachable peer: a fresh keypair never bound and never
    // registered with the shared address lookup.
    let unreachable = iroh::EndpointAddr::new(iroh::SecretKey::generate().public());

    let mut set = SourceSet::new();
    set.push(TransportSource::direct(unreachable.clone()));
    set.push(TransportSource::direct(pub_a.endpoint().addr()));
    let unreachable_id = iroh_live::SourceId::direct(unreachable.id);
    let working_id = iroh_live::SourceId::direct(pub_a.endpoint().id());

    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(set, BROADCAST_NAME);
    let mut events = sub.events();

    // The working source attaches; the unreachable one fails.
    let mut saw_failed = false;
    let mut saw_attached = false;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline && !(saw_failed && saw_attached) {
        match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
            Ok(Ok(SubscriptionEvent::AttachFailed { id, .. })) if id == unreachable_id => {
                saw_failed = true;
            }
            Ok(Ok(SubscriptionEvent::Attached { id })) if id == working_id => {
                saw_attached = true;
            }
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Err(_) => continue,
        }
    }
    assert!(saw_failed, "expected AttachFailed for unreachable source");
    assert!(saw_attached, "expected Attached for working source");

    // The subscription falls back to the working source.
    let active = sub
        .active()
        .await
        .expect("subscription should have an active source");
    assert_eq!(active.id, working_id);

    pub_a.shutdown().await;
    subscriber.shutdown().await;
}

/// When the preferred source is unhealthy and the subscription
/// has fallen over to a fallback, restoring the preferred source
/// switches the active back. `PreferOrdered` honours priority
/// recovery without hysteresis.
#[tokio::test]
#[traced_test]
async fn subscription_recovers_preferred_when_it_returns() {
    let pub_a = live_with_router(endpoint().await);
    let broadcast_a = broadcast_with_video();
    pub_a
        .publish(BROADCAST_NAME, &broadcast_a)
        .await
        .expect("publish a");

    let pub_b = live_with_router(endpoint().await);
    let broadcast_b = broadcast_with_video();
    pub_b
        .publish(BROADCAST_NAME, &broadcast_b)
        .await
        .expect("publish b");

    let mut set = SourceSet::new();
    set.push(TransportSource::direct(pub_a.endpoint().addr()));
    set.push(TransportSource::direct(pub_b.endpoint().addr()));
    let id_a = set.get(0).expect("a").id();
    let id_b = set.get(1).expect("b").id();

    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(set, BROADCAST_NAME);
    let mut events = sub.events();

    let initial = sub.ready().await.expect("ready");
    assert_eq!(initial.id, id_a, "should start on preferred");

    // Remove preferred; expect fall-over to b.
    sub.sources().remove(&id_a);
    wait_for_event(
        &mut events,
        "fall over to b",
        |ev| matches!(ev, SubscriptionEvent::ActiveChanged { current: Some(id), .. } if id == &id_b),
    )
    .await;

    // Restore preferred at the front of the set; expect swap
    // back. `PreferOrdered` honours the set's order, so callers
    // that want a stable preferred reset by replacing the whole
    // set rather than appending.
    let mut restored = SourceSet::new();
    restored.push(TransportSource::direct(pub_a.endpoint().addr()));
    restored.push(TransportSource::direct(pub_b.endpoint().addr()));
    sub.sources().set(restored);
    wait_for_event(
        &mut events,
        "recover to a",
        |ev| matches!(ev, SubscriptionEvent::ActiveChanged { current: Some(id), .. } if id == &id_a),
    )
    .await;

    pub_a.shutdown().await;
    pub_b.shutdown().await;
    subscriber.shutdown().await;
}

/// `wait_active` returns `None` when the subscription is shut
/// down before any source becomes healthy.
#[tokio::test]
#[traced_test]
async fn wait_active_returns_none_when_shut_down_with_empty_set() {
    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(SourceSet::new(), BROADCAST_NAME);

    // No sources, no publisher; cancel the subscription so the
    // actor exits and `wait_active` resolves with `None`.
    let waiter = sub.clone();
    let waiter_task = tokio::spawn(async move { waiter.wait_active().await });

    // Give the actor a moment to enter its select loop, then
    // shut it down.
    tokio::time::sleep(Duration::from_millis(50)).await;
    sub.shutdown();

    let result = tokio::time::timeout(TIMEOUT, waiter_task)
        .await
        .expect("waiter timeout")
        .expect("waiter task panicked");
    assert!(
        result.is_none(),
        "wait_active should return None on shutdown with no healthy source"
    );

    subscriber.shutdown().await;
}

/// Subscription handle is `Clone` and observers see consistent
/// state from any clone.
#[tokio::test]
#[traced_test]
async fn subscription_clones_share_state() {
    let publisher = live_with_router(endpoint().await);
    let broadcast = broadcast_with_video();
    publisher
        .publish(BROADCAST_NAME, &broadcast)
        .await
        .expect("publish");

    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(publisher.endpoint().addr(), BROADCAST_NAME);
    let sub_clone = sub.clone();

    // Wait for active on the first handle.
    let active = tokio::time::timeout(TIMEOUT, sub.ready())
        .await
        .expect("ready timeout")
        .expect("ready failed");

    // The clone reports the same active.
    let active_id = sub_clone.active_id().expect("active");
    assert_eq!(active_id, active.id);

    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Two audio publishers, one subscriber. The seamless audio
/// track is built once, plays, then keeps playing through the
/// active-source swap when the preferred source is removed.
/// Volume set on the consumer survives the swap; the rendition
/// watcher reflects the new track; `is_stopped` stays false.
#[tokio::test]
#[traced_test]
async fn seamless_audio_track_survives_source_swap() {
    let pub_a = live_with_router(endpoint().await);
    let broadcast_a = broadcast_with_video_and_audio();
    pub_a
        .publish(BROADCAST_NAME, &broadcast_a)
        .await
        .expect("publish a");

    let pub_b = live_with_router(endpoint().await);
    let broadcast_b = broadcast_with_video_and_audio();
    pub_b
        .publish(BROADCAST_NAME, &broadcast_b)
        .await
        .expect("publish b");

    let mut set = SourceSet::new();
    set.push(TransportSource::direct(pub_a.endpoint().addr()));
    set.push(TransportSource::direct(pub_b.endpoint().addr()));
    let id_a = set.get(0).expect("a").id();

    let subscriber = live_with_router(endpoint().await);
    let sub = subscriber.subscribe(set, BROADCAST_NAME);

    let _ = tokio::time::timeout(TIMEOUT, sub.ready())
        .await
        .expect("ready timeout")
        .expect("ready failed");

    // Build the seamless media surface. The audio track wraps an
    // inner pipeline that the swap task will rebuild on the next
    // active-source change.
    let backend: Arc<dyn AudioStreamFactory> = Arc::new(AudioBackend::default());
    let tracks = sub.media(backend, Default::default()).await.expect("media");
    let audio = tracks.audio.expect("audio rendition picked on init");

    assert!(
        !audio.is_stopped().await,
        "initial track should not be stopped"
    );
    let initial_rendition = audio.rendition();
    let _rendition_watcher = audio.rendition_watcher();

    audio.set_volume(0.42).await;
    assert!((audio.volume() - 0.42).abs() < f32::EPSILON);

    // Force a swap by removing the active source.
    assert!(sub.sources().remove(&id_a), "remove a");

    // The rendition watcher should fire as soon as the swap
    // task installs the new track. Two publishers on the same
    // codec resolve to the same rendition name, so the watcher
    // updates by sending the (possibly equal) value through the
    // channel; we wait for the swap by polling the volume on
    // the new track instead, and assert the swap completed.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let swapped = loop {
        if tokio::time::Instant::now() > deadline {
            panic!("seamless audio swap did not complete");
        }
        // The new track installs with the persisted volume. If
        // we observe the volume reading reflecting our setting
        // and the source set has converged, the swap landed.
        if (audio.volume() - 0.42).abs() < f32::EPSILON
            && sub.active_id().map(|id| id != id_a).unwrap_or(false)
        {
            break true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(swapped);

    // The track survived the swap: still playing, rendition
    // reflects whatever the new source uses (same codec; this
    // is either the same name or a compatible alternative), and
    // the volume the consumer set persists.
    assert!(
        !audio.is_stopped().await,
        "track must keep playing through swap"
    );
    let new_rendition = audio.rendition();
    // The picker prefers the same name when present; the test
    // publishers use the same codec and rendition layout, so
    // the names match.
    assert_eq!(initial_rendition, new_rendition);
    assert!((audio.volume() - 0.42).abs() < f32::EPSILON);

    pub_a.shutdown().await;
    pub_b.shutdown().await;
    subscriber.shutdown().await;
}
