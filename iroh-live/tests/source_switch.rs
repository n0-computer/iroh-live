//! Switching the source of a published video while a peer plays it over iroh.
//!
//! The same switches as `moq-ext-player`'s `switch` tests, across a real QUIC
//! connection. Timestamps, group sequences and catalog updates cross the wire
//! here, so a switch that works in-process can still stall a remote viewer.

#![cfg(feature = "media")]

use std::{future::ready, time::Instant};

use iroh::protocol::Router;
use iroh_live::{
    Node,
    layout::LiveLayout,
    media::{
        LocalBroadcast, PlayerConfig, VideoFormat, VideoFrames, VideoSource, remote_broadcast,
    },
};
use iroh_live_node::test_util::endpoint;
use moq_ext_player::test_util::{
    CAMERA, FIRST_PICTURE_TIMEOUT, HD, Look, PATTERN, SD, SWITCH_TIMEOUT, alternate_feeds, fed,
    feed, first_published, fps, shows, single,
};
use n0_tracing_test::traced_test;

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
        let publisher = Node::new(endpoint().await, LiveLayout::default().config());
        let router = publisher
            .mount(Router::builder(publisher.endpoint().clone()))
            .spawn();
        let broadcast = LiveLayout::default()
            .publish(&publisher, name)
            .expect("failed to publish");
        let subscriber = Node::new(endpoint().await, LiveLayout::default().config());
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
        let frames = player.video_frames();
        (player, frames)
    }

    /// Shuts down both nodes.
    async fn shutdown(self) {
        self.publisher.shutdown().await;
        self.subscriber.shutdown().await;
    }
}

/// A GPU application's switch over iroh: one pushed source, fed from the camera, then the pattern.
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
    let feeder = feed(sender.clone(), PATTERN);
    let (_player, mut frames) = peers.play("pushed").await;
    shows(
        &mut frames,
        PATTERN.look,
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;
    alternate_feeds(&peers.broadcast, &mut frames, &sender, feeder, 2).await;
    peers.shutdown().await;
}

/// `set_video` over iroh: the pattern at two sizes, a camera, and back.
#[tokio::test]
#[traced_test]
async fn replacing_the_video_source_keeps_a_peer_playing() {
    let peers = Peers::new("replaced").await;
    peers
        .broadcast
        .set_video(
            VideoSource::test_pattern(SD, fps(30)).expect("the pattern thread starts"),
            single(None),
        )
        .expect("a valid encoding");
    let (_player, mut frames) = peers.play("replaced").await;
    shows(
        &mut frames,
        Look::pattern(SD),
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;

    for round in 0..2 {
        peers
            .broadcast
            .set_video(
                VideoSource::test_pattern(HD, fps(30)).expect("the pattern thread starts"),
                single(None),
            )
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(HD),
            first_published(&peers.broadcast),
            SWITCH_TIMEOUT,
            &format!("HD pattern in round {round}"),
        )
        .await;

        let (camera, _feeder) = fed(CAMERA);
        peers
            .broadcast
            .set_video(camera, single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            CAMERA.look,
            first_published(&peers.broadcast),
            SWITCH_TIMEOUT,
            &format!("camera in round {round}"),
        )
        .await;

        peers
            .broadcast
            .set_video(
                VideoSource::test_pattern(SD, fps(30)).expect("the pattern thread starts"),
                single(None),
            )
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(SD),
            first_published(&peers.broadcast),
            SWITCH_TIMEOUT,
            &format!("SD pattern in round {round}"),
        )
        .await;
    }
    peers.shutdown().await;
}
