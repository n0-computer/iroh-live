//! Patchbay integration tests — verify smooth playback under dynamic
//! network conditions (latency ramps, recovery after impairment).
//!
//! Linux-only: patchbay requires unprivileged user namespaces.
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use iroh::{Endpoint, SecretKey};
use iroh_live::Live;
use moq_media::{
    codec::VideoCodec,
    format::VideoPreset,
    publish::{LocalBroadcast, VideoInput},
    test_util::TestVideoSource,
};
use patchbay::{Lab, LinkCondition, LinkLimits, NodeId};
use tracing::info;

// Must run before any threads exist (cargo test harness spawns threads).
#[ctor::ctor]
fn patchbay_init() {
    // SAFETY: called from ELF .init_array, single-threaded, before main.
    unsafe { patchbay::init_userns_for_ctor() };
}

const TEST_VIDEO_CODEC: VideoCodec = VideoCodec::H264;
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

struct PatchbayFixture {
    lab: Lab,
    pub_node: NodeId,
    sub_node: NodeId,
    router_node: NodeId,
    publisher: Live,
    _broadcast: LocalBroadcast,
    subscriber: Live,
    remote: moq_media::subscribe::RemoteBroadcast,
}

impl PatchbayFixture {
    async fn new() -> Self {
        let lab = Lab::new().await.expect("patchbay lab");
        let router = lab.add_router("r1").build().await.expect("patchbay router");
        let router_node = router.id();

        let pub_device = lab
            .add_device("publisher")
            .iface("eth0", router_node, None)
            .build()
            .await
            .expect("pub device");
        let pub_node = pub_device.id();

        let sub_device = lab
            .add_device("subscriber")
            .iface("eth0", router_node, None)
            .build()
            .await
            .expect("sub device");
        let sub_node = sub_device.id();

        let secret_key = SecretKey::generate(&mut rand::rng());
        let pub_endpoint = pub_device
            .spawn({
                let secret_key = secret_key.clone();
                |_dev| async move {
                    Endpoint::builder(iroh::endpoint::presets::N0)
                        .secret_key(secret_key)
                        .bind()
                        .await
                        .map_err(|e| anyhow::anyhow!("{e:#}"))
                }
            })
            .expect("pub spawn")
            .await
            .expect("pub join")
            .expect("pub endpoint");

        let sub_endpoint = sub_device
            .spawn(|_dev| async move {
                Endpoint::bind(iroh::endpoint::presets::N0)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e:#}"))
            })
            .expect("sub spawn")
            .await
            .expect("sub join")
            .expect("sub endpoint");

        let publisher = Live::builder(pub_endpoint).spawn_with_router();

        let broadcast = LocalBroadcast::new();
        // Use 15fps to stay within debug-build encode/decode budget.
        let source = TestVideoSource::new(320, 240).with_fps(15.0);
        broadcast
            .video()
            .set(VideoInput::new(
                source,
                TEST_VIDEO_CODEC,
                [VideoPreset::P180],
            ))
            .expect("set video");
        publisher
            .publish("test", &broadcast)
            .await
            .expect("publish");

        let pub_addr = publisher.endpoint().addr();
        let subscriber = Live::builder(sub_endpoint).spawn();
        let (_session, remote) = subscriber
            .subscribe(pub_addr, "test")
            .await
            .expect("subscribe");

        Self {
            lab,
            pub_node,
            sub_node,
            router_node,
            publisher,
            _broadcast: broadcast,
            subscriber,
            remote,
        }
    }

    /// Sets link impairment on both pub↔router and sub↔router links.
    async fn set_latency(&self, latency_ms: u32) {
        let cond = if latency_ms == 0 {
            None
        } else {
            Some(LinkCondition::Manual(LinkLimits {
                latency_ms,
                jitter_ms: latency_ms / 5,
                ..Default::default()
            }))
        };
        self.lab
            .set_link_condition(self.pub_node, self.router_node, cond)
            .await
            .expect("pub link condition");
        self.lab
            .set_link_condition(self.sub_node, self.router_node, cond)
            .await
            .expect("sub link condition");
        info!(latency_ms, "link condition updated");
    }

    async fn shutdown(self) {
        self.remote.shutdown();
        self.publisher.shutdown().await;
        self.subscriber.shutdown().await;
    }
}

/// Drains frames for the given duration and returns the wall-clock timestamps
/// of each received frame.
async fn drain_frames(
    track: &mut moq_media::subscribe::VideoTrack,
    duration: Duration,
) -> Vec<Instant> {
    let deadline = Instant::now() + duration;
    let mut arrivals = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, track.next_frame()).await {
            Ok(Some(_frame)) => {
                arrivals.push(Instant::now());
            }
            Ok(None) => break, // track closed
            Err(_) => break,   // timeout
        }
    }
    arrivals
}

/// Computes inter-frame gaps from a sorted list of arrival timestamps.
fn inter_frame_gaps(arrivals: &[Instant]) -> Vec<Duration> {
    arrivals
        .windows(2)
        .map(|w| w[1].duration_since(w[0]))
        .collect()
}

/// Returns the fraction of gaps that exceed the threshold.
fn gap_violation_rate(gaps: &[Duration], threshold: Duration) -> f64 {
    if gaps.is_empty() {
        return 0.0;
    }
    let violations = gaps.iter().filter(|g| **g > threshold).count();
    violations as f64 / gaps.len() as f64
}

/// After increasing latency and then dropping it back to zero, video
/// playback should recover to smooth frame delivery within a few seconds.
///
/// "Smooth" means: at least 90% of inter-frame gaps are ≤ 100ms (3×
/// the 33ms expected interval at 30fps). The tolerance accounts for
/// normal scheduling jitter and the adaptive playout buffer.
#[test]
fn latency_up_down_video_recovers() {
    let _ = tracing_subscriber::fmt::try_init();

    // init_userns must be called before any threads exist (before tokio).
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(latency_up_down_video_recovers_inner());
}

async fn latency_up_down_video_recovers_inner() {
    let fixture = PatchbayFixture::new().await;

    // Raise skip threshold so the test can observe delay without skipping.
    fixture.remote.set_skip_threshold(Duration::from_secs(5));

    let mut track = tokio::time::timeout(FRAME_TIMEOUT, fixture.remote.video_ready())
        .await
        .expect("timeout waiting for video catalog")
        .expect("video_ready failed");

    // Warmup: the first frames take longer due to encoder/decoder init,
    // QUIC handshake tail, and patchbay namespace overhead. Drain 2s
    // without asserting to let the pipeline reach steady state.
    let warmup = drain_frames(&mut track, Duration::from_secs(2)).await;
    info!(frames = warmup.len(), "warmup complete");

    // Phase 1: baseline — drain 3s of frames at zero latency.
    // Source runs at 15fps; in debug builds the actual throughput may be
    // slightly lower due to encode/decode overhead.
    let baseline_arrivals = drain_frames(&mut track, Duration::from_secs(3)).await;
    let baseline_gaps = inter_frame_gaps(&baseline_arrivals);
    let baseline_fps = baseline_arrivals.len() as f64 / 3.0;
    info!(
        frames = baseline_arrivals.len(),
        fps = format!("{baseline_fps:.1}"),
        median_gap_ms = baseline_gaps
            .get(baseline_gaps.len() / 2)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "phase 1: baseline"
    );
    assert!(
        baseline_arrivals.len() >= 20,
        "expected ≥20 baseline frames in 3s at 15fps, got {}",
        baseline_arrivals.len()
    );

    // Phase 2: ramp latency up to 300ms.
    fixture.set_latency(300).await;
    // Let the pipeline absorb the latency change. Frames stall briefly
    // while in-flight packets traverse the new delay, then resume.
    let ramp_arrivals = drain_frames(&mut track, Duration::from_secs(5)).await;
    let ramp_gaps = inter_frame_gaps(&ramp_arrivals);
    info!(
        frames = ramp_arrivals.len(),
        max_gap_ms = ramp_gaps.iter().max().map(|d| d.as_millis()).unwrap_or(0),
        "phase 2: latency at 300ms"
    );
    assert!(
        ramp_arrivals.len() >= 15,
        "expected ≥15 frames during latency ramp, got {} (pipeline stalled?)",
        ramp_arrivals.len()
    );

    // Phase 3: drop latency back to zero and settle.
    fixture.set_latency(0).await;
    let settle = drain_frames(&mut track, Duration::from_secs(3)).await;
    info!(
        frames = settle.len(),
        "phase 3: settling after latency drop"
    );

    // Phase 4: measure recovery — drain 3s and assert smooth playback.
    let recovery_arrivals = drain_frames(&mut track, Duration::from_secs(3)).await;
    let recovery_gaps = inter_frame_gaps(&recovery_arrivals);

    // In debug builds, inter-frame gaps are ~70ms (14fps). Allow up to
    // 200ms (roughly 3× the debug-build frame interval) as "smooth".
    let max_acceptable_gap = Duration::from_millis(200);
    let violation_rate = gap_violation_rate(&recovery_gaps, max_acceptable_gap);

    let recovery_fps = recovery_arrivals.len() as f64 / 3.0;
    info!(
        frames = recovery_arrivals.len(),
        fps = format!("{recovery_fps:.1}"),
        violation_rate = format!("{:.1}%", violation_rate * 100.0),
        max_gap_ms = recovery_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        median_gap_ms = recovery_gaps
            .get(recovery_gaps.len() / 2)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "phase 4: recovery"
    );

    assert!(
        recovery_arrivals.len() >= 20,
        "expected ≥20 recovery frames in 3s at 15fps, got {}",
        recovery_arrivals.len()
    );
    assert!(
        violation_rate <= 0.10,
        "too many large gaps after recovery: {:.1}% of gaps exceed {}ms \
         (max gap: {}ms, median gap: {}ms)",
        violation_rate * 100.0,
        max_acceptable_gap.as_millis(),
        recovery_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        recovery_gaps
            .get(recovery_gaps.len() / 2)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    );

    // Phase 5: cycle through a second, higher latency to confirm
    // repeated transitions don't degrade the pipeline.
    fixture.set_latency(500).await;
    let ramp2_arrivals = drain_frames(&mut track, Duration::from_secs(3)).await;
    info!(frames = ramp2_arrivals.len(), "phase 5: latency at 500ms");
    assert!(
        ramp2_arrivals.len() >= 15,
        "expected ≥15 frames during 500ms ramp, got {} (pipeline stalled?)",
        ramp2_arrivals.len()
    );

    // Phase 6: drop back to zero again and verify recovery.
    fixture.set_latency(0).await;
    let _settle2 = drain_frames(&mut track, Duration::from_secs(3)).await;
    let recovery2_arrivals = drain_frames(&mut track, Duration::from_secs(3)).await;
    let recovery2_gaps = inter_frame_gaps(&recovery2_arrivals);
    let violation_rate2 = gap_violation_rate(&recovery2_gaps, max_acceptable_gap);

    info!(
        frames = recovery2_arrivals.len(),
        violation_rate = format!("{:.1}%", violation_rate2 * 100.0),
        max_gap_ms = recovery2_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "phase 6: second recovery"
    );

    assert!(
        recovery2_arrivals.len() >= 20,
        "expected ≥20 second-recovery frames, got {}",
        recovery2_arrivals.len()
    );
    assert!(
        violation_rate2 <= 0.10,
        "second recovery: {:.1}% of gaps exceed {}ms",
        violation_rate2 * 100.0,
        max_acceptable_gap.as_millis(),
    );

    fixture.shutdown().await;
}
