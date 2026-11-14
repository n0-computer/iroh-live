//! Tests the media pipeline and rendition adaptation over impaired links.
//!
//! A publisher and a subscriber run in separate network namespaces with a
//! router between them, and netem adds latency, jitter, loss or a rate limit.
//! The adaptation reads the real link signals, from QUIC path stats through
//! the connection monitor in `iroh-live-node/src/link.rs`.
//!
//! Linux only, built on unprivileged user namespaces. Run with
//! `cargo make test-patchbay`, which includes the ignored tests.

#![cfg(all(target_os = "linux", feature = "media"))]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use iroh::{Endpoint, endpoint::presets, protocol::Router};
use iroh_live::{
    Node, RateControl, Reach, SendEstimate, Subscription,
    layout::LiveLayout,
    media::{
        Adaptation, LocalBroadcast, Player, PlayerConfig, Rate, RemoteBroadcast, RenditionMode,
        RenditionState, VideoEncoding, VideoFrames, VideoRendition, VideoSource, remote_broadcast,
        video::{Frame, FrameRate, Size},
    },
    node::NetworkSample,
};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;
use patchbay::{Lab, LinkCondition, NodeId};
use tracing::info;

/// Sets up the user namespace the lab needs.
///
/// Unshare needs a single-threaded process. The test harness starts threads
/// before any test runs, so this runs from `.init_array`.
#[ctor::ctor(unsafe)]
fn patchbay_init() {
    // SAFETY: runs from `.init_array`, single-threaded, before `main`.
    unsafe { patchbay::init_userns_for_ctor() };
}

/// How long any one wait may take.
///
/// Generous: openh264 encodes in software, and every switch waits for a
/// keyframe over an impaired link.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Low enough for a debug-build software encoder to keep up.
///
/// A missing frame then means the transport lost it.
const FRAMERATE: u32 = 15;

/// The frame interval at [`FRAMERATE`], the unit of the gap thresholds.
const FRAME_INTERVAL: Duration = Duration::from_millis(1000 / FRAMERATE as u64);

/// Returns adaptation timers short enough for a switch each way within a test.
///
/// The thresholds stay at their defaults, because they are what the tests check.
fn quick() -> Adaptation {
    let mut adaptation = Adaptation::default();
    adaptation.downgrade_hold = Duration::from_millis(300);
    adaptation.upgrade_hold = Duration::from_millis(500);
    adaptation.post_downgrade_cooldown = Duration::from_secs(1);
    adaptation.tick = Duration::from_millis(100);
    adaptation
}

/// Distinct round trip readings a queue has to show before a test counts it.
const QUEUEING_SAMPLES: u32 = 2;

/// The largest gap between frames that still counts as smooth delivery.
///
/// Three frame intervals absorb a scheduling hiccup and one dropped frame, but
/// not a stall.
const SMOOTH: Duration = FRAME_INTERVAL.saturating_mul(3);

/// A publisher and a subscriber on either side of a router, each in its own namespace.
struct Fixture {
    lab: Lab,
    publisher_node: NodeId,
    subscriber_node: NodeId,
    router_node: NodeId,
    publisher: Node,
    /// The published broadcast. Dropping it stops publishing.
    local: LocalBroadcast,
    subscriber: Node,
    subscription: Subscription,
    broadcast: RemoteBroadcast,
}

impl Fixture {
    /// Builds the lab, publishes `renditions` of a gradient and subscribes to it.
    ///
    /// The publisher runs rate control only if `IROH_LIVE_PATCHBAY_RATE` names
    /// a send estimate, such as `app` or `controller`, so the whole suite can
    /// run with it.
    async fn start(size: Size, renditions: Vec<VideoRendition>) -> Self {
        let rate_control = std::env::var("IROH_LIVE_PATCHBAY_RATE").ok().map(|name| {
            let estimate: SendEstimate = name.parse().expect("a send estimate");
            RateControl::new(estimate)
        });
        Self::start_with(size, renditions, rate_control).await
    }

    /// Builds the lab as [`start`](Self::start) does, with the publisher's rate control as given.
    async fn start_with(
        size: Size,
        renditions: Vec<VideoRendition>,
        rate_control: Option<RateControl>,
    ) -> Self {
        info!(?rate_control, "starting the lab");
        let lab = Lab::new().await.expect("failed to build the lab");
        let router = lab
            .add_router("r1")
            .build()
            .await
            .expect("failed to build the router");
        let router_node = router.id();

        let publisher_device = lab
            .add_device("publisher")
            .iface("eth0", router_node)
            .build()
            .await
            .expect("failed to build the publisher device");
        let publisher_node = publisher_device.id();

        let subscriber_device = lab
            .add_device("subscriber")
            .iface("eth0", router_node)
            .build()
            .await
            .expect("failed to build the subscriber device");
        let subscriber_node = subscriber_device.id();

        // `spawn` runs the closure in the device's namespace, where the
        // endpoint has to bind.
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

        let mut config = LiveLayout::default().config();
        config.rate_control = rate_control;
        let publisher = Node::new(publisher_endpoint.clone(), config);
        let _router = publisher.mount(Router::builder(publisher_endpoint)).spawn();
        let broadcast: LocalBroadcast = LiveLayout::default()
            .publish(&publisher, "patchbay")
            .expect("failed to publish");
        broadcast
            .set_video(gradient(size), VideoEncoding::ladder(renditions))
            .expect("failed to set video");

        // No address lookup. The lab gives each device a fixed address.
        let publisher_addr = publisher.endpoint().addr();
        let subscriber = Node::new(subscriber_endpoint, LiveLayout::default().config());
        subscriber
            .connect(publisher_addr)
            .await
            .expect("failed to connect");
        let ticket = LiveLayout::default()
            .ticket(publisher.id(), "patchbay")
            .expect("a valid broadcast name");
        let subscription = subscriber
            .subscribe(ticket.path(), Reach::Direct(ticket.peer()))
            .await
            .expect("failed to subscribe");
        let remote = remote_broadcast(&subscription);

        Self {
            lab,
            publisher_node,
            subscriber_node,
            router_node,
            publisher,
            local: broadcast,
            subscriber,
            subscription,
            broadcast: remote,
        }
    }

    /// Returns the link serving the subscription.
    fn link(&self) -> Link {
        Link(self.subscription.clone())
    }

    /// Applies `condition` to both the publisher's and the subscriber's link.
    ///
    /// Impairing one leg only would let acknowledgements through at full speed,
    /// which no real path does.
    async fn impair(&self, condition: LinkCondition) {
        self.set_condition(Some(condition)).await;
        info!(?condition, "link impaired");
    }

    /// Removes all impairment from both links.
    async fn clear(&self) {
        self.set_condition(None).await;
        info!("link cleared");
    }

    /// Applies `condition` to the links of both nodes, or clears them for `None`.
    async fn set_condition(&self, condition: Option<LinkCondition>) {
        for node in [self.publisher_node, self.subscriber_node] {
            self.lab
                .set_link_condition(node, self.router_node, condition)
                .await
                .expect("failed to set the link condition");
        }
    }

    /// Plays the broadcast pinned to `rendition`.
    ///
    /// Waits for the catalog to list `renditions` video renditions first.
    async fn play(&self, renditions: usize, rendition: &str) -> Viewer {
        self.play_with(
            renditions,
            RenditionMode::pinned(rendition),
            Adaptation::default(),
        )
        .await
    }

    /// Plays the broadcast with adaptation at the default timers.
    ///
    /// For tests that check the ladder holds steady, which only means something
    /// at the timers users get. Waits for `renditions` video renditions first.
    async fn play_auto(&self, renditions: usize) -> Viewer {
        self.play_with(renditions, RenditionMode::Auto, Adaptation::default())
            .await
    }

    /// Plays the broadcast with adaptation at [`quick`] timers.
    async fn play_auto_quick(&self, renditions: usize) -> Viewer {
        self.play_with(renditions, RenditionMode::Auto, quick())
            .await
    }

    /// Plays the broadcast with `mode` and `adaptation`, once the catalog lists `renditions` renditions.
    async fn play_with(
        &self,
        renditions: usize,
        mode: RenditionMode,
        adaptation: Adaptation,
    ) -> Viewer {
        let broadcast = &self.broadcast;
        let mut catalog = broadcast.catalog();
        tokio::time::timeout(
            TIMEOUT,
            catalog.wait_for(|catalog| {
                catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.video.renditions.len() >= renditions)
            }),
        )
        .await
        .expect("timed out waiting for the video catalog")
        .expect("the broadcast is alive");

        let player = broadcast
            .play(
                PlayerConfig::default()
                    .with_rendition(mode)
                    .with_adaptation(adaptation),
            )
            .expect("failed to play");
        let frames = player.video_frames();
        Viewer { player, frames }
    }

    /// Shuts down both nodes.
    async fn shutdown(self) {
        self.publisher.shutdown().await;
        self.subscriber.shutdown().await;
    }
}

/// Returns a diagonal gradient at [`FRAMERATE`] that shifts every frame.
///
/// The bitrates quoted in this file were measured on this picture. A different
/// picture encodes to a different rate.
fn gradient(size: Size) -> VideoSource {
    let rate = FrameRate::new(FRAMERATE, 1).expect("a valid rate");
    VideoSource::test_gradient(size, rate).expect("the gradient thread starts")
}

/// The link serving a subscription, looked up on every read.
struct Link(Subscription);

impl Link {
    /// Returns the latest reading, or an empty one while no link serves.
    fn read(&self) -> NetworkSample {
        self.0
            .serving_link()
            .map(|serving| serving.sample)
            .unwrap_or_default()
    }
}

/// Reports whether `sample`'s round trip is over ten times its minimum.
///
/// That means a queue, not a longer path.
fn queued(sample: &NetworkSample) -> bool {
    matches!((sample.rtt, sample.min_rtt), (Some(rtt), Some(min)) if rtt > min * 10)
}

/// Returns a measured duration in milliseconds, zero while unmeasured.
fn millis(duration: Option<Duration>) -> u64 {
    duration.map_or(0, |duration| duration.as_millis() as u64)
}

/// A player and the frames a test reads from it.
struct Viewer {
    player: Player,
    frames: VideoFrames,
}

impl Viewer {
    /// Returns the rendition on screen, or an empty string before the first.
    fn rendition(&self) -> String {
        self.player
            .status()
            .borrow()
            .rendition
            .clone()
            .unwrap_or_default()
    }

    /// Waits for the next frame.
    async fn next(&mut self) -> Option<Arc<Frame>> {
        self.frames.next().await
    }
}

/// Waits until `rendition` is on screen.
async fn switched_to(player: &Player, rendition: &str) {
    player
        .status()
        .wait_for(|status| status.rendition.as_deref() == Some(rendition))
        .await
        .expect("the player is alive");
}

/// Waits until the player is switching to `rendition`, or has.
async fn requested(player: &Player, rendition: &str) {
    player
        .status()
        .wait_for(|status| {
            status.switching_to.as_deref() == Some(rendition)
                || status.rendition.as_deref() == Some(rendition)
        })
        .await
        .expect("the player is alive");
}

/// Returns half the frames [`FRAMERATE`] delivers in `window`.
///
/// A test that counts switches asserts this too, since a stalled player does
/// not switch either.
fn half_rate(window: Duration) -> u32 {
    (window.as_secs_f64() * f64::from(FRAMERATE) / 2.0) as u32
}

/// Reads frames for `duration` and returns when each one arrived.
///
/// Awaits every frame rather than polling. The frame slot keeps only the
/// newest frame, so polling would measure the poll cadence.
async fn drain(frames: &mut VideoFrames, duration: Duration) -> Vec<Instant> {
    let deadline = Instant::now() + duration;
    let mut arrivals = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match tokio::time::timeout(remaining, frames.next()).await {
            Ok(Some(_frame)) => arrivals.push(Instant::now()),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    arrivals
}

/// Frame arrivals during a rendition handover and after it.
///
/// Only a gap during the handover is a cost of the switch. A gap after it is
/// ordinary jitter on the new rendition, which `frames_survive_a_*` covers.
struct Handover {
    /// Arrivals from the switch request to the replacement's first frame.
    across: Vec<Instant>,
    /// Arrivals in the settling window that followed.
    after: Vec<Instant>,
    /// How long the replacement took to open, subscribe and produce a frame.
    took: Duration,
}

/// Reads frames across a switch to `rendition`, and for `settle` after it lands.
///
/// The window ends when the switch lands. No fixed window fits both a slow
/// handover and a fast one.
async fn drain_across_switch(viewer: &mut Viewer, rendition: &str, settle: Duration) -> Handover {
    let asked = Instant::now();
    let mut across = Vec::new();
    {
        let switched = switched_to(&viewer.player, rendition);
        tokio::pin!(switched);
        loop {
            tokio::select! {
                frame = viewer.frames.next() => match frame {
                    Some(_frame) => across.push(Instant::now()),
                    None => return Handover { across, after: Vec::new(), took: asked.elapsed() },
                },
                () = &mut switched => break,
            }
        }
    }
    let took = asked.elapsed();
    let mut after = drain(&mut viewer.frames, settle).await;
    // The replacement's first frame is what lands the switch, so it is read
    // after. It closes the gap from the incumbent's last frame, so it counts
    // as part of the handover.
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

/// Logs a phase's frame count and gaps, so a failure shows the numbers.
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

/// Returns a rendition named `name` at `size`, or the source's, and `bitrate`.
fn rendition(name: &str, size: Option<Size>, bitrate: Rate) -> VideoRendition {
    let mut rendition = VideoRendition::new(name);
    rendition.size = size;
    rendition.config.bitrate = Some(bitrate);
    rendition
}

/// Returns a two-rung ladder, `high` at 800 kbit/s and `low` at 200 kbit/s.
///
/// The bitrates are encoder ceilings. Over a clear link this picture arrives at
/// about 316 kbit/s on `high` and 84 on `low`, some 40% of the ceilings, so an
/// impairment has to be tighter than the ladder suggests.
fn ladder() -> Vec<VideoRendition> {
    vec![
        rendition("high", None, Rate::from_bps(800_000)),
        rendition("low", Some(Size::new(320, 240)), Rate::from_bps(200_000)),
    ]
}

/// The longest a change on the link may take to reach the signals.
///
/// The throughput window is two seconds and follows a cap within a few seconds,
/// so this leaves room for a loaded machine. A slower signal is a failure.
const SIGNAL_LAG: Duration = Duration::from_secs(15);

/// Extra time on top of [`SIGNAL_LAG`] for [`QUEUEING_SAMPLES`] round trips.
///
/// QUIC takes a round trip sample only from an ack-eliciting packet. A
/// subscriber mostly sends acknowledgements, so fresh readings arrive seconds
/// apart.
const RTT_CORROBORATION: Duration = Duration::from_secs(30);

/// Frames survive added latency, and delivery recovers once it clears.
#[tokio::test]
#[traced_test]
async fn frames_survive_a_latency_ramp() {
    let fixture = Fixture::start(
        Size::new(320, 240),
        vec![rendition("video", None, Rate::from_bps(500_000))],
    )
    .await;
    let mut viewer = fixture.play(1, "video").await;

    // Skips startup, which takes the first couple of seconds.
    let warmup = drain(&mut viewer.frames, Duration::from_secs(2)).await;
    info!(frames = warmup.len(), "warmed up");

    let window = Duration::from_secs(3);
    let baseline = drain(&mut viewer.frames, window).await;
    report("baseline", &baseline, window);
    assert!(
        baseline.len() >= 10,
        "expected at least 10 frames in {window:?} at {FRAMERATE}fps before impairment, got {}",
        baseline.len(),
    );

    fixture
        .impair(LinkCondition::new().latency_ms(300).jitter_ms(60))
        .await;
    let ramp = drain(&mut viewer.frames, Duration::from_secs(5)).await;
    report("latency 300ms", &ramp, Duration::from_secs(5));
    // Loose on purpose. The check is that frames still come, not that they
    // come on time.
    assert!(
        ramp.len() >= 15,
        "expected at least 15 frames across 5s at 300ms latency, got {} (stalled?)",
        ramp.len(),
    );

    fixture.clear().await;
    // Packets in flight still carry the old delay.
    let settle = drain(&mut viewer.frames, Duration::from_secs(3)).await;
    info!(frames = settle.len(), "settled");

    let recovery = drain(&mut viewer.frames, window).await;
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

/// Frames survive 20% loss, and delivery recovers once it clears.
#[tokio::test]
#[traced_test]
async fn frames_survive_a_loss_spike() {
    let fixture = Fixture::start(
        Size::new(320, 240),
        vec![rendition("video", None, Rate::from_bps(500_000))],
    )
    .await;
    let mut viewer = fixture.play(1, "video").await;

    let _warmup = drain(&mut viewer.frames, Duration::from_secs(2)).await;
    let window = Duration::from_secs(2);
    let baseline = drain(&mut viewer.frames, window).await;
    report("baseline", &baseline, window);
    assert!(
        baseline.len() >= 8,
        "expected at least 8 frames in {window:?} before impairment, got {}",
        baseline.len(),
    );

    fixture.impair(LinkCondition::new().random_loss(20.0)).await;
    let lossy = drain(&mut viewer.frames, Duration::from_secs(3)).await;
    report("20% loss", &lossy, Duration::from_secs(3));
    assert!(
        lossy.len() >= 10,
        "expected at least 10 frames across 3s at 20% loss, got {} (stalled?)",
        lossy.len(),
    );

    fixture.clear().await;
    let _settle = drain(&mut viewer.frames, Duration::from_secs(3)).await;

    let window = Duration::from_secs(3);
    let recovery = drain(&mut viewer.frames, window).await;
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

/// Real packet loss steps the ladder down, and clearing it steps back up.
///
/// The loss reaches the adaptation through QUIC loss detection and the
/// connection monitor. `e2e::heavy_loss_moves_the_player_down_the_ladder` feeds made-up
/// signals instead.
#[tokio::test]
#[traced_test]
async fn adaptation_follows_a_real_link() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    // A clear link has to settle on the top rung by itself first.
    let mut viewer = fixture.play_auto_quick(2).await;

    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high"))
        .await
        .expect("a clear link should start at the top of the ladder");

    // Impairs only once the link carries video.
    tokio::time::timeout(TIMEOUT, viewer.next())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video ended before its first frame");

    // `NetworkSample::loss` counts the packets this endpoint sends, mostly
    // acknowledgements, so the loss has to reach the subscriber's side too.
    // 12% on each of two legs loses about 23% end to end, past the emergency
    // threshold, and still carries the replacement's keyframe.
    fixture.impair(LinkCondition::new().random_loss(12.0)).await;

    let downgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "low"))
        .await
        .expect("timed out waiting for a downgrade to `low`");
    info!(
        after_ms = downgraded.elapsed().as_millis() as u64,
        "downgraded"
    );

    fixture.clear().await;

    let upgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high"))
        .await
        .expect("timed out waiting for an upgrade back to `high`");
    info!(after_ms = upgraded.elapsed().as_millis() as u64, "upgraded");

    fixture.shutdown().await;
}

/// A rate limit with no loss steps the ladder down, and lifting it steps back up.
///
/// No packets are dropped. The cap shows as throughput pinned to it and a round
/// trip inflated by the queue. The test checks both in the signals before it
/// waits for the downgrade.
#[tokio::test]
#[traced_test]
async fn adaptation_follows_a_rate_limit() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let mut viewer = fixture.play_auto_quick(2).await;
    let signals = fixture.link();

    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high"))
        .await
        .expect("a clear link should start at the top of the ladder");

    // Frames first, so the signals have a healthy baseline before the cap.
    tokio::time::timeout(TIMEOUT, viewer.next())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video ended before its first frame");

    // The clear link has to carry well over the cap first, or the cap is no
    // shortfall. Waited for, because the encoder takes a few seconds to reach
    // its rate: this picture at 640x480 sits near 200 kbit/s between
    // keyframes and near 300 across one.
    let cap_kbit = 100;
    let clear_enough = u64::from(cap_kbit) * 1000 * 13 / 10;
    tokio::time::timeout(SIGNAL_LAG, async {
        loop {
            if signals
                .read()
                .throughput
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
            "the clear link never carried {clear_enough} bps, so a {cap_kbit} kbit/s cap is not \
             a shortfall and this test has nothing to measure",
        )
    });

    // Half of what `high` sends at its quietest, and above what `low` sends.
    fixture
        .impair(LinkCondition::new().rate_kbit(cap_kbit))
        .await;

    // Waits until the cap has shown in throughput and round trip for three
    // downgrade holds, backed by more than `QUEUEING_SAMPLES` distinct round
    // trip readings. The adaptation itself acts on the delivery estimate.
    let held = Adaptation::default().downgrade_hold * 3;
    let mut worst_loss: f64 = 0.0;
    let impaired = Instant::now();
    let (saw_the_cap, readings) = tokio::time::timeout(SIGNAL_LAG + RTT_CORROBORATION, async {
        let mut since = None;
        // Distinct round trip readings while the cap has been visible without
        // a break. One stale reading repeated for `held` proves nothing.
        let mut readings = 0u32;
        let mut last_sample = None;
        loop {
            let signals = signals.read();
            worst_loss = worst_loss.max(signals.loss.unwrap_or(0.0));
            let pinned = signals.throughput.is_some_and(|bps| bps < 250_000);
            if pinned && queued(&signals) {
                let start = *since.get_or_insert_with(Instant::now);
                if last_sample != Some(signals.rtt) {
                    last_sample = Some(signals.rtt);
                    readings += 1;
                }
                if Instant::now().duration_since(start) >= held && readings > QUEUEING_SAMPLES {
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
            QUEUEING_SAMPLES + 1,
            SIGNAL_LAG + RTT_CORROBORATION,
        )
    });
    info!(
        after_ms = impaired.elapsed().as_millis() as u64,
        throughput_kbps = saw_the_cap.throughput.unwrap_or(0) / 1000,
        rtt_ms = millis(saw_the_cap.rtt),
        min_rtt_ms = millis(saw_the_cap.min_rtt),
        rtt_readings = readings,
        worst_loss,
        "the rate limit reached the signals",
    );

    // Loss must not explain the downgrade. Checked against the step-down
    // threshold, not zero, since some loss can always happen.
    assert!(
        worst_loss < Adaptation::default().loss_step_down,
        "loss reached {worst_loss}, so the downgrade cannot be credited to the bandwidth signal",
    );

    // Waits for the decision under the cap that caused it.
    tokio::time::timeout(TIMEOUT, requested(&viewer.player, "low"))
        .await
        .expect("timed out waiting for the loop to ask for `low`");

    // Lifts the cap before the switch lands. A switch under a held cap is
    // `a_switch_lands_while_the_link_stays_capped`'s case.
    fixture.clear().await;

    let downgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "low"))
        .await
        .expect("timed out waiting for a downgrade to `low`");
    info!(
        after_ms = downgraded.elapsed().as_millis() as u64,
        "downgraded"
    );

    let upgraded = Instant::now();
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high"))
        .await
        .expect("timed out waiting for an upgrade back to `high`");
    info!(after_ms = upgraded.elapsed().as_millis() as u64, "upgraded");

    fixture.shutdown().await;
}

/// A pinned rendition switch keeps frames coming while the replacement opens.
///
/// The incumbent plays until the replacement catches up. Without that overlap
/// the gap would be a decoder open plus a keyframe, seconds rather than frames.
#[tokio::test]
#[traced_test]
async fn a_switch_does_not_blank_the_picture() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let mut viewer = fixture.play(2, "high").await;

    // Real delay on the link, but not enough to disturb the baseline cadence.
    fixture
        .impair(LinkCondition::new().latency_ms(50).jitter_ms(10))
        .await;

    let _warmup = drain(&mut viewer.frames, Duration::from_secs(3)).await;
    let window = Duration::from_secs(2);
    let baseline = drain(&mut viewer.frames, window).await;
    report("baseline", &baseline, window);
    assert!(
        baseline.len() >= 8,
        "expected at least 8 frames in {window:?} before the switch, got {}",
        baseline.len(),
    );

    viewer.player.set_rendition(RenditionMode::pinned("low"));

    let settle = Duration::from_secs(2);
    let handover = tokio::time::timeout(TIMEOUT, drain_across_switch(&mut viewer, "low", settle))
        .await
        .unwrap_or_else(|_| panic!("the switch to `low` did not land inside {TIMEOUT:?}"));
    report("across the switch", &handover.across, handover.took);
    report("after the switch", &handover.after, settle);
    let kept_running =
        baseline.len() as u32 * handover.took.as_millis() as u32 / (2 * window.as_millis() as u32);
    // Every figure the assertions use, in one line for scripts that compare runs.
    info!(
        took_ms = handover.took.as_millis() as u64,
        across = handover.across.len(),
        baseline = baseline.len(),
        kept_running,
        longest_gap_ms = longest(&gaps(&handover.across)).as_millis() as u64,
        after = handover.after.len(),
        "handover measured",
    );

    // At least half the baseline rate during the handover. Half, because both
    // renditions share the link while the switch is in flight.
    assert!(
        handover.across.len() as u32 >= kept_running,
        "only {} frames arrived in the {}ms the switch took, against the {kept_running} that half \
         the baseline's {} frames per {window:?} comes to, so the incumbent stopped delivering \
         while the replacement opened",
        handover.across.len(),
        handover.took.as_millis(),
        baseline.len(),
    );

    // Ten frame intervals, far less than a decoder open and a keyframe. Only
    // for the handover window: on a loaded machine the settled stream shows
    // 600ms gaps with no switch at all.
    let blank = FRAME_INTERVAL * 10;
    let across_gaps = gaps(&handover.across);
    assert!(
        longest(&across_gaps) <= blank,
        "the picture went blank for {}ms across the switch, more than the {}ms a handover should cost",
        longest(&across_gaps).as_millis(),
        blank.as_millis(),
    );

    // The new rendition keeps delivering, to the baseline's bar.
    assert!(
        handover.after.len() >= 8,
        "expected at least 8 frames in the {settle:?} after the switch landed, got {}",
        handover.after.len(),
    );

    fixture.shutdown().await;
}

/// A pinned switch lands while the link stays capped.
///
/// Runs without rate control whatever `IROH_LIVE_PATCHBAY_RATE` says. Its
/// premise is a link that stays saturated, and an encoder that follows the
/// cap drains the queue instead.
///
/// Ignored because the replacement's groups starve. The iroh transport has no
/// send groups, so moq-net ranks every group stream of the session in one
/// queue, and between two subscriptions at the same priority the older one
/// goes first. The replacement's first group, its keyframe, waits behind every
/// group the incumbent still has queued, and on a saturated link that queue
/// never empties. Send groups in `moq-transport-iroh`, which share bytes
/// between subscriptions of equal priority, or a replacement subscribed above
/// the incumbent's priority until it takes over, would let the switch land.
#[tokio::test]
#[traced_test]
#[ignore = "the replacement track's first group is starved behind the incumbent's groups. It passed 1 run in 6 at a commit from 2026-11-12 and none of 18 at three later ones"]
async fn a_switch_lands_while_the_link_stays_capped() {
    let fixture = Fixture::start_with(Size::new(640, 480), ladder(), None).await;
    let mut viewer = fixture.play(2, "high").await;
    let signals = fixture.link();

    tokio::time::timeout(TIMEOUT, viewer.next())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video ended before its first frame");
    let _settle = drain(&mut viewer.frames, Duration::from_secs(2)).await;

    // Two thirds of what `high` sends, so the send queue never drains.
    fixture.impair(LinkCondition::new().rate_kbit(200)).await;

    // Waits until the signals show the link saturated.
    tokio::time::timeout(SIGNAL_LAG, async {
        loop {
            let signals = signals.read();
            if signals.throughput.is_some_and(|bps| bps < 250_000) && queued(&signals) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the signals did not show the rate limit inside {SIGNAL_LAG:?}"));
    info!("the link is saturated");

    let asked = Instant::now();
    viewer.player.set_rendition(RenditionMode::pinned("low"));
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "low"))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the switch to `low` did not land inside {TIMEOUT:?} while the cap was held, \
                 still on `{}`",
                viewer.rendition(),
            )
        });
    info!(after_ms = asked.elapsed().as_millis() as u64, "switched");

    fixture.shutdown().await;
}

/// A cap held for a minute does not make the ladder oscillate.
///
/// Counts landed switches and requested ones. The cap defaults to 300 kbit/s.
/// Set `IROH_LIVE_PATCHBAY_CAP_KBIT` to sweep it.
#[tokio::test]
#[traced_test]
async fn adaptation_holds_steady_under_a_marginal_cap() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let mut viewer = fixture.play_auto(2).await;
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high"))
        .await
        .expect("a clear link should start at the top of the ladder");
    tokio::time::timeout(TIMEOUT, viewer.next())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video ended before its first frame");
    let _settle = drain(&mut viewer.frames, Duration::from_secs(3)).await;

    let cap_kbit: u32 = std::env::var("IROH_LIVE_PATCHBAY_CAP_KBIT")
        .ok()
        .and_then(|cap| cap.parse().ok())
        .unwrap_or(300);
    fixture
        .impair(LinkCondition::new().rate_kbit(cap_kbit))
        .await;

    let watched = Duration::from_secs(60);
    let until = Instant::now() + watched;
    let mut status = viewer.player.status();
    let mut last = status.borrow_and_update().clone();
    let (mut switches, mut requests) = (0u32, 0u32);
    let mut frames = 0u32;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        tokio::select! {
            changed = status.changed() => {
                changed.expect("the player is alive");
                let current = status.borrow_and_update().clone();
                if current.rendition != last.rendition && current.rendition.is_some() {
                    info!(from = ?last.rendition, to = ?current.rendition, "rendition changed");
                    switches += 1;
                }
                if current.switching_to != last.switching_to && current.switching_to.is_some() {
                    info!(to = ?current.switching_to, "switch asked for");
                    requests += 1;
                }
                last = current;
            }
            frame = viewer.frames.next() => {
                if frame.is_some() {
                    frames += 1;
                }
            }
            () = tokio::time::sleep(left) => break,
        }
    }
    info!(
        cap_kbit,
        switches,
        requests,
        frames,
        rendition = ?last.rendition,
        "marginal cap watched",
    );
    // One step down, plus at most one probe up and back down.
    assert!(
        switches <= 3,
        "the ladder switched {switches} times ({requests} asked for) in {watched:?} under a \
         {cap_kbit} kbit/s cap",
    );
    // A player that stalls never switches either.
    assert!(
        frames >= half_rate(watched),
        "only {frames} frames arrived in {watched:?} under a {cap_kbit} kbit/s cap",
    );

    fixture.shutdown().await;
}

/// A round trip that rises and stays up leaves the ladder alone.
///
/// Falling back from a direct path to a relay looks like this: a longer path,
/// not a congested one. The test watches for longer than the round trip
/// minimum's window, and checks that the minimum moves to the longer path.
#[tokio::test]
#[traced_test]
async fn a_risen_baseline_round_trip_does_not_downgrade() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
    let mut viewer = fixture.play_auto(2).await;
    let signals = fixture.link();

    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high"))
        .await
        .expect("a clear link should start at the top of the ladder");

    tokio::time::timeout(TIMEOUT, viewer.next())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video ended before its first frame");
    // Frames over a clear link first, to set the round trip minimum and the
    // throughput baseline.
    let _settle = drain(&mut viewer.frames, Duration::from_secs(3)).await;

    let before = signals.read();
    info!(
        rtt_ms = millis(before.rtt),
        min_rtt_ms = millis(before.min_rtt),
        throughput_kbps = ?before.throughput.map(|bps| bps / 1000),
        "clear link",
    );

    // 30ms on each device's egress adds 60ms of round trip, with no loss and
    // no cap.
    fixture.impair(LinkCondition::new().latency_ms(30)).await;

    let watched = Duration::from_secs(40);
    let until = Instant::now() + watched;
    let mut last = viewer.rendition();
    let (mut switches, mut frames) = (0, 0u32);
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    while Instant::now() < until {
        tokio::select! {
            Some(_) = viewer.frames.next() => frames += 1,
            _ = poll.tick() => {
                let current = viewer.rendition();
                if current != last {
                    info!(from = %last, to = %current, "rendition changed");
                    switches += 1;
                    last = current;
                }
            }
        }
    }

    let after = signals.read();
    info!(
        rtt_ms = millis(after.rtt),
        min_rtt_ms = millis(after.min_rtt),
        throughput_kbps = ?after.throughput.map(|bps| bps / 1000),
        switches,
        frames,
        "risen baseline watched",
    );

    assert_eq!(
        switches, 0,
        "the ladder moved {switches} time(s) over {watched:?} on a path that only got longer, \
         ending on `{last}`",
    );
    assert!(
        frames >= half_rate(watched),
        "only {frames} frames arrived in {watched:?} on a path that only got longer",
    );

    // A minimum stuck on the old path makes every later round trip read as a
    // queue. Measured: 1 to 3ms on the clear link, 37 to 40ms once impaired.
    assert!(
        after
            .min_rtt
            .is_some_and(|min| min >= Duration::from_millis(25)),
        "the round trip minimum went from {}ms to {}ms across {watched:?} of an impairment that \
         added 60ms of round trip, so it never re-baselined onto the longer path",
        millis(before.min_rtt),
        millis(after.min_rtt),
    );

    fixture.shutdown().await;
}

/// Returns the publisher's rate control, with the estimate `IROH_LIVE_PATCHBAY_RATE` names if set.
fn rate_control() -> RateControl {
    match std::env::var("IROH_LIVE_PATCHBAY_RATE") {
        Ok(name) => RateControl::new(name.parse().expect("a send estimate")),
        Err(_) => RateControl::default(),
    }
}

/// One reading of the publisher's rate control and of what reaches the subscriber.
#[derive(Debug, Clone)]
struct RateRow {
    at: Duration,
    phase: &'static str,
    rendition: String,
    state: String,
    target_kbps: Option<u64>,
    grant_kbps: Option<u64>,
    encoded_kbps: Option<u64>,
    delivery_kbps: Option<u64>,
    throughput_kbps: Option<u64>,
    rtt_ms: u64,
    /// The publisher's own smoothed round trip, which its estimator reads.
    publisher_rtt_ms: u64,
}

/// Records [`RateRow`]s of one rendition every half second, while it lives.
struct RateTrace {
    rows: Arc<std::sync::Mutex<Vec<RateRow>>>,
    phase: Arc<std::sync::Mutex<&'static str>>,
    _task: tokio::task::JoinHandle<()>,
}

impl RateTrace {
    /// Starts recording rendition `rendition` of `fixture`'s broadcast.
    fn start(fixture: &Fixture, rendition: &str) -> Self {
        let rows = Arc::new(std::sync::Mutex::new(Vec::new()));
        let phase = Arc::new(std::sync::Mutex::new("clear"));
        let (local, link, name) = (fixture.local.clone(), fixture.link(), rendition.to_string());
        let publisher = fixture.publisher.clone();
        let task = tokio::spawn({
            let (rows, phase) = (rows.clone(), phase.clone());
            async move {
                let start = Instant::now();
                let kbps = |rate: Option<Rate>| rate.map(|rate| rate.as_bps() / 1000);
                loop {
                    let stats = local
                        .stats()
                        .renditions
                        .get(&name)
                        .cloned()
                        .unwrap_or_default();
                    let state = local.status().borrow().renditions.get(&name).cloned();
                    let signals = link.read();
                    let publisher_rtt = publisher.sessions().get().first().and_then(|session| {
                        let paths = session.connection().paths();
                        let selected = paths.iter().find(|path| path.is_selected());
                        selected.map(|path| path.rtt())
                    });
                    rows.lock().expect("poisoned").push(RateRow {
                        at: start.elapsed(),
                        phase: *phase.lock().expect("poisoned"),
                        rendition: name.clone(),
                        state: match state {
                            Some(RenditionState::Encoding { .. }) => "encoding".into(),
                            Some(other) => format!("{other:?}").to_lowercase(),
                            None => "-".into(),
                        },
                        target_kbps: kbps(stats.target),
                        grant_kbps: kbps(stats.grant),
                        encoded_kbps: kbps(stats.bitrate),
                        delivery_kbps: signals.delivery.map(|bps| bps / 1000),
                        throughput_kbps: signals.throughput.map(|bps| bps / 1000),
                        rtt_ms: millis(signals.rtt),
                        publisher_rtt_ms: millis(publisher_rtt),
                    });
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        });
        Self {
            rows,
            phase,
            _task: task,
        }
    }

    /// Marks the readings from now on as `phase`.
    fn phase(&self, phase: &'static str) {
        *self.phase.lock().expect("poisoned") = phase;
    }

    /// Prints the readings as a table, so a run shows the course of the bitrate.
    fn print(&self, title: &str) {
        println!("rate trace: {title}");
        println!(
            "{:>6} {:<8} {:<6} {:<9} {:>7} {:>7} {:>8} {:>9} {:>8} {:>6} {:>6}",
            "t_s",
            "phase",
            "track",
            "state",
            "target",
            "grant",
            "encoded",
            "delivery",
            "through",
            "rtt_ms",
            "p_rtt"
        );
        let show =
            |kbps: Option<u64>| kbps.map_or_else(|| "-".to_string(), |kbps| kbps.to_string());
        for row in self.rows.lock().expect("poisoned").iter() {
            println!(
                "{:>6.1} {:<8} {:<6} {:<9} {:>7} {:>7} {:>8} {:>9} {:>8} {:>6} {:>6}",
                row.at.as_secs_f64(),
                row.phase,
                row.rendition,
                row.state,
                show(row.target_kbps),
                show(row.grant_kbps),
                show(row.encoded_kbps),
                show(row.delivery_kbps),
                show(row.throughput_kbps),
                row.rtt_ms,
                row.publisher_rtt_ms,
            );
        }
    }
}

/// Waits up to `limit` until `check` holds for `fixture`'s rendition `name`, and returns how long it took.
async fn rate_reaches(
    fixture: &Fixture,
    name: &str,
    limit: Duration,
    check: impl Fn(&iroh_live::media::EncodeStats, Option<&RenditionState>) -> bool,
) -> Option<Duration> {
    let start = Instant::now();
    tokio::time::timeout(limit, async {
        loop {
            let stats = fixture.local.stats().renditions.get(name).cloned();
            let state = fixture
                .local
                .status()
                .borrow()
                .renditions
                .get(name)
                .cloned();
            if stats.is_some_and(|stats| check(&stats, state.as_ref())) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .ok()
    .map(|()| start.elapsed())
}

/// The cap the rate control scenarios apply, under what the picture encodes to.
const RATE_CAP_KBIT: u32 = 250;

/// The ceiling of the one rendition the cap scenarios publish.
const RATE_CEILING: Rate = Rate::from_kbps(1_500);

/// Starts a publisher with rate control and one rendition at [`RATE_CEILING`], and plays it.
///
/// Returns once frames flowed over the clear link for a few seconds and the
/// encoder sits at its ceiling.
async fn capped_rendition() -> (Fixture, Viewer, RateTrace) {
    let fixture = Fixture::start_with(
        Size::new(640, 480),
        vec![rendition("high", None, RATE_CEILING)],
        Some(rate_control()),
    )
    .await;
    let mut viewer = fixture.play(1, "high").await;
    tokio::time::timeout(TIMEOUT, viewer.next())
        .await
        .expect("timed out waiting for the first frame")
        .expect("the video ended before its first frame");
    let trace = RateTrace::start(&fixture, "high");
    let _settle = drain(&mut viewer.frames, Duration::from_secs(5)).await;
    assert_eq!(
        fixture.local.stats().renditions["high"].target,
        Some(RATE_CEILING),
        "a clear link keeps the encoder at its ceiling"
    );
    (fixture, viewer, trace)
}

/// Caps the link and waits until the encoder's target is under 1.1 times the cap.
///
/// Returns how long that took after the cap went on.
async fn cap_and_follow(fixture: &Fixture, trace: &RateTrace) -> Duration {
    trace.phase("capped");
    fixture
        .impair(LinkCondition::new().rate_kbit(RATE_CAP_KBIT))
        .await;
    let under = u64::from(RATE_CAP_KBIT) * 1000 * 11 / 10;
    let followed = rate_reaches(fixture, "high", Duration::from_secs(20), |stats, _| {
        stats.target.is_some_and(|target| target.as_bps() <= under)
    })
    .await;
    match followed {
        Some(after) => {
            info!(
                after_ms = after.as_millis() as u64,
                "the encoder followed the cap"
            );
            after
        }
        None => {
            trace.print("cap not followed");
            panic!(
                "the encoder's target stayed above {under} bps under a {RATE_CAP_KBIT} kbit/s cap"
            );
        }
    }
}

/// A capped link lowers the encoder's bitrate, and frames keep coming at the lower rate.
///
/// The target falls under 1.1 times the cap. After the queue the drop built
/// has drained, frames arrive at more than half the frame rate, with no gap
/// over two seconds.
#[tokio::test]
#[traced_test]
async fn the_encoder_follows_a_cap_without_stalling() {
    let (fixture, mut viewer, trace) = capped_rendition().await;
    let followed = cap_and_follow(&fixture, &trace).await;
    // The queue from before the drop drains first.
    let _drain = drain(&mut viewer.frames, Duration::from_secs(5)).await;
    let window = Duration::from_secs(15);
    let arrivals = drain(&mut viewer.frames, window).await;
    report("capped", &arrivals, window);
    trace.print("cap drop");
    let gaps = gaps(&arrivals);
    assert!(
        arrivals.len() as u32 >= half_rate(window),
        "only {} frames in {window:?} under the cap, {followed:?} after the encoder followed it",
        arrivals.len()
    );
    assert!(
        longest(&gaps) < Duration::from_secs(2),
        "a {:?} gap under the cap",
        longest(&gaps)
    );
    fixture.shutdown().await;
}

/// Lifting the cap lets the encoder climb back to its ceiling.
///
/// Without padding probes the estimate climbs by 10% a second once the path
/// stays clean, from 0.85 times what the capped link delivered.
#[tokio::test]
#[traced_test]
async fn the_encoder_recovers_when_the_cap_lifts() {
    let (fixture, mut viewer, trace) = capped_rendition().await;
    cap_and_follow(&fixture, &trace).await;
    let _capped = drain(&mut viewer.frames, Duration::from_secs(5)).await;
    trace.phase("lifted");
    fixture.clear().await;
    let recovered = rate_reaches(&fixture, "high", Duration::from_secs(60), |stats, _| {
        stats.target == Some(RATE_CEILING)
    })
    .await;
    trace.print("cap lifted");
    let recovered = recovered.expect("the encoder did not climb back to its ceiling in 60 s");
    info!(
        after_ms = recovered.as_millis() as u64,
        "the encoder recovered"
    );
    let window = Duration::from_secs(5);
    let arrivals = drain(&mut viewer.frames, window).await;
    report("recovered", &arrivals, window);
    assert!(arrivals.len() as u32 >= half_rate(window));
    fixture.shutdown().await;
}

/// A cap under a rung's band boundary pauses the rung, and lifting it resumes the rung.
///
/// `high` reserves 1.2 Mbit/s above `low` at 200 kbit/s, so it pauses below
/// 533 kbit/s. The player follows the catalog to `low` while `high` is paused.
#[tokio::test]
#[traced_test]
async fn a_paused_rendition_resumes_when_the_cap_lifts() {
    let renditions = vec![
        rendition("high", None, Rate::from_kbps(1_200)),
        rendition("low", Some(Size::new(320, 240)), Rate::from_kbps(200)),
    ];
    let fixture = Fixture::start_with(Size::new(640, 480), renditions, Some(rate_control())).await;
    let mut viewer = fixture.play_auto_quick(2).await;
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high"))
        .await
        .expect("a clear link should start at the top of the ladder");
    let trace = RateTrace::start(&fixture, "high");
    let _settle = drain(&mut viewer.frames, Duration::from_secs(5)).await;

    trace.phase("capped");
    fixture
        .impair(LinkCondition::new().rate_kbit(RATE_CAP_KBIT))
        .await;
    let paused = rate_reaches(&fixture, "high", Duration::from_secs(30), |_, state| {
        state == Some(&RenditionState::Paused)
    })
    .await;
    let Some(paused) = paused else {
        trace.print("never paused");
        panic!("`high` did not pause under a {RATE_CAP_KBIT} kbit/s cap");
    };
    info!(after_ms = paused.as_millis() as u64, "`high` paused");
    tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "low"))
        .await
        .expect("the player did not leave the paused rendition");
    let window = Duration::from_secs(5);
    let arrivals = drain(&mut viewer.frames, window).await;
    report("paused", &arrivals, window);

    trace.phase("lifted");
    fixture.clear().await;
    let resumed = rate_reaches(&fixture, "high", Duration::from_secs(60), |_, state| {
        state != Some(&RenditionState::Paused)
    })
    .await;
    let Some(resumed) = resumed else {
        trace.print("never resumed");
        panic!("`high` did not resume in 60 s after the cap lifted");
    };
    info!(after_ms = resumed.as_millis() as u64, "`high` resumed");
    let upgraded = tokio::time::timeout(TIMEOUT, switched_to(&viewer.player, "high")).await;
    trace.print("pause and resume");
    upgraded.expect("the player did not return to the resumed rendition");
    fixture.shutdown().await;
}
