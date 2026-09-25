//! Tests the media pipeline and rendition adaptation over impaired links.
//!
//! A publisher and a subscriber run in separate network namespaces with a
//! router between them, and netem adds latency, jitter, loss or a rate limit.
//! The adaptation reads the real link signals, from QUIC path stats through
//! the connection monitor in `iroh-moq/src/link.rs`.
//!
//! Linux only, built on unprivileged user namespaces. Run with
//! `cargo make test-patchbay`, which includes the ignored tests.

#![cfg(all(target_os = "linux", feature = "media"))]

use std::time::{Duration, Instant};

use iroh::{Endpoint, endpoint::presets};
use iroh_live::{BroadcastTicket, Live, Reach, RemoteBroadcast, Subscription, moq::LinkSample};
use iroh_live_media::{
    Adaptation, Bitrate, LocalBroadcast, Player, PlayerConfig, RenditionMode, VideoEncoding,
    VideoFormat, VideoFrames, VideoRendition, VideoSource,
    video::{Frame, Rate, Size, Surface},
};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher as _;
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
    Adaptation {
        downgrade_hold: Duration::from_millis(300),
        upgrade_hold: Duration::from_millis(500),
        post_downgrade_cooldown: Duration::from_secs(1),
        tick: Duration::from_millis(100),
        ..Adaptation::default()
    }
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
    publisher: Live,
    /// Held because dropping it stops publishing.
    _broadcast: LocalBroadcast,
    subscriber: Live,
    subscription: Subscription,
    broadcast: RemoteBroadcast,
}

impl Fixture {
    /// Builds the lab, publishes `renditions` of a gradient and subscribes to it.
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

        let publisher = Live::builder(publisher_endpoint).with_router().spawn();
        let broadcast = LocalBroadcast::new();
        publisher
            .publish("patchbay", &broadcast)
            .expect("failed to publish");
        broadcast
            .set_video(gradient(size), VideoEncoding::ladder(renditions))
            .expect("failed to set video");

        // No address lookup. The lab gives each device a fixed address.
        let publisher_addr = publisher.endpoint().addr();
        let subscriber = Live::builder(subscriber_endpoint).spawn();
        subscriber
            .moq()
            .connect(publisher_addr)
            .await
            .expect("failed to connect");
        let ticket = BroadcastTicket::new(publisher.endpoint().id(), "patchbay");
        let subscription = subscriber
            .moq()
            .subscribe(ticket.path(), Reach::Direct(ticket.peer()))
            .await
            .expect("failed to subscribe");
        let remote = subscriber.remote_broadcast(&subscription);

        Self {
            lab,
            publisher_node,
            subscriber_node,
            router_node,
            publisher,
            _broadcast: broadcast,
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
        self.play_with(renditions, RenditionMode::auto(), Adaptation::default())
            .await
    }

    /// Plays the broadcast with adaptation at [`quick`] timers.
    async fn play_auto_quick(&self, renditions: usize) -> Viewer {
        self.play_with(renditions, RenditionMode::auto(), quick())
            .await
    }

    async fn play_with(
        &self,
        renditions: usize,
        mode: RenditionMode,
        adaptation: Adaptation,
    ) -> Viewer {
        let broadcast = &self.broadcast;
        let mut catalog = broadcast.catalog();
        tokio::time::timeout(TIMEOUT, async {
            while catalog
                .get()
                .is_none_or(|catalog| catalog.video.renditions.len() < renditions)
            {
                catalog.updated().await.expect("the broadcast is alive");
            }
        })
        .await
        .expect("timed out waiting for the video catalog");

        let player = broadcast
            .play(PlayerConfig {
                rendition: mode,
                adaptation,
                ..PlayerConfig::default()
            })
            .expect("failed to play");
        let frames = player.video();
        Viewer { player, frames }
    }

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
    let format = VideoFormat {
        size,
        rate: Rate::new(FRAMERATE, 1).expect("a valid rate"),
    };
    VideoSource::spawn("gradient", format, move |sender| {
        let started = Instant::now();
        let mut rgba = vec![0u8; (size.width * size.height * 4) as usize];
        let mut next = Instant::now();
        for tick in 0u32.. {
            let phase = tick.wrapping_mul(3) as u8;
            for (offset, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = (offset as u32 % size.width) as u8;
                let y = (offset as u32 / size.width) as u8;
                *pixel = [x.wrapping_add(phase), y.wrapping_add(phase), phase, 0xff];
            }
            let surface = Surface::rgba(&rgba, size).expect("the buffer fits the size");
            let timestamp = moq_net::Timestamp::from_micros(started.elapsed().as_micros() as u64)
                .expect("in range");
            if sender.push(Frame::new(surface, timestamp)).is_err() {
                break;
            }
            next += FRAME_INTERVAL;
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        Ok(())
    })
    .expect("the gradient thread starts")
}

/// The link serving a subscription, looked up on every read.
struct Link(Subscription);

impl Link {
    /// Returns the latest reading, or an empty one while no link serves.
    fn read(&self) -> LinkSample {
        self.0
            .link()
            .map(|serving| serving.sample)
            .unwrap_or_default()
    }
}

/// Reports whether `sample`'s round trip is over ten times its minimum.
///
/// That means a queue, not a longer path.
fn queued(sample: &LinkSample) -> bool {
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
        self.player.status().get().rendition.unwrap_or_default()
    }

    /// Waits for the next frame.
    async fn next(&mut self) -> Option<std::sync::Arc<Frame>> {
        self.frames.next().await
    }
}

/// Waits until `rendition` is on screen.
async fn switched_to(player: &Player, rendition: &str) {
    let mut status = player.status();
    while status.get().rendition.as_deref() != Some(rendition) {
        status.updated().await.expect("the player is alive");
    }
}

/// Waits until the player is switching to `rendition`, or has.
async fn requested(player: &Player, rendition: &str) {
    let mut status = player.status();
    loop {
        let current = status.get();
        if current.switching_to.as_deref() == Some(rendition)
            || current.rendition.as_deref() == Some(rendition)
        {
            return;
        }
        status.updated().await.expect("the player is alive");
    }
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

/// Returns a two-rung ladder, `high` at 800 kbit/s and `low` at 200 kbit/s.
///
/// The bitrates are encoder ceilings. Over a clear link this picture arrives at
/// about 316 kbit/s on `high` and 84 on `low`, some 40% of the ceilings, so an
/// impairment has to be tighter than the ladder suggests.
fn ladder() -> Vec<VideoRendition> {
    vec![
        VideoRendition {
            bitrate: Some(Bitrate::from_bps(800_000)),
            ..VideoRendition::new("high")
        },
        VideoRendition {
            size: Some(Size::new(320, 240)),
            bitrate: Some(Bitrate::from_bps(200_000)),
            ..VideoRendition::new("low")
        },
    ]
}

/// The longest a change on the link may take to reach the signals.
///
/// The goodput window is two seconds and follows a cap within a few seconds,
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
        vec![VideoRendition {
            bitrate: Some(Bitrate::from_bps(500_000)),
            ..VideoRendition::new("video")
        }],
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
        vec![VideoRendition {
            bitrate: Some(Bitrate::from_bps(500_000)),
            ..VideoRendition::new("video")
        }],
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
/// connection monitor. `e2e::adaptive_rendition_switching` feeds made-up
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

    // `LinkSample::loss_rate` counts the packets this endpoint sends, mostly
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
/// No packets are dropped. The cap shows as goodput pinned to it and a round
/// trip inflated by the queue. The test checks both in the
/// signals before it waits for the downgrade.
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

    // Half of what `high` sends at its quietest, and above what `low` sends.
    fixture
        .impair(LinkCondition::new().rate_kbit(cap_kbit))
        .await;

    // Waits until the cap has shown in goodput and round trip for three
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
            worst_loss = worst_loss.max(signals.loss_rate.unwrap_or(0.0));
            let pinned = signals.goodput_bps.is_some_and(|bps| bps < 250_000);
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
        goodput_kbps = saw_the_cap.goodput_bps.unwrap_or(0) / 1000,
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
/// Ignored because of a moq-net defect. Its send queue is session-wide, keyed
/// `(track_priority, group_sequence)`, and ties go to the higher sequence. The
/// replacement's first group, its keyframe, starts at sequence 0 and waits
/// behind every group the incumbent still has queued. On a saturated link that
/// queue never empties. The fix is to rank groups by age within their track.
#[tokio::test]
#[traced_test]
#[ignore = "the replacement track's first group is starved behind the incumbent's higher-numbered ones; passes about five runs in six"]
async fn a_switch_lands_while_the_link_stays_capped() {
    let fixture = Fixture::start(Size::new(640, 480), ladder()).await;
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
            if signals.goodput_bps.is_some_and(|bps| bps < 250_000) && queued(&signals) {
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
    let mut last = status.get();
    let (mut switches, mut requests) = (0u32, 0u32);
    let mut frames = 0u32;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        tokio::select! {
            updated = status.updated() => {
                let current = updated.expect("the player is alive");
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
    // goodput baseline.
    let _settle = drain(&mut viewer.frames, Duration::from_secs(3)).await;

    let before = signals.read();
    info!(
        rtt_ms = millis(before.rtt),
        min_rtt_ms = millis(before.min_rtt),
        goodput_kbps = ?before.goodput_bps.map(|bps| bps / 1000),
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
        goodput_kbps = ?after.goodput_bps.map(|bps| bps / 1000),
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
