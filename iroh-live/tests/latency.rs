//! Measures the time from a frame handed to the publisher to the decoded frame.
//!
//! Both ends run in one process over a loopback QUIC connection and share one
//! clock. The measurement covers encode, transport, decode and the playout
//! policy, with no display in it.
//!
//! The publisher shifts source timestamps by an unknown offset. The source
//! adds an irregular stagger to its timestamps, so only one offset lines the
//! received frames up with the handed ones.
//!
//! The two tests differ only in playout policy, so the difference between
//! their figures is the playout hold. The assertions are loose sanity bounds.
//! Run with `--nocapture` to see the figures.

#![cfg(feature = "media")]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets};
use iroh_live::{BroadcastTicket, Live, LocalBroadcast};
use iroh_live_media::{
    Latency, PlayerConfig, VideoEncoding, VideoFormat, VideoRendition, VideoSource,
    video::{Frame, Rate, Size, Surface, decode},
};
use n0_tracing_test::traced_test;

/// Generous, because tests run in parallel and openh264 encodes in software.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Frames measured after the first, which is reported apart as the join.
const SAMPLES: usize = 60;

/// The stamped source's picture size.
const SIZE: Size = Size {
    width: 640,
    height: 480,
};

/// The nominal gap between two frames, in microseconds.
const FRAME_MICROS: u64 = 33_333;

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

/// Hand-over instants keyed by source timestamp in microseconds.
type Handed = Arc<Mutex<HashMap<u64, Instant>>>;

/// Returns the stagger added to frame `index`'s timestamp.
///
/// It stays under a third of a frame and has no short repeating pattern.
fn stagger(index: u64) -> u64 {
    (index * 7_919 % 97) * 100
}

/// Returns a moving gradient at 30 fps that records when each frame is handed over.
fn stamped_source(handed: Handed) -> VideoSource {
    let format = VideoFormat {
        size: SIZE,
        rate: Rate::new(30, 1).expect("a valid rate"),
    };
    VideoSource::spawn("stamped", format, move |sender| {
        let started = Instant::now();
        let mut rgba = vec![0u8; (SIZE.width * SIZE.height * 4) as usize];
        for index in 0u64.. {
            let micros = index * FRAME_MICROS + stagger(index);
            let due = started + Duration::from_micros(micros);
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            paint(&mut rgba, index);
            let surface = Surface::rgba(&rgba, SIZE).expect("the buffer fits the size");
            let timestamp = moq_net::Timestamp::from_micros(micros).expect("in range");
            handed
                .lock()
                .expect("poisoned")
                .insert(micros, Instant::now());
            if sender.push(Frame::new(surface, timestamp)).is_err() {
                break;
            }
        }
        Ok(())
    })
    .expect("the source thread starts")
}

/// Fills `rgba` with a diagonal gradient that shifts with `index`.
fn paint(rgba: &mut [u8], index: u64) {
    let phase = (index % 256) as u8;
    for (offset, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (offset as u32 % SIZE.width) as u8;
        let y = (offset as u32 / SIZE.width) as u8;
        *pixel = [x.wrapping_add(phase), y.wrapping_add(phase), phase, 0xff];
    }
}

/// Returns the shift that maps the most received timestamps onto handed ones.
fn offset(received: &[(u64, Instant)], handed: &HashMap<u64, Instant>) -> i128 {
    let (first, _) = received[0];
    handed
        .keys()
        .map(|&source| i128::from(first) - i128::from(source))
        .max_by_key(|&shift| {
            received
                .iter()
                .filter(|(wire, _)| {
                    u64::try_from(i128::from(*wire) - shift)
                        .is_ok_and(|source| handed.contains_key(&source))
                })
                .count()
        })
        .expect("frames were handed")
}

/// Plays the stamped source at `latency` and returns the join and frame latencies.
async fn measure(latency: Latency) -> (Duration, Vec<Duration>) {
    let handed: Handed = Arc::default();

    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = LocalBroadcast::new();
    publisher
        .publish("latency", &broadcast)
        .expect("failed to publish");
    broadcast
        .set_video(
            stamped_source(handed.clone()),
            VideoEncoding::single(VideoRendition::new("video")),
        )
        .expect("failed to set video");
    let ticket = BroadcastTicket::new(publisher.endpoint().id(), "latency");

    let subscriber = Live::builder(endpoint().await).spawn();
    let subscribed_at = Instant::now();
    let subscription = subscriber
        .subscribe(&ticket)
        .await
        .expect("failed to subscribe");
    let remote = subscriber.remote_broadcast(&subscription);
    let config = PlayerConfig {
        latency,
        decoder: decode::Kind::Software,
        ..PlayerConfig::default()
    };
    let player = remote.play(config).expect("failed to play");
    let mut frames = player.video();

    let mut received = Vec::with_capacity(SAMPLES + 1);
    while received.len() <= SAMPLES {
        let frame = tokio::time::timeout(TIMEOUT, frames.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("the video ended mid-measurement");
        received.push((frame.timestamp.as_micros() as u64, Instant::now()));
    }
    let join = received[0].1.duration_since(subscribed_at);

    let handed = handed.lock().expect("poisoned").clone();
    let shift = offset(&received, &handed);
    let latencies = received[1..]
        .iter()
        .map(|(wire, arrived)| {
            let source = u64::try_from(i128::from(*wire) - shift).expect("a source timestamp");
            let handed_at = handed
                .get(&source)
                .expect("every frame the player shows was handed to the publisher");
            arrived.duration_since(*handed_at)
        })
        .collect();

    drop(frames);
    drop(player);
    drop(remote);
    subscriber.shutdown().await;
    broadcast.close();
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

/// Measures latency with no playout hold, frames going out as they decode.
#[tokio::test]
#[traced_test]
async fn pipeline_latency_without_a_playout_hold() {
    let (join, mut latencies) = measure(Latency::IMMEDIATE).await;
    let median = report("unmanaged", join, &mut latencies);
    assert!(
        median < Duration::from_secs(2),
        "a loopback pipeline with no playout hold took {median:?} median, which is not a \
         pipeline any more but a queue"
    );
}

/// Measures latency under the default playout hold.
///
/// With no audio track, the hold is the jitter buffer alone.
#[tokio::test]
#[traced_test]
async fn pipeline_latency_with_the_default_playout_hold() {
    let (join, mut latencies) = measure(Latency::default()).await;
    let median = report("synced (default)", join, &mut latencies);
    assert!(
        median < Duration::from_secs(3),
        "the default latency took {median:?} median on loopback, far past its own \
         jitter buffer"
    );
}
