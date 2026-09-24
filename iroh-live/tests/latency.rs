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
//! The publisher moves every source onto its own clock, so a frame's timestamp
//! on the wire is the source's plus one unknown offset. The source here spaces
//! its timestamps with a small irregular stagger, which makes the offset the
//! one shift that lines every received frame up with a handed one.
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
use iroh_live_media::{
    Latency, PlayerConfig, VideoEncoding, VideoFormat, VideoRendition, VideoSource,
    video::{Frame, Rate, Size, Surface, decode},
};
use n0_tracing_test::traced_test;

/// Generous, because the workspace test suite runs in parallel and openh264
/// encodes in software.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Frames measured after the first, which carries the join and is reported on
/// its own.
const SAMPLES: usize = 60;

/// The picture size of the stamped source.
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

/// When each frame, by its source timestamp in microseconds, was handed to the
/// publisher.
type Handed = Arc<Mutex<HashMap<u64, Instant>>>;

/// The stagger added to frame `index`'s timestamp, under a tenth of a frame
/// and irregular enough that no two stretches of frames share a pattern.
fn stagger(index: u64) -> u64 {
    (index * 7_919 % 97) * 100
}

/// A moving gradient at 30 fps, with every frame's hand-over instant recorded
/// under its source timestamp.
fn stamped_source(handed: Handed) -> VideoSource {
    let format = VideoFormat::new(SIZE, Rate::new(30, 1).expect("a valid rate"));
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

/// Fills `rgba` with a diagonal gradient that shifts with `index`, so no two
/// frames encode to nothing.
fn paint(rgba: &mut [u8], index: u64) {
    let phase = (index % 256) as u8;
    for (offset, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (offset as u32 % SIZE.width) as u8;
        let y = (offset as u32 / SIZE.width) as u8;
        *pixel = [x.wrapping_add(phase), y.wrapping_add(phase), phase, 0xff];
    }
}

/// Finds the offset the publisher moved the timestamps by: the one shift that
/// lands the most received timestamps on handed ones.
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

/// Publishes the stamped source and plays it at `latency`, returning the join
/// latency and the per-frame latencies after it.
async fn measure(latency: Latency) -> (Duration, Vec<Duration>) {
    let handed: Handed = Arc::default();

    let publisher = Live::builder(endpoint().await).with_router().spawn();
    let broadcast = publisher.publish("latency").expect("failed to publish");
    broadcast
        .set_video(
            stamped_source(handed.clone()),
            VideoEncoding::single(VideoRendition::new("video")),
        )
        .expect("failed to set video");
    let publisher_addr = publisher.endpoint().addr();

    let subscriber = Live::builder(endpoint().await).spawn();
    let subscribed_at = Instant::now();
    let sub = subscriber
        .subscribe(publisher_addr, "latency")
        .await
        .expect("failed to subscribe");
    let config = PlayerConfig::default()
        .with_latency(latency)
        .with_decoder(decode::Kind::Software);
    let player = sub.broadcast().play(config).expect("failed to play");
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
    drop(sub);
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

/// No playout hold: a frame goes to the caller the moment it decodes. This is
/// the pipeline's own latency, encoder to decoder over a loopback connection.
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

/// The default latency: the playout clock holds each frame by its jitter
/// buffer plus whatever audio is buffered. With no audio track that is the
/// jitter figure alone, and the difference from the run above is what the
/// hold costs.
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
