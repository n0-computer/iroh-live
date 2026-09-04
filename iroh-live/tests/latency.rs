//! Measures how long a picture takes to cross the pipeline, publisher to
//! decoded frame, with both ends in one process.
//!
//! Both ends share a wall clock, so the number is honest to the microsecond
//! and leaves out everything a two-machine measurement cannot separate: the
//! player's compositor, the screenshot that reads it, and two clocks that only
//! agree to a few milliseconds. What is left is capture-to-decode: the test
//! pattern's frame handed to the encoder, openh264, the mux, a real QUIC
//! connection over loopback, the demux, the decoder, and the playout policy.
//!
//! Two runs, one per playout policy, so the hold the clock adds is read off as
//! the difference rather than reasoned about. The assertions are sanity
//! bounds wide enough never to flake; the figures are the point, and they are
//! printed. Run with `--nocapture` to see them.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets};
use iroh_live::Live;
use moq_media::{
    playout::PlaybackPolicy,
    publish::VideoSource,
    test_source,
    video::{Size, decode},
};
use n0_future::{StreamExt as _, boxed::BoxStream};
use n0_tracing_test::traced_test;

/// Generous, because the workspace test suite runs in parallel and openh264
/// encodes in software.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Frames measured after the first, which carries the join and is reported on
/// its own.
const SAMPLES: usize = 60;

async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("failed to bind endpoint");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

/// When each frame, by its media timestamp, was handed to the publisher.
type Handed = Arc<Mutex<HashMap<u128, Instant>>>;

/// The test pattern, with every frame's hand-over instant recorded as it goes.
fn stamped_source(handed: Handed) -> VideoSource {
    let VideoSource::Frames(frames) = test_source::video(Size::new(640, 480), 30) else {
        panic!("the test pattern is a frame stream");
    };
    let stamped: BoxStream<moq_media::video::Frame> = Box::pin(frames.map(move |frame| {
        handed
            .lock()
            .expect("poisoned")
            .insert(frame.timestamp.as_micros(), Instant::now());
        frame
    }));
    VideoSource::Frames(stamped)
}

/// Publishes the stamped pattern and subscribes under `policy`, returning the
/// join latency and the per-frame latencies after it.
async fn measure(policy: PlaybackPolicy) -> (Duration, Vec<Duration>) {
    let handed: Handed = Arc::default();

    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = publisher.publish("latency").expect("failed to publish");
    broadcast
        .video()
        .set(stamped_source(handed.clone()))
        .expect("failed to set video");
    let publisher_addr = publisher.endpoint().addr();

    let subscriber = Live::builder(endpoint().await).spawn();
    let subscribed_at = Instant::now();
    let sub = subscriber
        .subscribe(publisher_addr, "latency")
        .await
        .expect("failed to subscribe");
    sub.broadcast().set_playback_policy(policy);
    let track = tokio::time::timeout(TIMEOUT, sub.broadcast().video())
        .await
        .expect("timed out waiting for the video catalog")
        .expect("failed to open the video track");

    let first = tokio::time::timeout(TIMEOUT, track.recv())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the track closed before its first frame");
    let join = subscribed_at.elapsed();
    let _ = first;

    let mut latencies = Vec::with_capacity(SAMPLES);
    while latencies.len() < SAMPLES {
        let frame = tokio::time::timeout(TIMEOUT, track.recv())
            .await
            .expect("timed out waiting for a frame")
            .expect("the track closed mid-measurement");
        let arrived = Instant::now();
        let handed_at = handed
            .lock()
            .expect("poisoned")
            .get(&frame.timestamp.as_micros())
            .copied()
            .expect("every frame the subscriber sees was handed to the publisher");
        latencies.push(arrived.duration_since(handed_at));
    }

    drop(track);
    drop(sub);
    subscriber.shutdown().await;
    drop(broadcast);
    publisher.shutdown().await;
    (join, latencies)
}

fn report(label: &str, join: Duration, latencies: &mut [Duration]) -> Duration {
    latencies.sort();
    let at = |fraction: f64| latencies[((latencies.len() - 1) as f64 * fraction) as usize];
    let median = at(0.5);
    println!(
        "{label}: join {join:?}; then over {} frames min {:?} median {median:?} p90 {:?} max {:?}",
        latencies.len(),
        latencies[0],
        at(0.9),
        latencies[latencies.len() - 1],
    );
    median
}

/// No playout hold: a frame goes to the caller the moment it decodes. This is
/// the pipeline's own latency, encoder to decoder over a loopback connection.
#[tokio::test]
#[traced_test]
async fn pipeline_latency_without_a_playout_hold() {
    let policy = PlaybackPolicy {
        decoder: decode::Kind::Software,
        ..PlaybackPolicy::unmanaged()
    };
    let (join, mut latencies) = measure(policy).await;
    let median = report("unmanaged", join, &mut latencies);
    assert!(
        median < Duration::from_secs(2),
        "a loopback pipeline with no playout hold took {median:?} median, which is not a \
         pipeline any more but a queue"
    );
}

/// The default policy: the playout clock holds each frame by its jitter
/// buffer plus whatever audio is buffered. With no audio track that is the
/// jitter figure alone, and the difference from the run above is what the
/// hold costs.
#[tokio::test]
#[traced_test]
async fn pipeline_latency_with_the_default_playout_hold() {
    let policy = PlaybackPolicy {
        decoder: decode::Kind::Software,
        ..PlaybackPolicy::default()
    };
    let (join, mut latencies) = measure(policy).await;
    let median = report("synced (default)", join, &mut latencies);
    assert!(
        median < Duration::from_secs(3),
        "the default playout policy took {median:?} median on loopback, far past its own \
         jitter buffer"
    );
}
