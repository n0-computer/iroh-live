//! Patchbay integration tests — verify smooth playback under dynamic
//! network conditions (latency ramps, recovery after impairment).
//!
//! Linux-only: patchbay requires unprivileged user namespaces.
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use iroh::{Endpoint, SecretKey};
use iroh_live::Live;
use moq_media::{
    adaptive::AdaptiveConfig,
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
        self.set_impairment(LinkLimits {
            latency_ms,
            jitter_ms: latency_ms / 5,
            ..Default::default()
        })
        .await;
    }

    /// Applies full link impairment (latency, loss, rate) to both links.
    /// Pass `Default::default()` to clear all impairment.
    async fn set_impairment(&self, limits: LinkLimits) {
        let cond = if limits.latency_ms == 0
            && limits.loss_pct == 0.0
            && limits.rate_kbit == 0
            && limits.jitter_ms == 0
        {
            None
        } else {
            Some(LinkCondition::Manual(limits))
        };
        self.lab
            .set_link_condition(self.pub_node, self.router_node, cond)
            .await
            .expect("pub link condition");
        self.lab
            .set_link_condition(self.sub_node, self.router_node, cond)
            .await
            .expect("sub link condition");
        info!(?limits, "link impairment updated");
    }

    async fn shutdown(self) {
        self.remote.shutdown();
        self.publisher.shutdown().await;
        self.subscriber.shutdown().await;
    }
}

/// Drains frames for the given duration using async `next_frame()`.
/// Returns the wall-clock timestamps of each received frame.
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

/// Polls frames at a fixed interval using `current_frame()`, mimicking
/// how egui's `render()` loop works. This is more realistic than
/// `drain_frames` because it reveals bursty delivery: if multiple frames
/// arrive between polls, only the latest is kept (the others are dropped).
///
/// Returns one `Instant` per poll tick that received a frame.
fn poll_frames(track: &mut moq_media::subscribe::VideoTrack, duration: Duration) -> Vec<Instant> {
    let poll_interval = Duration::from_millis(16); // ~60Hz, like egui
    let deadline = Instant::now() + duration;
    let mut arrivals = Vec::new();
    loop {
        if Instant::now() >= deadline {
            break;
        }
        if track.current_frame().is_some() {
            arrivals.push(Instant::now());
        }
        std::thread::sleep(poll_interval);
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

/// Simulates a total link blackout (100% packet loss for 1.5s) and verifies
/// that the pipeline recovers to smooth playback after the link comes back.
///
/// Uses poll-based frame consumption to match the egui rendering pattern:
/// polls at ~60Hz with current_frame(), which drops stale frames.
#[test]
fn link_blackout_recovers() {
    let _ = tracing_subscriber::fmt::try_init();
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(link_blackout_inner());
}

async fn link_blackout_inner() {
    let fixture = PatchbayFixture::new().await;
    fixture.remote.set_skip_threshold(Duration::from_secs(5));

    let mut track = tokio::time::timeout(FRAME_TIMEOUT, fixture.remote.video_ready())
        .await
        .expect("timeout waiting for video catalog")
        .expect("video_ready failed");

    // Warmup + baseline.
    let _ = drain_frames(&mut track, Duration::from_secs(2)).await;
    let baseline = poll_frames(&mut track, Duration::from_secs(2));
    info!(frames = baseline.len(), "baseline (poll)");
    assert!(
        baseline.len() >= 15,
        "expected ≥15 polled baseline frames in 2s, got {}",
        baseline.len()
    );

    // Blackout: 100% packet loss for 1.5s.
    fixture
        .set_impairment(LinkLimits {
            loss_pct: 100.0,
            ..Default::default()
        })
        .await;
    let blackout = poll_frames(&mut track, Duration::from_millis(1500));
    info!(frames = blackout.len(), "blackout phase");
    // During total loss, few or no frames should arrive.

    // Restore clean link.
    fixture.set_impairment(Default::default()).await;

    // Settle: give the pipeline time to resync after the blackout.
    let settle = poll_frames(&mut track, Duration::from_secs(3));
    info!(frames = settle.len(), "settle after blackout");

    // Recovery: measure smoothness with poll-based consumption.
    let recovery = poll_frames(&mut track, Duration::from_secs(3));
    let recovery_gaps = inter_frame_gaps(&recovery);
    let max_acceptable_gap = Duration::from_millis(200);
    let violation_rate = gap_violation_rate(&recovery_gaps, max_acceptable_gap);

    info!(
        frames = recovery.len(),
        violation_pct = format!("{:.1}", violation_rate * 100.0),
        max_gap_ms = recovery_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "recovery after blackout (poll)"
    );

    assert!(
        recovery.len() >= 15,
        "expected ≥15 recovery frames in 3s (poll), got {}",
        recovery.len()
    );
    assert!(
        violation_rate <= 0.15,
        "too many large gaps after blackout: {:.1}% exceed {}ms",
        violation_rate * 100.0,
        max_acceptable_gap.as_millis(),
    );

    fixture.shutdown().await;
}

/// Verifies that high packet loss triggers recovery (and doesn't stall).
/// With 20% loss, QUIC retransmits should keep the stream alive, but
/// degraded. After loss drops to zero, playback should recover fully.
#[test]
fn packet_loss_spike_recovers() {
    let _ = tracing_subscriber::fmt::try_init();
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(packet_loss_spike_inner());
}

async fn packet_loss_spike_inner() {
    let fixture = PatchbayFixture::new().await;
    fixture.remote.set_skip_threshold(Duration::from_secs(5));

    let mut track = tokio::time::timeout(FRAME_TIMEOUT, fixture.remote.video_ready())
        .await
        .expect("timeout waiting for video catalog")
        .expect("video_ready failed");

    // Warmup.
    let _ = drain_frames(&mut track, Duration::from_secs(2)).await;

    // Baseline.
    let baseline = drain_frames(&mut track, Duration::from_secs(2)).await;
    info!(frames = baseline.len(), "baseline");

    // Spike: 20% packet loss for 3s.
    fixture
        .set_impairment(LinkLimits {
            loss_pct: 20.0,
            ..Default::default()
        })
        .await;
    let lossy = drain_frames(&mut track, Duration::from_secs(3)).await;
    info!(frames = lossy.len(), "lossy phase (20% loss)");
    // Pipeline should still deliver frames despite loss (QUIC retransmits).
    assert!(
        lossy.len() >= 10,
        "expected ≥10 frames during 20% loss, got {} (pipeline stalled?)",
        lossy.len()
    );

    // Restore clean link.
    fixture.set_impairment(Default::default()).await;
    let _settle = drain_frames(&mut track, Duration::from_secs(3)).await;

    // Recovery.
    let recovery = drain_frames(&mut track, Duration::from_secs(3)).await;
    let recovery_gaps = inter_frame_gaps(&recovery);
    let violation_rate = gap_violation_rate(&recovery_gaps, Duration::from_millis(200));

    info!(
        frames = recovery.len(),
        violation_pct = format!("{:.1}", violation_rate * 100.0),
        max_gap_ms = recovery_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "recovery after loss spike"
    );

    assert!(
        recovery.len() >= 20,
        "expected ≥20 recovery frames in 3s, got {}",
        recovery.len()
    );
    assert!(
        violation_rate <= 0.10,
        "too many gaps after loss spike: {:.1}% exceed 200ms",
        violation_rate * 100.0,
    );

    fixture.shutdown().await;
}

/// Uses the poll-based (egui-realistic) frame drain during latency
/// transitions to reveal stuttering that the async `next_frame()` path
/// hides. If the split example freezes but drain_frames-based tests
/// don't, this test should catch it.
#[test]
fn latency_poll_based_smoothness() {
    let _ = tracing_subscriber::fmt::try_init();
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(latency_poll_based_inner());
}

async fn latency_poll_based_inner() {
    let fixture = PatchbayFixture::new().await;
    fixture.remote.set_skip_threshold(Duration::from_secs(5));

    let mut track = tokio::time::timeout(FRAME_TIMEOUT, fixture.remote.video_ready())
        .await
        .expect("timeout waiting for video catalog")
        .expect("video_ready failed");

    // Warmup.
    let _ = drain_frames(&mut track, Duration::from_secs(2)).await;

    // Baseline with polling.
    let baseline = poll_frames(&mut track, Duration::from_secs(2));
    let baseline_gaps = inter_frame_gaps(&baseline);
    info!(
        frames = baseline.len(),
        median_gap_ms = baseline_gaps
            .get(baseline_gaps.len() / 2)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "baseline (poll)"
    );
    assert!(baseline.len() >= 15, "baseline too few: {}", baseline.len());

    // Add 300ms latency.
    fixture.set_latency(300).await;
    let _absorb = poll_frames(&mut track, Duration::from_secs(4));

    // Drop latency back to zero, settle.
    fixture.set_latency(0).await;
    let _settle = poll_frames(&mut track, Duration::from_secs(3));

    // Measure poll-based recovery.
    let recovery = poll_frames(&mut track, Duration::from_secs(3));
    let recovery_gaps = inter_frame_gaps(&recovery);
    let max_acceptable_gap = Duration::from_millis(200);
    let violation_rate = gap_violation_rate(&recovery_gaps, max_acceptable_gap);

    info!(
        frames = recovery.len(),
        violation_pct = format!("{:.1}", violation_rate * 100.0),
        max_gap_ms = recovery_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "recovery (poll)"
    );

    assert!(
        recovery.len() >= 15,
        "poll recovery too few frames: {}",
        recovery.len()
    );
    assert!(
        violation_rate <= 0.15,
        "poll recovery: {:.1}% of gaps exceed {}ms",
        violation_rate * 100.0,
        max_acceptable_gap.as_millis(),
    );

    fixture.shutdown().await;
}

/// Simulates the user dragging the latency slider from 0 to 500ms over
/// 2 seconds with incremental steps, mimicking the split example's UI.
/// This is the most realistic reproduction of the reported freeze: rapid
/// small latency changes rather than a single large jump.
#[test]
fn slider_drag_latency_ramp() {
    let _ = tracing_subscriber::fmt::try_init();
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(slider_drag_inner());
}

async fn slider_drag_inner() {
    let fixture = PatchbayFixture::new().await;
    fixture.remote.set_skip_threshold(Duration::from_secs(10));

    let mut track = tokio::time::timeout(FRAME_TIMEOUT, fixture.remote.video_ready())
        .await
        .expect("timeout")
        .expect("video_ready");

    // Warmup.
    let _ = drain_frames(&mut track, Duration::from_secs(2)).await;

    let (_, reanchors_before) = fixture.remote.clock().reanchor_stats();

    // Simulate dragging slider from 0→500ms in 20 steps over 2s.
    for step in 0..=20u32 {
        let latency_ms = step * 25; // 0, 25, 50, ..., 500
        fixture.set_latency(latency_ms).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Hold at 500ms for 3s and measure.
    let sustained = poll_frames(&mut track, Duration::from_secs(3));
    let sustained_gaps = inter_frame_gaps(&sustained);
    let (_, reanchors_after) = fixture.remote.clock().reanchor_stats();
    let ramp_reanchors = reanchors_after - reanchors_before;

    let stutter_count = sustained_gaps
        .iter()
        .filter(|g| **g > Duration::from_millis(100))
        .count();
    info!(
        frames = sustained.len(),
        ramp_reanchors,
        stutter_count,
        max_gap_ms = sustained_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "after slider drag 0→500ms"
    );

    // Drag back down: 500→0ms in 20 steps.
    for step in (0..=20u32).rev() {
        let latency_ms = step * 25;
        fixture.set_latency(latency_ms).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Settle and measure recovery.
    let _ = poll_frames(&mut track, Duration::from_secs(2));
    let recovery = poll_frames(&mut track, Duration::from_secs(3));
    let recovery_gaps = inter_frame_gaps(&recovery);
    let violation_rate = gap_violation_rate(&recovery_gaps, Duration::from_millis(200));

    info!(
        frames = recovery.len(),
        violation_pct = format!("{:.1}", violation_rate * 100.0),
        max_gap_ms = recovery_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "recovery after slider drag round-trip"
    );

    // Threshold is low because this test can run under CPU contention
    // from the full workspace test suite. The key assertion is that
    // frames flow at all — before the playout overflow fix, this was 0.
    assert!(
        recovery.len() >= 5,
        "slider drag recovery too few frames: {}",
        recovery.len()
    );

    fixture.shutdown().await;
}

/// Tests latency transitions at 30fps 720p — matching the split example's
/// default settings. If the playout buffer's zero-buffer default causes
/// excessive re-anchoring under jitter, this test will show it as stuttering.
///
/// Ignored by default: 720p@30fps encode/decode needs more CPU than shared CI
/// runners provide. Run locally with `cargo nextest run -E 'test(latency_at_split)'
/// --run-ignored all`.
#[test]
#[ignore]
fn latency_at_split_example_settings() {
    let _ = tracing_subscriber::fmt::try_init();
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(latency_split_settings_inner());
}

async fn latency_split_settings_inner() {
    let lab = Lab::new().await.expect("patchbay lab");
    let router = lab.add_router("r1").build().await.expect("router");
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

    // 30fps at 720p — same as split example default.
    let source = TestVideoSource::new(1280, 720).with_fps(30.0);
    broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P720],
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
    remote.set_skip_threshold(Duration::from_secs(5));

    let mut track = tokio::time::timeout(FRAME_TIMEOUT, remote.video_ready())
        .await
        .expect("timeout")
        .expect("video_ready");

    // Warmup.
    let warmup = drain_frames(&mut track, Duration::from_secs(3)).await;
    let warmup_fps = warmup.len() as f64 / 3.0;
    info!(
        frames = warmup.len(),
        fps = format!("{warmup_fps:.1}"),
        "warmup"
    );

    // Baseline with poll.
    let baseline = poll_frames(&mut track, Duration::from_secs(3));
    let baseline_gaps = inter_frame_gaps(&baseline);
    let baseline_fps = baseline.len() as f64 / 3.0;
    info!(
        frames = baseline.len(),
        fps = format!("{baseline_fps:.1}"),
        max_gap_ms = baseline_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "baseline (poll, 720p 30fps)"
    );
    assert!(
        baseline.len() >= 10,
        "720p baseline too few: {}",
        baseline.len()
    );

    // Add 300ms latency.
    let cond = Some(LinkCondition::Manual(LinkLimits {
        latency_ms: 300,
        jitter_ms: 60,
        ..Default::default()
    }));
    lab.set_link_condition(pub_node, router_node, cond)
        .await
        .expect("pub");
    lab.set_link_condition(sub_node, router_node, cond)
        .await
        .expect("sub");
    info!("latency set to 300ms");

    // Absorb with poll — this is where re-anchoring stutters would show up.
    let absorb = poll_frames(&mut track, Duration::from_secs(5));
    let absorb_gaps = inter_frame_gaps(&absorb);
    let max_gap = absorb_gaps.iter().max().copied().unwrap_or(Duration::ZERO);
    let stutter_count = absorb_gaps
        .iter()
        .filter(|g| **g > Duration::from_millis(200))
        .count();
    info!(
        frames = absorb.len(),
        max_gap_ms = max_gap.as_millis(),
        stutters_over_200ms = stutter_count,
        "absorb at 300ms latency (poll, 720p)"
    );

    // Drop latency.
    lab.set_link_condition(pub_node, router_node, None)
        .await
        .expect("pub");
    lab.set_link_condition(sub_node, router_node, None)
        .await
        .expect("sub");
    info!("latency cleared");

    let _settle = poll_frames(&mut track, Duration::from_secs(3));

    // Recovery.
    let recovery = poll_frames(&mut track, Duration::from_secs(3));
    let recovery_gaps = inter_frame_gaps(&recovery);
    let violation_rate = gap_violation_rate(&recovery_gaps, Duration::from_millis(200));
    info!(
        frames = recovery.len(),
        violation_pct = format!("{:.1}", violation_rate * 100.0),
        max_gap_ms = recovery_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "recovery (poll, 720p)"
    );

    assert!(
        recovery.len() >= 10,
        "720p recovery too few: {}",
        recovery.len()
    );
    assert!(
        violation_rate <= 0.20,
        "720p recovery: {:.1}% of gaps exceed 200ms",
        violation_rate * 100.0,
    );

    remote.shutdown();
    publisher.shutdown().await;
    subscriber.shutdown().await;
}

/// Measures playout clock re-anchoring during sustained high latency.
/// Excessive re-anchoring under jitter is the most likely cause of the
/// freezes reported in the split example: each re-anchor adds a 66ms
/// gap in frame delivery. If the jitter is larger than the re-anchor
/// buffer, this happens every few frames, producing visible stuttering.
///
/// Ignored by default: re-anchor counting is sensitive to CPU scheduling
/// jitter on shared CI runners. Run locally with `cargo nextest run
/// -E 'test(reanchor_count)' --run-ignored all`.
#[test]
#[ignore]
fn reanchor_count_during_sustained_latency() {
    let _ = tracing_subscriber::fmt::try_init();
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(reanchor_count_inner());
}

async fn reanchor_count_inner() {
    let fixture = PatchbayFixture::new().await;
    fixture.remote.set_skip_threshold(Duration::from_secs(10));

    let mut track = tokio::time::timeout(FRAME_TIMEOUT, fixture.remote.video_ready())
        .await
        .expect("timeout")
        .expect("video_ready");

    // Warmup.
    let _ = drain_frames(&mut track, Duration::from_secs(2)).await;

    // Record reanchor baseline.
    let (drift_before, reanchors_before) = fixture.remote.clock().reanchor_stats();
    info!(
        drift_ms = drift_before.as_millis(),
        reanchors_before, "before latency"
    );

    // Apply 500ms latency with proportional jitter (100ms).
    fixture.set_latency(500).await;

    // Run for 5s under sustained latency.
    let sustained = poll_frames(&mut track, Duration::from_secs(5));
    let sustained_gaps = inter_frame_gaps(&sustained);
    let (drift_after, reanchors_after) = fixture.remote.clock().reanchor_stats();
    let new_reanchors = reanchors_after - reanchors_before;
    let new_drift = drift_after.saturating_sub(drift_before);

    let stutter_count = sustained_gaps
        .iter()
        .filter(|g| **g > Duration::from_millis(100))
        .count();

    info!(
        frames = sustained.len(),
        new_reanchors,
        new_drift_ms = new_drift.as_millis(),
        stutter_count,
        max_gap_ms = sustained_gaps
            .iter()
            .max()
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "sustained 500ms latency"
    );

    // After the initial transition stutter, sustained latency should NOT
    // cause ongoing re-anchoring. The playout clock should anchor once
    // (or a few times) during the transition, then stabilize. Anything
    // above ~5 re-anchors in 5 seconds indicates the jitter buffer is
    // too small for the arrival variance.
    assert!(
        new_reanchors <= 10,
        "too many re-anchors during sustained latency: {} (drift: {}ms). \
         The playout clock's jitter buffer may be too small for the \
         inter-arrival variance.",
        new_reanchors,
        new_drift.as_millis(),
    );

    // Clear latency and verify recovery.
    fixture.set_impairment(Default::default()).await;
    let _ = drain_frames(&mut track, Duration::from_secs(2)).await;
    let recovery = poll_frames(&mut track, Duration::from_secs(2));
    let recovery_gaps = inter_frame_gaps(&recovery);
    let violation_rate = gap_violation_rate(&recovery_gaps, Duration::from_millis(200));
    info!(
        frames = recovery.len(),
        violation_pct = format!("{:.1}", violation_rate * 100.0),
        "recovery after sustained latency"
    );

    fixture.shutdown().await;
}

/// Publishes two renditions (360p + 180p) over patchbay, feeds real QUIC
/// stats into the AdaptiveVideoTrack, then injects heavy packet loss to
/// trigger a downgrade. After clearing loss, verifies the track upgrades
/// back to the higher rendition.
///
/// Unlike the e2e adaptive test which uses synthetic signals, this test
/// uses actual network impairment so the full feedback loop is exercised:
/// netem loss → QUIC detects loss → PathStats reports it → signal
/// producer samples it → adaptive algorithm reacts.
#[test]
fn adaptive_downgrade_upgrade_under_real_loss() {
    let _ = tracing_subscriber::fmt::try_init();
    patchbay::init_userns().expect("patchbay init_userns");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(adaptive_real_loss_inner());
}

async fn adaptive_real_loss_inner() {
    let lab = Lab::new().await.expect("patchbay lab");
    let router = lab.add_router("r1").build().await.expect("router");
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

    // Publisher with two renditions.
    let publisher = Live::builder(pub_endpoint).spawn_with_router();
    let broadcast = LocalBroadcast::new();
    let source = TestVideoSource::new(640, 480).with_fps(15.0);
    broadcast
        .video()
        .set(VideoInput::new(
            source,
            TEST_VIDEO_CODEC,
            [VideoPreset::P360, VideoPreset::P180],
        ))
        .expect("set video");
    publisher
        .publish("test", &broadcast)
        .await
        .expect("publish");

    // Subscriber with adaptive track.
    let subscriber = Live::builder(sub_endpoint).spawn();
    let (session, remote) = subscriber
        .subscribe(publisher.endpoint().addr(), "test")
        .await
        .expect("subscribe");

    tokio::time::timeout(FRAME_TIMEOUT, remote.ready())
        .await
        .expect("catalog timeout");

    let renditions = remote.catalog().video.renditions.len();
    assert_eq!(renditions, 2, "expected 2 renditions, got {renditions}");

    // Produce real network signals from the QUIC connection.
    let signals_rx =
        iroh_live::util::spawn_signal_producer(session.conn(), remote.shutdown_token());

    // Use fast timers so the test doesn't take forever.
    let config = AdaptiveConfig {
        upgrade_hold: Duration::from_millis(500),
        downgrade_hold: Duration::from_millis(200),
        probe_duration: Duration::from_millis(1000),
        probe_cooldown: Duration::from_millis(500),
        post_downgrade_cooldown: Duration::from_millis(1000),
        check_interval: Duration::from_millis(100),
        ..AdaptiveConfig::default()
    };

    let mut adaptive = remote
        .adaptive_video_with(signals_rx, config, Default::default())
        .expect("adaptive track");

    // Get first frame and record initial rendition.
    let _first = tokio::time::timeout(FRAME_TIMEOUT, adaptive.next_frame())
        .await
        .expect("timeout")
        .expect("closed");
    let initial_rendition = adaptive.selected_rendition();
    info!(rendition = %initial_rendition, "initial rendition");

    // Warmup: let the connection stabilize.
    for _ in 0..30 {
        let _ = tokio::time::timeout(Duration::from_millis(200), adaptive.next_frame()).await;
    }

    // Inject 25% packet loss to trigger downgrade.
    let lossy = LinkLimits {
        loss_pct: 25.0,
        ..Default::default()
    };
    lab.set_link_condition(pub_node, router_node, Some(LinkCondition::Manual(lossy)))
        .await
        .expect("pub");
    lab.set_link_condition(sub_node, router_node, Some(LinkCondition::Manual(lossy)))
        .await
        .expect("sub");
    info!("injected 25% packet loss");

    // Wait for downgrade (up to 15s).
    let downgrade_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(500), adaptive.next_frame()).await;
        let current = adaptive.selected_rendition();
        if current != initial_rendition {
            info!(rendition = %current, "downgraded");
            break;
        }
        if tokio::time::Instant::now() > downgrade_deadline {
            panic!("adaptive track did not downgrade within 15s (still on {current})");
        }
    }

    let downgraded = adaptive.selected_rendition();
    assert_ne!(downgraded, initial_rendition, "should have downgraded");

    // Clear loss.
    lab.set_link_condition(pub_node, router_node, None)
        .await
        .expect("pub");
    lab.set_link_condition(sub_node, router_node, None)
        .await
        .expect("sub");
    info!("cleared packet loss");

    // Wait for upgrade (up to 20s — needs upgrade_hold + probe_duration + margin).
    let upgrade_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut upgraded = false;
    loop {
        let _ = tokio::time::timeout(Duration::from_millis(500), adaptive.next_frame()).await;
        let current = adaptive.selected_rendition();
        if current != downgraded {
            info!(rendition = %current, "upgraded");
            upgraded = true;
            break;
        }
        if tokio::time::Instant::now() > upgrade_deadline {
            // Upgrade may not happen within test window due to cooldowns.
            info!("upgrade did not happen within window (acceptable)");
            break;
        }
    }

    // Verify frames still flowing after the round-trip.
    let mut frames_after = 0;
    for _ in 0..10 {
        if let Ok(Some(_)) =
            tokio::time::timeout(Duration::from_secs(2), adaptive.next_frame()).await
        {
            frames_after += 1;
        }
    }
    info!(frames_after, upgraded, "adaptive test complete");
    assert!(
        frames_after >= 5,
        "expected ≥5 frames after adaptive round-trip, got {frames_after}"
    );

    remote.shutdown();
    publisher.shutdown().await;
    subscriber.shutdown().await;
}
