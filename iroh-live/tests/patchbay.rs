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

/// What arrived while a rendition handover was in progress, and what arrived
/// once it had finished.
///
/// The two are kept apart because only the first says anything about the
/// handover. A gap during it is the cost of the switch; a gap after it is
/// ordinary delivery jitter on the new rendition, which `frames_survive_a_*`
/// already covers and which reaching a switch's tolerance would only make the
/// switch look expensive.
struct Handover {
    /// Arrivals from the moment the switch was asked for through to the
    /// replacement's first frame.
    across: Vec<Instant>,
    /// Arrivals in the settling window that followed.
    after: Vec<Instant>,
    /// How long the replacement took to open, subscribe, and produce a frame.
    took: Duration,
}

/// Reads frames across a switch to `rendition`, and for `settle` after it lands.
///
/// A fixed window would have to be long enough for the slowest handover the
/// machine can produce and short enough for the gaps inside it to still mean
/// something, and there is no width that is both. Following the switch itself
/// removes the choice: the window ends when the thing being measured has
/// happened.
async fn drain_across_switch(track: &VideoTrack, rendition: &str, settle: Duration) -> Handover {
    let asked = Instant::now();
    let mut across = Vec::new();
    {
        let switched = track.switched_to(rendition);
        tokio::pin!(switched);
        loop {
            tokio::select! {
                frame = track.recv() => match frame {
                    Some(_frame) => across.push(Instant::now()),
                    // The track ended, so there is nothing left to measure.
                    None => return Handover { across, after: Vec::new(), took: asked.elapsed() },
                },
                () = &mut switched => break,
            }
        }
    }
    let took = asked.elapsed();
    let mut after = drain(track, settle).await;
    // The replacement's first frame is what flips the rendition, so it is read
    // out just after the switch has landed rather than before. It closes the one
    // gap the whole test is about, the one between the incumbent's last frame
    // and the replacement's first, so it belongs on the near side of the line.
    if !after.is_empty() {
        across.push(after.remove(0));
    }
    Handover {
        across,
        after,
        took,
    }
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

/// The longest the round trip readings that corroborate a queue may take to
/// arrive, on top of [`SIGNAL_LAG`].
///
/// Not a claim about the link, which is why it is separate. QUIC takes a round
/// trip sample only from a packet that asks to be acknowledged, and a subscriber
/// mostly sends acknowledgements, so fresh readings arrive seconds apart on a
/// path that is otherwise busy. The adaptation loop counts those readings rather
/// than elapsed time, and this is the room it needs to collect them.
const RTT_CORROBORATION: Duration = Duration::from_secs(30);

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
///
/// The cap has to come off before the switch is waited for, so this test cannot
/// wait for the downgrade and then check the signals; it has to establish that
/// the downgrade is due first. What that takes is the loop's own precondition,
/// distinct round trip readings and not elapsed time, which is why the wait
/// below counts them.
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

    let config = AdaptiveConfig {
        // The cap is lifted the moment the downgrade is due, so conditions turn
        // good while the replacement decoder is still opening. The default
        // cooldown would let the loop decide to come back up inside that gap,
        // and `low` would never reach the screen for the assertion below to see.
        post_downgrade_cooldown: Duration::from_secs(10),
        ..quick_adaptation()
    };
    // Enabled before the settling drain rather than after it, so the fallback
    // rule has seconds of clear-link goodput behind it should the estimate be
    // absent. With the estimate present the loop needs no history at all, and
    // that is the case this test now exercises: a publisher on BBR3 whose
    // estimate reads the cap.
    track.enable_adaptation_with(signals.clone(), config.clone());

    // Waited for rather than timed. This is the test's own evidence that the
    // cap about to go on is a real shortfall, independent of what the loop
    // decides: the link has to have been seen carrying comfortably more than
    // the cap first. The encoder takes a few seconds to find its real rate (a
    // gradient at 640x480 sits near 200 kbit/s between keyframes and climbs
    // to nearly 300 across one), and a stopwatch cannot know when it has.
    //
    // If the fixture ever stops producing such a stream, this says so in those
    // terms rather than leaving a downgrade to time out further down.
    let cap_kbit = 100;
    let clear_enough = u64::from(cap_kbit) * 1000 * 13 / 10;
    tokio::time::timeout(SIGNAL_LAG, async {
        loop {
            if signals
                .borrow()
                .goodput_bps
                .is_some_and(|bps| bps >= clear_enough)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the clear link never carried {clear_enough} bps, so a {cap_kbit} kbit/s cap is not              a shortfall and this test has nothing to measure",
        )
    });

    // Half of what `high` delivers at its quietest and above the 320x240 `low`
    // at its loudest, so the top rung cannot fit whatever the encoder is doing
    // this second and the bottom one comfortably can.
    fixture
        .impair(LinkLimits {
            rate_kbit: cap_kbit,
            ..Default::default()
        })
        .await;

    // Held for three times the downgrade hold, so the loop has had the reading
    // in front of it for longer than it needs to act on it. The queueing round
    // trip readings counted below are the fallback rule's evidence; the loop
    // acts on the estimate before they add up, and they are kept here so the
    // test still proves the cap reached the signals in every form.
    let held = config.downgrade_hold * 3;
    let mut worst_loss: f64 = 0.0;
    let impaired = Instant::now();
    let (saw_the_cap, readings) = tokio::time::timeout(SIGNAL_LAG + RTT_CORROBORATION, async {
        let mut since = None;
        // Distinct values of `rtt_samples` seen while the cap has been visible
        // without a break. The loop below lifts the cap once its evidence is in,
        // and the loop's evidence is not elapsed time: a queueing round trip
        // only counts towards a downgrade when QUIC measures it again (see
        // `moq_media::adaptive`, `queueing_samples`). Waiting out `held` on a
        // path that handed out one reading throughout satisfies this test and
        // nothing in the adaptation loop, which is what used to make it flake.
        let mut readings = 0u32;
        let mut last_sample = None;
        loop {
            let signals = *signals.borrow();
            worst_loss = worst_loss.max(signals.loss_rate);
            let pinned = signals.goodput_bps.is_some_and(|bps| bps < 250_000);
            let queued = signals.rtt > signals.min_rtt * 10;
            if pinned && queued {
                let start = *since.get_or_insert_with(Instant::now);
                if last_sample != Some(signals.rtt_samples) {
                    last_sample = Some(signals.rtt_samples);
                    readings += 1;
                }
                // One more reading than the loop counts. The loop's window opens
                // a tick after this one does and latches whatever reading is
                // current then, so the first of these may be the one it started
                // from rather than one it counted.
                if Instant::now().duration_since(start) >= held
                    && readings > config.queueing_samples
                {
                    return (signals, readings);
                }
            } else {
                since = None;
                readings = 0;
                last_sample = None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the signals did not show the rate limit, corroborated by {} distinct round trip \
             readings, inside {:?}",
            config.queueing_samples + 1,
            SIGNAL_LAG + RTT_CORROBORATION,
        )
    });
    info!(
        after_ms = impaired.elapsed().as_millis() as u64,
        goodput_kbps = saw_the_cap.goodput_bps.unwrap_or(0) / 1000,
        rtt_ms = saw_the_cap.rtt.as_millis() as u64,
        min_rtt_ms = saw_the_cap.min_rtt.as_millis() as u64,
        rtt_readings = readings,
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

    // The decision, waited for under the cap that caused it. Reconstructing the
    // loop's state from the signals is what this used to do, and it cannot be
    // made reliable: the loop samples the signals on its own interval where
    // this polls them every 50ms, so on a loaded machine two round trip
    // readings can land inside one of its ticks and it counts one where this
    // counts two. Its evidence was then still short when the cap came off, its
    // window reset, and the downgrade never happened.
    tokio::time::timeout(TIMEOUT, track.requested("low"))
        .await
        .expect("timed out waiting for the loop to ask for `low`");

    // Lifted before the switch is asked to complete, and only now that the
    // decision above is a fact rather than an inference. A cap that starves the
    // top rung is by construction too small for both rungs at once, and the two
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

    // Measured across the handover itself rather than across a fixed window:
    // the replacement decoder has to open and wait for a keyframe over the
    // impaired link, and how long that takes is the machine's business, not the
    // claim's. The claim is about what delivery does while it happens.
    let settle = Duration::from_secs(2);
    let handover = tokio::time::timeout(TIMEOUT, drain_across_switch(&track, "low", settle))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the switch to `low` did not land inside {TIMEOUT:?}, still on `{}`",
                track.rendition(),
            )
        });
    report("across the switch", &handover.across, handover.took);
    report("after the switch", &handover.after, settle);

    // The incumbent carries the picture for as long as the replacement takes to
    // open, so the handover window delivers frames at the cadence the baseline
    // just measured. A handover that tore the incumbent down first delivers none
    // at all, whatever that window turns out to be worth on the day, which is
    // why this is stated against the baseline's own rate rather than against a
    // fixed count. Half of it, because both renditions are subscribed at once
    // while the switch is in flight and the incumbent gives up part of the link
    // to the replacement's keyframe.
    let kept_running =
        baseline.len() as u32 * handover.took.as_millis() as u32 / (2 * window.as_millis() as u32);
    assert!(
        handover.across.len() as u32 >= kept_running,
        "only {} frames arrived in the {}ms the switch took, against the {kept_running} that half \
         the baseline's {} frames per {window:?} comes to, so the incumbent stopped delivering \
         while the replacement opened",
        handover.across.len(),
        handover.took.as_millis(),
        baseline.len(),
    );

    // Ten frame intervals. A handover that kept the incumbent running costs
    // nothing beyond normal pacing; one that tore it down first costs a decoder
    // open and a keyframe, which at this frame rate is far more than ten.
    //
    // Applied to the handover window alone. The same threshold over the settling
    // window that follows would be measuring ordinary jitter on the new
    // rendition against a switch's tolerance, and on a loaded machine this
    // pipeline delivers 600ms gaps with nothing switching at all.
    let blank = FRAME_INTERVAL * 10;
    let across_gaps = gaps(&handover.across);
    assert!(
        longest(&across_gaps) <= blank,
        "the picture went blank for {}ms across the switch, more than the {}ms a handover should cost",
        longest(&across_gaps).as_millis(),
        blank.as_millis(),
    );

    // And the new rendition keeps delivering once it has taken over, to the same
    // bar the baseline had to clear. A switch that landed and then stalled is
    // not a switch that worked.
    assert!(
        handover.after.len() >= 8,
        "expected at least 8 frames in the {settle:?} after the switch landed, got {}",
        handover.after.len(),
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
/// Holds the cap across the whole switch, which the rate-limit test above cannot.
///
/// Ignored, and the reason is a real defect rather than a slow test. moq-net's
/// send-order queue is session-wide and keyed `(track_priority, group_sequence)`,
/// with ties broken by the higher sequence first. Both renditions subscribe at the
/// same priority, so a replacement starting at sequence 0 sorts below an incumbent
/// already at sequence 17, and its first group, the keyframe nothing can be drawn
/// without, waits behind every group the outgoing rendition still has queued. On a
/// saturated link that queue never empties. Run it with `--run-ignored all`.
///
/// The tiebreak is right within one track, where a newer group does matter more
/// than an older one, and meaningless across two, where the numbers count
/// different things. Fixing it means ranking by a group's age within its own
/// track rather than by its absolute sequence.
#[ignore = "the replacement track's first group is starved behind the incumbent's higher-numbered ones; passes about five runs in six"]
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

/// A round trip whose baseline rises and stays risen must leave the ladder
/// alone.
///
/// An iroh connection that loses its direct path and falls back to a relay goes
/// from a couple of milliseconds to tens of them and stays there, and a Wi-Fi to
/// cellular handoff does the same. Nothing about the new path is congested: it
/// carries every bit the publisher sends, at the rate it sent them before. Two
/// things can make the loop read that as a bottleneck anyway. A round trip
/// minimum that never forgets the path it was measured on calls the new one
/// permanently queueing, and a shortfall measured against a bitrate the encoder
/// undershoots by design is true whatever the link is doing. Together they step
/// the ladder down, probe back up on a loss counter that never saw anything, and
/// step down again for as long as the path stays where it moved to.
///
/// Watched for longer than the minimum's window, so a ladder that holds through
/// the first stale minutes and then gives way is still caught, and the minimum
/// itself is asserted at the end: one that never re-baselines is the half of the
/// failure a rendition assertion cannot see.
#[tokio::test]
#[traced_test]
async fn a_risen_baseline_round_trip_does_not_downgrade() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let track = fixture.video(2).await;
    let signals = fixture.subscription.signals().clone();

    assert_eq!(
        track.rendition(),
        "high",
        "a fresh subscription should start at the top of the ladder",
    );

    tokio::time::timeout(TIMEOUT, track.recv())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video track closed before its first frame");
    // Frames over a clear link first. Both baselines the risen round trip is
    // judged against are established here: the path's minimum, and the goodput
    // this rendition delivers when there is nothing in its way.
    let _settle = drain(&track, Duration::from_secs(3)).await;

    track.enable_adaptation_with(signals.clone(), quick_adaptation());
    let before = *signals.borrow();
    info!(
        rtt_ms = before.rtt.as_millis() as u64,
        min_rtt_ms = before.min_rtt.as_millis() as u64,
        goodput_kbps = ?before.goodput_bps.map(|bps| bps / 1000),
        "clear link",
    );

    // 30ms on each device's own egress, so the round trip gains 60ms: the step
    // a direct path takes when it becomes a relayed one. Nothing is dropped and
    // nothing is capped, so the top rung arrives exactly as it did before.
    fixture
        .impair(LinkLimits {
            latency_ms: 30,
            ..Default::default()
        })
        .await;

    let watched = Duration::from_secs(40);
    let until = Instant::now() + watched;
    let mut last = track.rendition();
    let mut switches = 0;
    while Instant::now() < until {
        let current = track.rendition();
        if current != last {
            info!(from = %last, to = %current, "rendition changed");
            switches += 1;
            last = current;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let after = *signals.borrow();
    info!(
        rtt_ms = after.rtt.as_millis() as u64,
        min_rtt_ms = after.min_rtt.as_millis() as u64,
        goodput_kbps = ?after.goodput_bps.map(|bps| bps / 1000),
        switches,
        "risen baseline watched",
    );

    assert_eq!(
        switches, 0,
        "the ladder moved {switches} time(s) over {watched:?} on a path that only got longer, \
         ending on `{last}`",
    );

    // A minimum still sitting on the old path's couple of milliseconds means
    // every later round trip reads as a queue. Well clear of both sides of the
    // question: the clear link above measures one to three milliseconds, and the
    // impaired one settles at 37 to 40 across the runs behind this figure.
    assert!(
        after.min_rtt >= Duration::from_millis(25),
        "the round trip minimum went from {}ms to {}ms across {watched:?} of an impairment that \
         added 60ms of round trip, so it never re-baselined onto the longer path",
        before.min_rtt.as_millis(),
        after.min_rtt.as_millis(),
    );

    fixture.shutdown().await;
}
