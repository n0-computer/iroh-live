//! Switching the source of a published video while a peer plays it over iroh.
//!
//! The same switches as `moq-ext-player`'s `switch` tests, across a real QUIC
//! connection. Timestamps, group sequences and catalog updates cross the wire
//! here, so a switch that works in-process can still stall a remote viewer.

#![cfg(feature = "media")]

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets, protocol::Router};
use iroh_live::{
    Node,
    layout::LiveLayout,
    media::{
        FrameSender, LocalBroadcast, PlayerConfig, VideoEncoding, VideoFormat, VideoFrames,
        VideoRendition, VideoSource, remote_broadcast,
        video::{Frame, FrameRate, I420, Size, Surface},
    },
};
use n0_tracing_test::traced_test;
use tracing::info;

/// How long a switch may take until the remote player shows the new source.
const SWITCH: Duration = Duration::from_secs(2);

/// How long the first picture may take, with the connection still to open.
const FIRST: Duration = Duration::from_secs(30);

/// How many frames the player must show after a switch to count as playing.
const PLAYING: usize = 10;

/// A standard-definition frame size.
const SD: Size = Size {
    width: 640,
    height: 360,
};
/// A high-definition frame size.
const HD: Size = Size {
    width: 1280,
    height: 720,
};

/// Returns a frame rate of `frames` per second.
fn fps(frames: u32) -> FrameRate {
    FrameRate::new(frames, 1).expect("a valid rate")
}

/// Binds an endpoint that resolves the other test endpoints through a shared in-memory lookup.
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

/// A publisher and a subscriber, connected.
struct Peers {
    publisher: Node,
    subscriber: Node,
    broadcast: LocalBroadcast,
    _router: Router,
}

impl Peers {
    /// Publishes an empty broadcast as `name` and returns both nodes.
    async fn new(name: &str) -> Self {
        let publisher = Node::new(endpoint().await, LiveLayout::default().config(&[]));
        let router = publisher
            .mount(Router::builder(publisher.endpoint().clone()))
            .spawn();
        let broadcast = LiveLayout::default()
            .publish(&publisher, name)
            .expect("failed to publish");
        let subscriber = Node::new(endpoint().await, LiveLayout::default().config(&[]));
        Self {
            publisher,
            subscriber,
            broadcast,
            _router: router,
        }
    }

    /// Plays the broadcast `name` on the subscriber.
    async fn play(&self, name: &str) -> (iroh_live::media::Player, VideoFrames) {
        let ticket = LiveLayout::default()
            .ticket(self.publisher.id(), name)
            .expect("a valid broadcast name");
        let subscription = self
            .subscriber
            .subscribe_ticket(&ticket)
            .await
            .expect("failed to subscribe");
        let player = remote_broadcast(&subscription)
            .play(PlayerConfig::default())
            .expect("failed to play");
        let frames = player.video();
        (player, frames)
    }

    /// Shuts down both nodes.
    async fn shutdown(self) {
        self.publisher.shutdown().await;
        self.subscriber.shutdown().await;
    }
}

/// Returns one software-encoded rendition named `video`, at `size` or the source's.
fn single(size: Option<Size>) -> VideoEncoding {
    VideoEncoding {
        prefer_hardware: false,
        ..VideoEncoding::single(VideoRendition {
            size,
            keyframe_interval: Duration::from_secs(1),
            ..VideoRendition::new("video")
        })
    }
}

/// Where a fed source's timestamps start.
#[derive(Debug, Clone, Copy)]
enum Clock {
    /// Near zero at every open, as a camera's capture timeline.
    Camera,
    /// On one process-wide clock that starts at ten seconds, as the test pattern's.
    Pattern,
}

impl Clock {
    /// Returns the timestamp of a frame captured now, for a source opened at `opened`.
    fn now(self, opened: Instant) -> iroh_live::net::Timestamp {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let micros = match self {
            Self::Camera => opened.elapsed().as_micros(),
            Self::Pattern => EPOCH.get_or_init(Instant::now).elapsed().as_micros() + 10_000_000,
        };
        iroh_live::net::Timestamp::from_micros(micros as u64).expect("in range")
    }
}

/// What a source shows: flat frames of one size and brightness, or a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    size: Size,
    /// The luma of every pixel. `None` for a picture that is not flat.
    luma: Option<u8>,
}

impl Look {
    /// Returns a flat look of `size` with every pixel at `luma`.
    const fn flat(size: Size, luma: u8) -> Self {
        Self {
            size,
            luma: Some(luma),
        }
    }

    /// Returns the look of the test pattern at `size`.
    const fn pattern(size: Size) -> Self {
        Self { size, luma: None }
    }

    /// Returns what `frame` shows, reading five pixels spread over it.
    fn of(frame: &Frame) -> Self {
        let image = frame.surface.to_i420().expect("a CPU picture");
        let (width, height) = (image.width() as usize, image.height() as usize);
        let luma = |x: usize, y: usize| image.data()[y * height / 4 * width + x * width / 4];
        let center = luma(2, 2);
        let samples = [luma(1, 1), luma(3, 1), luma(1, 3), luma(3, 3)];
        let flat = samples.iter().all(|sample| sample.abs_diff(center) <= 12);
        Self {
            size: frame.size(),
            luma: flat.then_some(center),
        }
    }

    /// Returns whether `seen` shows this, allowing for coding loss.
    fn matches(self, seen: Self) -> bool {
        let luma = match (self.luma, seen.luma) {
            (Some(luma), Some(seen)) => luma.abs_diff(seen) <= 12,
            (None, None) => true,
            _ => false,
        };
        seen.size == self.size && luma
    }
}

/// A camera at 25 fps: 1280x720, bright, timestamps from zero.
const CAMERA: (Look, u32, Clock) = (Look::flat(HD, 200), 25, Clock::Camera);
/// The test pattern as the GPU demo sends it: 640x360 at 30 fps, dark.
const PATTERN: (Look, u32, Clock) = (Look::flat(SD, 40), 30, Clock::Pattern);

/// Pushes flat `look` frames at `rate` into `sender` until aborted.
fn feed(
    sender: FrameSender<Frame>,
    (look, rate, clock): (Look, u32, Clock),
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let luma = look.luma.expect("a flat look");
        let pixels = look.size.pixels() as usize;
        let mut planes = vec![luma; pixels];
        planes.resize(pixels * 3 / 2, 128);
        let opened = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_secs(1) / rate);
        loop {
            tick.tick().await;
            let image = I420::new(look.size, planes.clone()).expect("valid planes");
            if sender
                .push(Frame::new(Surface::I420(image), clock.now(opened)))
                .is_err()
            {
                return;
            }
        }
    })
}

/// Waits until `frames` shows `look`, then until it has shown it [`PLAYING`] times.
async fn shows(frames: &mut VideoFrames, look: Look, bound: Duration, what: &str) {
    let started = Instant::now();
    let mut last = None;
    let first = tokio::time::timeout(bound, async {
        loop {
            let frame = frames.next().await?;
            let seen = Look::of(&frame);
            if look.matches(seen) {
                return Some(());
            }
            last = Some(seen);
        }
    })
    .await;
    match first {
        Err(_) => panic!("{what}: no {look:?} within {bound:?}, last saw {last:?}"),
        Ok(None) => panic!("{what}: the video ended while waiting for {look:?}"),
        Ok(Some(())) => {}
    }
    let switched = started.elapsed();
    for index in 0..PLAYING {
        let frame = tokio::time::timeout(SWITCH, frames.next())
            .await
            .unwrap_or_else(|_| panic!("{what}: frame {index} after the switch never came"))
            .unwrap_or_else(|| panic!("{what}: the video ended {index} frames after the switch"));
        let seen = Look::of(&frame);
        assert!(
            look.matches(seen),
            "{what}: frame {index} showed {seen:?}, not {look:?}"
        );
    }
    info!(what, ?switched, "switch shown");
}

/// The GPU demo's switch over iroh: one pushed source, fed from the camera, then the pattern.
#[tokio::test]
#[traced_test]
async fn switching_what_feeds_a_pushed_source_keeps_a_peer_playing() {
    let peers = Peers::new("pushed").await;
    let (sender, source) = VideoSource::push(VideoFormat {
        size: SD,
        rate: fps(30),
    });
    peers
        .broadcast
        .set_video(source, single(Some(SD)))
        .expect("a valid encoding");
    let mut feeder = feed(sender.clone(), PATTERN);
    let (_player, mut frames) = peers.play("pushed").await;
    shows(&mut frames, PATTERN.0, FIRST, "first pattern").await;

    for round in 0..2 {
        for (name, source) in [("camera", CAMERA), ("pattern", PATTERN)] {
            feeder.abort();
            tokio::time::sleep(Duration::from_millis(200)).await;
            feeder = feed(sender.clone(), source);
            let look = Look {
                size: SD,
                ..source.0
            };
            shows(
                &mut frames,
                look,
                SWITCH,
                &format!("{name} in round {round}"),
            )
            .await;
        }
    }
    feeder.abort();
    peers.shutdown().await;
}

/// `set_video` over iroh: the pattern at two sizes, a camera, and back.
#[tokio::test]
#[traced_test]
async fn replacing_the_video_source_keeps_a_peer_playing() {
    let peers = Peers::new("replaced").await;
    peers
        .broadcast
        .set_video(VideoSource::test_pattern(SD, fps(30)), single(None))
        .expect("a valid encoding");
    let (_player, mut frames) = peers.play("replaced").await;
    shows(&mut frames, Look::pattern(SD), FIRST, "first pattern").await;

    for round in 0..2 {
        peers
            .broadcast
            .set_video(VideoSource::test_pattern(HD, fps(30)), single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(HD),
            SWITCH,
            &format!("HD pattern in round {round}"),
        )
        .await;

        let (sender, camera) = VideoSource::push(VideoFormat {
            size: HD,
            rate: fps(25),
        });
        let feeder = feed(sender, CAMERA);
        peers
            .broadcast
            .set_video(camera, single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            CAMERA.0,
            SWITCH,
            &format!("camera in round {round}"),
        )
        .await;

        peers
            .broadcast
            .set_video(VideoSource::test_pattern(SD, fps(30)), single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(SD),
            SWITCH,
            &format!("SD pattern in round {round}"),
        )
        .await;
        feeder.abort();
    }
    peers.shutdown().await;
}
