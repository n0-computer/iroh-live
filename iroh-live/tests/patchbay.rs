//! Network-simulation tests: the media pipeline over a link that is really impaired.
//!
//! [`e2e`](../e2e.rs) proves the pipeline works when nothing is wrong with the
//! transport, and drives adaptation by pushing made-up
//! [`NetworkSignals`](moq_media::net::NetworkSignals) into a watch channel.
//! Neither says anything about the chain those signals come from. These tests
//! put a publisher and a subscriber in separate network namespaces with a router
//! between them, apply netem latency, jitter and loss to the links,
//! and let the real chain run: the impairment reaches QUIC, QUIC reports it
//! through path stats, the signal producer samples those, and the adaptation
//! loop decides. That loop is the thing under test here.
//!
//! Linux only: the lab is built out of unprivileged user namespaces, which is
//! also why the namespace setup runs from an ELF initialiser below rather than
//! from a test.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use iroh::{Endpoint, endpoint::presets};
use iroh_live::Live;
use moq_media::{
    adaptive::AdaptiveConfig,
    publish::{LocalBroadcast, VideoRendition},
    subscribe::VideoTrack,
    test_source,
    video::Size,
};
use n0_tracing_test::traced_test;
use patchbay::{Lab, LinkCondition, LinkLimits, NodeId};
use tracing::info;

/// Sets up the user namespace the lab needs.
///
/// Unshare only works from a single-threaded process, and the test harness has
/// already spawned its threads by the time the first test runs, so this has to
/// happen in `.init_array` rather than anywhere reachable from a test body.
#[ctor::ctor]
fn patchbay_init() {
    // SAFETY: runs from `.init_array`, single-threaded, before `main`.
    unsafe { patchbay::init_userns_for_ctor() };
}

/// Generous, because openh264 encodes in software, the tests hold the machine
/// one at a time but share it with whatever else is running, and every switch
/// waits on a keyframe crossing an impaired link.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Low enough that a debug-build software encoder keeps up, so a missing frame
/// means the transport lost it rather than the CPU never producing it.
const FRAMERATE: u32 = 15;

/// The frame interval implied by [`FRAMERATE`], as the gap thresholds' unit.
const FRAME_INTERVAL: Duration = Duration::from_millis(1000 / FRAMERATE as u64);

/// The gap a frame may fall behind by and still count as smooth delivery.
///
/// Three frame intervals, which absorbs a scheduling hiccup and one dropped
/// frame without absorbing a stall.
const SMOOTH: Duration = FRAME_INTERVAL.saturating_mul(3);

/// A publisher and a subscriber either side of a router, each in its own
/// namespace, with both links available for impairment.
struct Fixture {
    lab: Lab,
    publisher_node: NodeId,
    subscriber_node: NodeId,
    router_node: NodeId,
    publisher: Live,
    /// Held because dropping it stops the publish task.
    _broadcast: LocalBroadcast,
    subscriber: Live,
    subscription: iroh_live::Subscription,
}

impl Fixture {
    /// Builds the lab and publishes `renditions` of a generated pattern at
    /// `size`, subscribed from the far side of the router.
    async fn start(size: Size, renditions: Vec<VideoRendition>) -> Self {
        let lab = Lab::new().await.expect("failed to build the lab");
        let router = lab
            .add_router("r1")
            .build()
            .await
            .expect("failed to build the router");
        let router_node = router.id();

        let publisher_device = lab
            .add_device("publisher")
            .iface("eth0", router_node, None)
            .build()
            .await
            .expect("failed to build the publisher device");
        let publisher_node = publisher_device.id();

        let subscriber_device = lab
            .add_device("subscriber")
            .iface("eth0", router_node, None)
            .build()
            .await
            .expect("failed to build the subscriber device");
        let subscriber_node = subscriber_device.id();

        // Each endpoint has to bind inside its own namespace, which is what
        // `spawn` is for: the closure runs on the device's network stack.
        let publisher_endpoint = publisher_device
            .spawn(|_device| async move { Endpoint::builder(presets::Minimal).bind().await })
            .expect("failed to spawn on the publisher device")
            .await
            .expect("the publisher bind task failed")
            .expect("failed to bind the publisher endpoint");
        let subscriber_endpoint = subscriber_device
            .spawn(|_device| async move { Endpoint::builder(presets::Minimal).bind().await })
            .expect("failed to spawn on the subscriber device")
            .await
            .expect("the subscriber bind task failed")
            .expect("failed to bind the subscriber endpoint");

        let publisher = Live::builder(publisher_endpoint).with_router().spawn();
        let broadcast = publisher.publish("patchbay").expect("failed to publish");
        broadcast
            .video()
            .set_renditions(test_source::video(size, FRAMERATE), renditions)
            .expect("failed to set video");

        // No address lookup: the lab gives each device a fixed address, so the
        // publisher's is handed over directly.
        let publisher_addr = publisher.endpoint().addr();
        let subscriber = Live::builder(subscriber_endpoint).spawn();
        let subscription = subscriber
            .subscribe(publisher_addr, "patchbay")
            .await
            .expect("failed to subscribe");

        Self {
            lab,
            publisher_node,
            subscriber_node,
            router_node,
            publisher,
            _broadcast: broadcast,
            subscriber,
            subscription,
        }
    }

    /// Applies `limits` to both the publisher's and the subscriber's link.
    ///
    /// Both, because the router only forwards: impairing one leg would leave
    /// the other free to carry acknowledgements at full speed, which is not a
    /// shape any real path has.
    async fn impair(&self, limits: LinkLimits) {
        self.set_condition(Some(LinkCondition::Manual(limits)))
            .await;
        info!(?limits, "link impaired");
    }

    /// Removes all impairment from both links.
    async fn clear(&self) {
        self.set_condition(None).await;
        info!("link cleared");
    }

    async fn set_condition(&self, condition: Option<LinkCondition>) {
        for node in [self.publisher_node, self.subscriber_node] {
            self.lab
                .set_link_condition(node, self.router_node, condition)
                .await
                .expect("failed to set the link condition");
        }
    }

    /// Opens the video track, waiting for the catalog to carry `renditions` of
    /// them first.
    async fn video(&self, renditions: usize) -> VideoTrack {
        let broadcast = self.subscription.broadcast();
        tokio::time::timeout(TIMEOUT, async {
            while broadcast.catalog().video().len() < renditions {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("timed out waiting for the video catalog");

        broadcast
            .video()
            .await
            .expect("failed to open the video track")
    }

    async fn shutdown(self) {
        self.publisher.shutdown().await;
        self.subscriber.shutdown().await;
    }
}

/// Reads frames for `duration`, returning the instant each one arrived.
///
/// Reads rather than polls: the frame slot keeps only the newest frame, so a
/// poll loop measures its own cadence as much as the pipeline's.
async fn drain(track: &VideoTrack, duration: Duration) -> Vec<Instant> {
    let deadline = Instant::now() + duration;
    let mut arrivals = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match tokio::time::timeout(remaining, track.recv()).await {
            Ok(Some(_frame)) => arrivals.push(Instant::now()),
            // The track ended, so waiting out the rest of the window would only
            // delay the assertion that is about to fail.
            Ok(None) => break,
            Err(_) => break,
        }
    }
    arrivals
}

/// Returns the interval between consecutive arrivals.
fn gaps(arrivals: &[Instant]) -> Vec<Duration> {
    arrivals
        .windows(2)
        .map(|pair| pair[1].duration_since(pair[0]))
        .collect()
}

/// Returns the fraction of `gaps` longer than `threshold`, in `0.0..=1.0`.
fn over(gaps: &[Duration], threshold: Duration) -> f64 {
    if gaps.is_empty() {
        return 1.0;
    }
    let count = gaps.iter().filter(|gap| **gap > threshold).count();
    count as f64 / gaps.len() as f64
}

/// Returns the longest gap, or zero if there were fewer than two arrivals.
fn longest(gaps: &[Duration]) -> Duration {
    gaps.iter().copied().max().unwrap_or_default()
}

/// Logs a phase's frame count and gap distribution, so a failure has the
/// numbers next to it in the captured output rather than only the assertion.
fn report(phase: &str, arrivals: &[Instant], window: Duration) {
    let gaps = gaps(arrivals);
    info!(
        phase,
        frames = arrivals.len(),
        fps = format!("{:.1}", arrivals.len() as f64 / window.as_secs_f64()),
        longest_gap_ms = longest(&gaps).as_millis() as u64,
        rough = format!("{:.0}%", over(&gaps, SMOOTH) * 100.0),
        "phase measured",
    );
}

/// A ladder wide enough for the adaptation loop to have somewhere to go.
///
/// A rendition's bitrate is a ceiling handed to the encoder, not a promise, and
/// openh264 spends nothing it does not need: measured over a clear link, this
/// pattern arrives at about 316 kbit/s on `high` and 84 on `low`, some 40% of
/// what each declares. That gap is why nothing here turns on the arriving
/// goodput reaching the declared figure. It is also why an impairment has to be
/// tighter than the ladder suggests before it binds on anything.
fn ladder() -> Vec<VideoRendition> {
    vec![
        VideoRendition::new("high").with_bitrate(800_000),
        VideoRendition::new("low")
            .with_size(Size::new(320, 240))
            .with_bitrate(200_000),
    ]
}

/// The longest a change on the link may take to reach the signals.
///
/// Part of the claim rather than a convenience. A bandwidth figure that arrives
/// at the right answer a minute later has not measured the link, it has been
/// dragged along by it, and a loop that acts on it is acting on the link the
/// subscriber had rather than the one it has. Measured through this lab the
/// goodput window follows a cap in two to three seconds, so this leaves room
/// for a loaded machine without leaving room for a signal that lags.
const SIGNAL_LAG: Duration = Duration::from_secs(15);

/// Timers short enough for a switch to happen inside the test's own budget.
///
/// Only the timers are shortened. The thresholds are left at their defaults,
/// because those are the part being tested: a test that also moved the loss and
/// bandwidth limits would be checking arithmetic it had just written.
fn quick_adaptation() -> AdaptiveConfig {
    AdaptiveConfig {
        downgrade_hold: Duration::from_millis(300),
        upgrade_hold: Duration::from_millis(500),
        probe_duration: Duration::from_millis(500),
        probe_cooldown: Duration::from_secs(1),
        post_downgrade_cooldown: Duration::from_secs(1),
        check_interval: Duration::from_millis(100),
        ..AdaptiveConfig::default()
    }
}

/// Raising the latency must not stop frames arriving, and dropping it back must
/// return delivery to the cadence it had before.
///
/// The failure this guards against is a pipeline that treats a latency step as
/// an end of stream: frames stop, and nothing restarts them when the link comes
/// back.
#[tokio::test]
#[traced_test]
async fn frames_survive_a_latency_ramp() {
    let fixture = Fixture::start(
        Size::new(320, 240),
        vec![VideoRendition::new("video").with_bitrate(500_000)],
    )
    .await;
    let track = fixture.video(1).await;

    // Encoder and decoder startup, the QUIC handshake tail, and namespace
    // setup all land in the first couple of seconds and none of them are what
    // is being measured.
    let warmup = drain(&track, Duration::from_secs(2)).await;
    info!(frames = warmup.len(), "warmed up");

    let window = Duration::from_secs(3);
    let baseline = drain(&track, window).await;
    report("baseline", &baseline, window);
    assert!(
        baseline.len() >= 10,
        "expected at least 10 frames in {window:?} at {FRAMERATE}fps before impairment, got {}",
        baseline.len(),
    );

    fixture
        .impair(LinkLimits {
            latency_ms: 300,
            jitter_ms: 60,
            ..Default::default()
        })
        .await;
    let ramp = drain(&track, Duration::from_secs(5)).await;
    report("latency 300ms", &ramp, Duration::from_secs(5));
    // Deliberately loose: 600ms of added round trip pushes frames late, and the
    // claim is that they still come, not that they come on time.
    assert!(
        ramp.len() >= 15,
        "expected at least 15 frames across 5s at 300ms latency, got {} (stalled?)",
        ramp.len(),
    );

    fixture.clear().await;
    // In-flight packets are still traversing the old delay, and the congestion
    // controller has a round trip's worth of stale estimate to work off.
    let settle = drain(&track, Duration::from_secs(3)).await;
    info!(frames = settle.len(), "settled");

    let recovery = drain(&track, window).await;
    report("recovery", &recovery, window);
    let recovery_gaps = gaps(&recovery);
    assert!(
        recovery.len() >= 20,
        "expected at least 20 frames in {window:?} after the latency cleared, got {}",
        recovery.len(),
    );
    let rough = over(&recovery_gaps, SMOOTH);
    assert!(
        rough <= 0.10,
        "delivery did not recover: {:.0}% of gaps exceed {}ms, longest {}ms",
        rough * 100.0,
        SMOOTH.as_millis(),
        longest(&recovery_gaps).as_millis(),
    );

    fixture.shutdown().await;
}

/// Losing a fifth of the packets must degrade delivery rather than end it, and
/// clearing the loss must return it.
///
/// QUIC retransmits, so the stream should survive; what this catches is a
/// decoder that gives up on the first gap in its input instead of waiting for
/// the retransmission.
#[tokio::test]
#[traced_test]
async fn frames_survive_a_loss_spike() {
    let fixture = Fixture::start(
        Size::new(320, 240),
        vec![VideoRendition::new("video").with_bitrate(500_000)],
    )
    .await;
    let track = fixture.video(1).await;

    let _warmup = drain(&track, Duration::from_secs(2)).await;
    let window = Duration::from_secs(2);
    let baseline = drain(&track, window).await;
    report("baseline", &baseline, window);
    assert!(
        baseline.len() >= 8,
        "expected at least 8 frames in {window:?} before impairment, got {}",
        baseline.len(),
    );

    fixture
        .impair(LinkLimits {
            loss_pct: 20.0,
            ..Default::default()
        })
        .await;
    let lossy = drain(&track, Duration::from_secs(3)).await;
    report("20% loss", &lossy, Duration::from_secs(3));
    assert!(
        lossy.len() >= 10,
        "expected at least 10 frames across 3s at 20% loss, got {} (stalled?)",
        lossy.len(),
    );

    fixture.clear().await;
    let _settle = drain(&track, Duration::from_secs(3)).await;

    let window = Duration::from_secs(3);
    let recovery = drain(&track, window).await;
    report("recovery", &recovery, window);
    let recovery_gaps = gaps(&recovery);
    assert!(
        recovery.len() >= 20,
        "expected at least 20 frames in {window:?} after the loss cleared, got {}",
        recovery.len(),
    );
    let rough = over(&recovery_gaps, SMOOTH);
    assert!(
        rough <= 0.10,
        "delivery did not recover: {:.0}% of gaps exceed {}ms, longest {}ms",
        rough * 100.0,
        SMOOTH.as_millis(),
        longest(&recovery_gaps).as_millis(),
    );

    fixture.shutdown().await;
}

/// The whole adaptive loop, end to end, with nothing about it simulated:
/// netem drops packets, QUIC's loss detection declares them lost, the signal
/// producer reads that out of the path stats, and the adaptation loop steps down
/// the ladder. Clearing the impairment steps it back up.
///
/// This is the one thing here that no other test in any of the repos covers.
/// `e2e::adaptive_rendition_switching` reaches the same decision by writing the
/// signals by hand, which exercises the algorithm but not the four stages that
/// feed it.
#[tokio::test]
#[traced_test]
async fn adaptation_follows_a_real_link() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let track = fixture.video(2).await;

    assert_eq!(
        track.rendition(),
        "high",
        "a fresh subscription should start at the top of the ladder",
    );

    // Wait for a frame before impairing anything, so the downgrade is measured
    // against a link that was carrying video rather than one still opening.
    tokio::time::timeout(TIMEOUT, track.recv())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video track closed before its first frame");

    track.enable_adaptation_with(fixture.subscription.signals().clone(), quick_adaptation());

    // Loss on both legs, so it reaches the subscriber's own transmissions:
    // acknowledgements are dropped in proportion to the impairment like
    // anything else, and `NetworkSignals::loss_rate` counts what this endpoint
    // sent. `adaptation_follows_a_rate_limit` covers the case this cannot,
    // where the downlink runs out of room without dropping anything.
    //
    // 12% on each of the two links the media crosses is measured as far more
    // than 12%, because the loss rate is a ratio over a 200ms window and the
    // subscriber sends few enough packets in one for a couple of losses to
    // dominate it. That puts it past the emergency threshold rather than the
    // sustained one, so the drop goes straight to the bottom of the ladder,
    // which with two rungs is where a graduated downgrade would have gone too.
    // The link still carries the replacement rendition's keyframe.
    fixture
        .impair(LinkLimits {
            loss_pct: 12.0,
            ..Default::default()
        })
        .await;

    let downgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, track.switched_to("low"))
        .await
        .expect("timed out waiting for a downgrade to `low`");
    info!(
        after_ms = downgraded.elapsed().as_millis() as u64,
        "downgraded"
    );

    fixture.clear().await;

    let upgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, track.switched_to("high"))
        .await
        .expect("timed out waiting for an upgrade back to `high`");
    info!(after_ms = upgraded.elapsed().as_millis() as u64, "upgraded");

    fixture.shutdown().await;
}

/// The same loop again, driven by an impairment that drops nothing.
///
/// A rate limit is the shape of downlink trouble a subscriber is worst placed
/// to see. Nothing is lost, so `loss_rate` stays at zero, and the congestion
/// window and congestion counter both describe the direction the subscriber
/// sends in, where a handful of acknowledgements never runs out of room. What
/// is left is the pair this asserts on: the bytes arriving pin to the cap, and
/// the round trip inflates because the queue holding up the media holds up the
/// acknowledgements behind it. Measured here, 200 kbit/s against a 316 kbit/s
/// stream takes goodput to about 195 and the round trip from 1ms to 330 while
/// loss stays at exactly zero throughout.
///
/// This is the test the previous, congestion-window-derived bandwidth estimate
/// could not pass: an application-limited window stays wide whatever the far
/// end is doing, so it read a capped link as an idle one.
#[tokio::test]
#[traced_test]
async fn adaptation_follows_a_rate_limit() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let track = fixture.video(2).await;
    let signals = fixture.subscription.signals().clone();

    assert_eq!(
        track.rendition(),
        "high",
        "a fresh subscription should start at the top of the ladder",
    );

    // Frames first, so the cap lands on a link that was carrying video and the
    // producer has a round trip and a goodput window off a healthy path to
    // compare against.
    tokio::time::timeout(TIMEOUT, track.recv())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video track closed before its first frame");
    let _settle = drain(&track, Duration::from_secs(2)).await;

    let config = quick_adaptation();
    track.enable_adaptation_with(signals.clone(), config.clone());

    // Two thirds of the 316 kbit/s `high` actually sends, and two and a half
    // times the 84 `low` does, so the top rung cannot fit and the bottom one
    // comfortably can.
    fixture
        .impair(LinkLimits {
            rate_kbit: 200,
            ..Default::default()
        })
        .await;

    // Held for three times the downgrade hold, so the loop has had the reading
    // in front of it for longer than it needs to act on it.
    let held = config.downgrade_hold * 3;
    let mut worst_loss: f64 = 0.0;
    let impaired = Instant::now();
    let saw_the_cap = tokio::time::timeout(SIGNAL_LAG, async {
        let mut since = None;
        loop {
            let signals = *signals.borrow();
            worst_loss = worst_loss.max(signals.loss_rate);
            let pinned = signals.goodput_bps.is_some_and(|bps| bps < 250_000);
            let queued = signals.rtt > signals.min_rtt * 10;
            match (pinned && queued, since) {
                (true, Some(start)) if Instant::now().duration_since(start) >= held => {
                    return signals;
                }
                (true, Some(_)) => {}
                (true, None) => since = Some(Instant::now()),
                (false, _) => since = None,
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the signals did not show the rate limit inside {SIGNAL_LAG:?}"));
    info!(
        after_ms = impaired.elapsed().as_millis() as u64,
        goodput_kbps = saw_the_cap.goodput_bps.unwrap_or(0) / 1000,
        rtt_ms = saw_the_cap.rtt.as_millis() as u64,
        min_rtt_ms = saw_the_cap.min_rtt.as_millis() as u64,
        worst_loss,
        "the rate limit reached the signals",
    );

    // The point of the whole test: nothing was dropped, so nothing the loop
    // knew about loss could have moved it. Checked against the threshold the
    // loop actually uses rather than against zero, since a retransmission for
    // some other reason is always possible.
    assert!(
        worst_loss < config.loss_downgrade,
        "loss reached {worst_loss}, so the downgrade cannot be credited to the bandwidth signal",
    );

    // Lifted before the switch is asked to complete. A cap that starves the top
    // rung is by construction too small for both rungs at once, and the two
    // overlap by design while the replacement decoder waits for its first
    // frame: the subscriber holds the incumbent so the picture does not blank.
    // Under that overlap the replacement's subscription does not get set up at
    // all, which is worth knowing and is not what this test is about.
    fixture.clear().await;

    let downgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, track.switched_to("low"))
        .await
        .expect("timed out waiting for a downgrade to `low`");
    info!(
        after_ms = downgraded.elapsed().as_millis() as u64,
        "downgraded"
    );

    let upgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, track.switched_to("high"))
        .await
        .expect("timed out waiting for an upgrade back to `high`");
    info!(after_ms = upgraded.elapsed().as_millis() as u64, "upgraded");

    fixture.shutdown().await;
}

/// A rendition switch must not blank the picture.
///
/// The decode supervisor opens the replacement alongside the incumbent and hands
/// over on the replacement's first frame, so delivery should carry on through
/// the switch at roughly its usual cadence. If that overlap regressed, the gap
/// would be the whole cost of opening a decoder and waiting for a keyframe over
/// the impaired link, which is seconds rather than frames.
///
/// Driven by an explicit switch rather than by adaptation: the assertion is
/// about the handover, and waiting for the algorithm to ask for one would only
/// add the time it takes to decide.
#[tokio::test]
#[traced_test]
async fn a_switch_does_not_blank_the_picture() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let track = fixture.video(2).await;

    // Enough latency that the switch has to cross a link with real delay on it,
    // not so much that the baseline cadence is itself in question.
    fixture
        .impair(LinkLimits {
            latency_ms: 50,
            jitter_ms: 10,
            ..Default::default()
        })
        .await;

    let _warmup = drain(&track, Duration::from_secs(3)).await;
    let window = Duration::from_secs(2);
    let baseline = drain(&track, window).await;
    report("baseline", &baseline, window);
    assert!(
        baseline.len() >= 8,
        "expected at least 8 frames in {window:?} before the switch, got {}",
        baseline.len(),
    );

    track.set_rendition("low");
    let window = Duration::from_secs(5);
    let across = drain(&track, window).await;
    report("across the switch", &across, window);

    assert_eq!(
        track.rendition(),
        "low",
        "the switch did not land inside {window:?}",
    );

    // Ten frame intervals. A handover that kept the incumbent running costs
    // nothing beyond normal pacing; one that tore it down first costs a decoder
    // open and a keyframe, which at this frame rate is far more than ten.
    let blank = FRAME_INTERVAL * 10;
    let across_gaps = gaps(&across);
    assert!(
        longest(&across_gaps) <= blank,
        "the picture went blank for {}ms across the switch, more than the {}ms a handover should cost",
        longest(&across_gaps).as_millis(),
        blank.as_millis(),
    );

    fixture.shutdown().await;
}

/// A switch asked for while the link is saturated must still land.
///
/// The reproduction for the stall `adaptation_follows_a_rate_limit` documents
/// and works around by lifting the cap first. The cap is held across the whole
/// switch here, which is the shape the real case has: the link is saturated,
/// that is why a lower rendition is wanted, and lifting the cap is not
/// something a subscriber can do.
///
/// Driven by an explicit `set_rendition` rather than by adaptation, so a
/// failure is about the switch and not about the decision to make one.
///
/// Ignored because it fails, and the cause is upstream. `moq_net` gives every
/// group stream a QUIC send order of `u8::MAX - rank` and leaves every control
/// stream at quinn's default of zero, and quinn schedules strictly by priority.
/// A publisher whose media backlog never drains therefore never transmits the
/// TRACK_INFO answering the replacement's request, and the subscriber's
/// `track::Consumer::subscribe` waits on it forever. Un-ignore once control
/// streams outrank media.
#[tokio::test]
#[traced_test]
async fn a_switch_lands_while_the_link_stays_capped() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let track = fixture.video(2).await;
    let signals = fixture.subscription.signals().clone();

    tokio::time::timeout(TIMEOUT, track.recv())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video track closed before its first frame");
    let _settle = drain(&track, Duration::from_secs(2)).await;

    // Two thirds of what `high` actually sends, so the top rung cannot fit and
    // the send queue never drains.
    fixture
        .impair(LinkLimits {
            rate_kbit: 200,
            ..Default::default()
        })
        .await;

    // Wait until the cap is visible in the signals, so the switch is asked for
    // on a link that is demonstrably saturated rather than one still filling.
    tokio::time::timeout(SIGNAL_LAG, async {
        loop {
            let signals = *signals.borrow();
            if signals.goodput_bps.is_some_and(|bps| bps < 250_000)
                && signals.rtt > signals.min_rtt * 10
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the signals did not show the rate limit inside {SIGNAL_LAG:?}"));
    info!("the link is saturated");

    let asked = Instant::now();
    track.set_rendition("low");
    tokio::time::timeout(TIMEOUT, track.switched_to("low"))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the switch to `low` did not land inside {TIMEOUT:?} while the cap was held, \
                 still on `{}`",
                track.rendition(),
            )
        });
    info!(after_ms = asked.elapsed().as_millis() as u64, "switched");

    fixture.shutdown().await;
}
