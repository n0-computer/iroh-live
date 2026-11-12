//! Publishing and playing back in-process, with no transport.
//!
//! Each test builds a [`LocalBroadcast`] from a generated or pushed source and
//! plays it through [`RemoteBroadcast::from_consumer`]. That covers the whole pipeline
//! from source to decoded frame, encoders and decoders included.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use moq_ext_player::{
    DEFAULT_LATENCY, Delay, Error, IMMEDIATE_LATENCY, NetworkSample, Player, PlayerConfig,
    RemoteBroadcast, RenditionMode, SlotState,
};
use moq_ext_publisher::{LocalBroadcast, VideoEncoding, VideoRendition, audio_encoding};
use moq_ext_sources::{AudioSource, FrameSender, VideoFormat, VideoSource};
use moq_mux::catalog::Stream as _;
use moq_net::bandwidth::Rate;
use tokio::{sync::watch, task::JoinHandle};

/// Reads `broadcast` in-process, without a transport.
fn local(broadcast: &LocalBroadcast) -> RemoteBroadcast {
    RemoteBroadcast::from_consumer(broadcast.consume())
}

/// How long any one wait may take.
///
/// Generous, because debug-build software codecs share the machine with the
/// rest of the suite.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Returns a frame rate of `frames_per_second`.
fn fps(frames_per_second: u32) -> moq_video::FrameRate {
    moq_video::FrameRate::new(frames_per_second, 1).expect("a valid rate")
}

/// Returns `encoding` on the software encoder, which every build has.
fn software(mut encoding: VideoEncoding) -> VideoEncoding {
    for rendition in &mut encoding.renditions {
        rendition.config.kind = moq_video::encode::Kind::Software;
    }
    encoding
}

/// Returns a rendition named `name` at `size` and `bitrate`.
fn rung(name: &str, size: moq_video::Size, bitrate: Rate) -> VideoRendition {
    let mut rendition = VideoRendition {
        size: Some(size),
        ..VideoRendition::new(name)
    };
    rendition.config.bitrate = Some(bitrate);
    rendition
}

/// Waits until `receiver` holds a value `done` accepts, and returns it.
async fn until<T: Clone>(mut receiver: watch::Receiver<T>, done: impl FnMut(&T) -> bool) -> T {
    tokio::time::timeout(TIMEOUT, receiver.wait_for(done))
        .await
        .expect("the awaited value never came")
        .expect("the watched value is alive")
        .clone()
}

/// Waits until the rendition `name` is on screen.
async fn on_screen(player: &Player, name: &str) {
    until(player.status(), |status| {
        status.rendition.as_deref() == Some(name)
    })
    .await;
}

/// Pushes flat grey frames at 30 fps until the source is gone.
async fn push_frames(sender: FrameSender<moq_video::Frame>, format: VideoFormat) {
    let rgba = vec![0x80u8; format.size.pixels() as usize * 4];
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    for index in 0u64.. {
        tick.tick().await;
        let surface = moq_video::Surface::rgba(&rgba, format.size).expect("valid");
        let timestamp = moq_net::Timestamp::from_micros(index * 33_333).expect("in range");
        if sender
            .push(moq_video::Frame::new(surface, timestamp))
            .is_err()
        {
            return;
        }
    }
}

/// Returns a two-rung ladder of the test pattern.
fn ladder() -> (LocalBroadcast, VideoSource) {
    let source = VideoSource::test_pattern(moq_video::Size::new(640, 360), fps(30))
        .expect("the pattern thread starts");
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source.clone(),
            software(VideoEncoding::ladder([
                VideoRendition {
                    size: Some(moq_video::Size::new(640, 360)),
                    ..VideoRendition::new("high")
                },
                VideoRendition {
                    size: Some(moq_video::Size::new(320, 180)),
                    ..VideoRendition::new("low")
                },
            ])),
        )
        .expect("a valid ladder");
    (broadcast, source)
}

/// Waits until `frames` yields a picture of `size`.
async fn wait_for_size(frames: &mut moq_ext_player::VideoFrames, size: moq_video::Size) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let frame = frames.next().await.expect("the player keeps playing");
            if frame.size() == size {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {size} picture arrived"));
}

#[tokio::test]
async fn a_local_broadcast_plays_in_process() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("low"),
            ..PlayerConfig::default()
        })
        .expect("a valid config");
    on_screen(&player, "low").await;
    let mut frames = player.video_frames();
    wait_for_size(&mut frames, moq_video::Size::new(320, 180)).await;
    let status = player.status().borrow().clone();
    assert_eq!(status.video, SlotState::Running);
    assert_eq!(status.rendition.as_deref(), Some("low"));
    assert!(status.decoder.is_some());
    // The timeline has the pictures shown, none presented before it decoded.
    let timeline = player.timeline();
    assert!(
        timeline
            .iter()
            .any(|timing| timing.kind == moq_ext_player::MediaKind::Video),
        "no picture in the timeline"
    );
    assert!(
        timeline
            .iter()
            .all(|timing| timing.presented >= timing.decoded)
    );
}

/// A player started after the catalog arrived chooses a rendition and plays.
///
/// There is no audio and no network signal, so nothing changes after the
/// player starts.
#[tokio::test]
async fn a_player_started_after_the_catalog_plays() {
    let (broadcast, _source) = ladder();
    let remote = local(&broadcast);
    until(remote.catalog(), |catalog| {
        catalog
            .as_ref()
            .is_some_and(|catalog| catalog.video.renditions.len() >= 2)
    })
    .await;
    let player = remote
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("low"),
            ..PlayerConfig::default()
        })
        .expect("valid");
    tokio::time::timeout(TIMEOUT, player.video_frames().next())
        .await
        .expect("a player of a described broadcast never chose a rendition")
        .expect("the video plays");
}

/// A bandwidth shortfall that holds moves the player down a rendition.
#[tokio::test]
async fn a_held_shortfall_moves_the_player_down() {
    let source = VideoSource::test_pattern(moq_video::Size::new(640, 360), fps(30))
        .expect("the pattern thread starts");
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::ladder([
                rung(
                    "high",
                    moq_video::Size::new(640, 360),
                    Rate::from_bps(2_000_000),
                ),
                rung(
                    "low",
                    moq_video::Size::new(320, 180),
                    Rate::from_bps(200_000),
                ),
            ])),
        )
        .expect("a valid ladder");
    let sample = Arc::new(Mutex::new(NetworkSample {
        delivery: Some(10_000_000),
        ..NetworkSample::default()
    }));
    let reader = sample.clone();
    let remote = local(&broadcast);
    remote.set_network(move || *reader.lock().expect("poisoned"));
    let player = remote.play(PlayerConfig::default()).expect("valid");
    // A healthy link plays the top rendition.
    on_screen(&player, "high").await;
    // Room for `low` but not for `high`, with no loss, so only the hold delays
    // the switch.
    *sample.lock().expect("poisoned") = NetworkSample {
        delivery: Some(300_000),
        ..NetworkSample::default()
    };
    on_screen(&player, "low").await;
}

/// Two players of one broadcast each keep their own rendition.
#[tokio::test]
async fn two_players_of_one_broadcast_do_not_interfere() {
    let (broadcast, _source) = ladder();
    let remote = local(&broadcast);
    let high = remote
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("high"),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let low = remote
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("low"),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let mut high_frames = high.video_frames();
    let mut low_frames = low.video_frames();
    wait_for_size(&mut high_frames, moq_video::Size::new(640, 360)).await;
    wait_for_size(&mut low_frames, moq_video::Size::new(320, 180)).await;
    assert_eq!(high.status().borrow().rendition.as_deref(), Some("high"));
    assert_eq!(low.status().borrow().rendition.as_deref(), Some("low"));
}

#[tokio::test]
async fn a_pin_switches_without_ending_the_frames() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("high"),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let mut frames = player.video_frames();
    wait_for_size(&mut frames, moq_video::Size::new(640, 360)).await;

    player.set_rendition(RenditionMode::pinned("low"));
    on_screen(&player, "low").await;
    // The same handle keeps reading across the switch.
    wait_for_size(&mut frames, moq_video::Size::new(320, 180)).await;
}

#[tokio::test]
async fn a_rendition_the_catalog_lacks_is_unknown() {
    let (broadcast, _source) = ladder();
    let catalog = until(local(&broadcast).catalog(), |catalog| {
        catalog
            .as_ref()
            .is_some_and(|catalog| catalog.video.renditions.len() >= 2)
    })
    .await
    .expect("a catalog");
    let result = moq_ext_player::video_rendition(&catalog, "4k");
    let Err(Error::UnknownRendition { offered, .. }) = result else {
        panic!("{result:?}");
    };
    assert_eq!(offered, ["high", "low"]);
}

/// A pin to a missing rendition falls back to automatic selection.
///
/// The status says why.
#[tokio::test]
async fn a_pin_that_cannot_be_honoured_falls_back_and_says_why() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("4k"),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let fell_back = until(player.status(), |status| {
        status.rendition.is_some() && status.switch_error.is_some()
    })
    .await;
    assert_eq!(fell_back.rendition.as_deref(), Some("high"));
}

/// A decoder that does not open fails the switch and the video.
#[tokio::test]
async fn a_decoder_that_will_not_open_fails_the_video() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig {
            decoder: moq_video::decode::Kind::Named("no-such-decoder".to_string()),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let failed = until(player.status(), |status| {
        matches!(status.video, SlotState::Failed(_))
    })
    .await;
    assert!(failed.rendition.is_none());
    assert!(failed.switch_error.is_some());
}

/// A failed first decoder is tried again after its backoff.
///
/// This holds even with no other rendition to step to.
#[tokio::test]
async fn a_failed_first_decoder_is_tried_again() {
    let source = VideoSource::test_pattern(moq_video::Size::new(320, 180), fps(30))
        .expect("the pattern thread starts");
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::single(VideoRendition::new("video"))),
        )
        .expect("valid");
    let player = local(&broadcast)
        .play(PlayerConfig {
            decoder: moq_video::decode::Kind::Named("no-such-decoder".to_string()),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let first = until(player.status(), |status| status.switch_error.is_some())
        .await
        .switch_error
        .expect("the first open fails");
    until(player.status(), |status| {
        status
            .switch_error
            .as_ref()
            .is_some_and(|error| *error != first)
    })
    .await;
}

/// One rendition of the test pattern at `size`, called `video`.
fn single(size: moq_video::Size) -> LocalBroadcast {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            VideoSource::test_pattern(size, fps(30)).expect("the pattern thread starts"),
            software(VideoEncoding::single(VideoRendition {
                size: Some(size),
                ..VideoRendition::new("video")
            })),
        )
        .expect("a valid encoding");
    broadcast
}

/// Records every video state the player reports until dropped.
fn record_states(player: &moq_ext_player::Player) -> (Arc<Mutex<Vec<SlotState>>>, JoinHandle<()>) {
    let states = Arc::new(Mutex::new(Vec::new()));
    let mut status = player.status();
    let written = states.clone();
    let task = tokio::spawn(async move {
        loop {
            let video = status.borrow_and_update().video.clone();
            written.lock().expect("poisoned").push(video);
            if status.changed().await.is_err() {
                return;
            }
        }
    });
    (states, task)
}

/// Asserts no state in `states` said the video was over.
fn never_over(states: &Mutex<Vec<SlotState>>) {
    let states = states.lock().expect("poisoned");
    assert!(
        !states
            .iter()
            .any(|state| matches!(state, SlotState::Ended | SlotState::Failed(_))),
        "the player called the video over on the way: {states:?}"
    );
}

/// Publishes a moving test pattern of `size` at `live/cam` on `origin`.
///
/// It stands in for a remote publisher's broadcast, as a transport carries it.
/// The route costs `cost`. The broadcast and its route end when the task is
/// aborted.
fn serve(origin: &moq_net::origin::Producer, cost: u64, size: moq_video::Size) -> JoinHandle<()> {
    let route = origin
        .route(
            "live/cam",
            moq_net::origin::RouteOptions::hidden().with_cost(cost),
        )
        .expect("a route");
    let (_track, encode) = pattern(route.broadcast().clone(), size);
    route.announce().expect("announced");
    tokio::spawn(async move {
        let _route = route;
        encode.await;
    })
}

/// Encodes a moving test pattern of `size` into a `video` track of `broadcast`.
///
/// Returns the track, whose aggregate subscription shows what the readers ask
/// for, and the future that encodes until dropped.
fn pattern(
    mut broadcast: moq_net::broadcast::Producer,
    size: moq_video::Size,
) -> (
    moq_net::track::Producer,
    impl std::future::Future<Output = ()> + Send + 'static,
) {
    let catalog: moq_mux::catalog::Producer = moq_mux::catalog::Producer::new(
        &mut broadcast,
        moq_mux::catalog::Config::default().with_catalog(hang::catalog::Catalog::default()),
    )
    .expect("a new broadcast has no catalog track yet");
    let track = broadcast
        .create_track(
            "video",
            Some(catalog.track_info(hang::catalog::PRIORITY.video)),
        )
        .expect("a new track");
    let mut import =
        moq_mux::codec::h264::Import::new(track.clone(), catalog.reserve(), Default::default())
            .expect("an importer");
    let encode = async move {
        // Held until the encoding stops, so the catalog and the broadcast stay up.
        let _catalog = catalog;
        let _broadcast = broadcast;
        let mut split = moq_mux::codec::h264::Split::new();
        let mut config = moq_video::encode::Config::new(size.width, size.height, fps(30));
        config.kind = moq_video::encode::Kind::Software;
        config.join = Duration::from_secs(1);
        let mut encoder = moq_video::encode::Sink::open(&config)
            .await
            .expect("the software encoder opens");
        let mut tick = tokio::time::interval(Duration::from_millis(33));
        for index in 0..u64::MAX {
            tick.tick().await;
            if index % 30 == 0 {
                encoder
                    .cut()
                    .await
                    .expect("the software encoder can cut a group");
            }
            let mut rgba = vec![0u8; size.pixels() as usize * 4];
            for (offset, byte) in rgba.iter_mut().enumerate() {
                *byte = (offset / 4 + index as usize * 37) as u8;
            }
            let surface = moq_video::Surface::rgba(&rgba, size).expect("valid");
            let timestamp = moq_net::Timestamp::from_micros(index * 33_333).expect("in range");
            encoder
                .encode(moq_video::Frame::new(surface, timestamp))
                .await
                .expect("the software encoder takes every frame");
            while let Some(unit) = encoder.receive().expect("the encoder hands out its units") {
                let mut frames = split.decode(&unit.payload, unit.timestamp).expect("split");
                frames.extend(split.flush(unit.timestamp).expect("flushed"));
                import.decode(frames).expect("imported");
            }
        }
    };
    (track, encode)
}

/// A player keeps playing when its route goes and another serves the path.
///
/// The transport ends a broadcast when its session closes, so `from_resolved`
/// asks for the path again. The player takes the new broadcast as a switch and
/// never reports the video ended.
#[tokio::test]
async fn a_player_keeps_playing_when_its_route_goes() {
    let (origin, driver) = moq_net::origin::Producer::new(Default::default());
    let driver = tokio::spawn(moq_net::time::run(driver));
    // The same path twice, as a direct peer and a relay would carry it. The
    // cheaper direct route serves first.
    let direct_route = serve(&origin, 1, moq_video::Size::new(320, 180));
    let relayed_route = serve(&origin, 10, moq_video::Size::new(640, 360));

    let path = moq_net::Path::new("live/cam").expect("a valid path");
    let first = origin
        .consume()
        .follow(path.clone(), None)
        .expect("the path can be followed")
        .available()
        .await
        .expect("the path resolves")
        .expect("a route serves the path");
    let remote = RemoteBroadcast::from_resolved(origin.consume(), path, first);
    let player = remote.play(PlayerConfig::default()).expect("valid");
    let mut frames = player.video_frames();
    wait_for_size(&mut frames, moq_video::Size::new(320, 180)).await;
    let (states, recorder) = record_states(&player);

    // The direct session goes: its route is withdrawn and its broadcast ends.
    direct_route.abort();
    let _ = direct_route.await;

    wait_for_size(&mut frames, moq_video::Size::new(640, 360)).await;
    on_screen(&player, "video").await;
    assert!(!remote.is_closed());
    assert_eq!(player.status().borrow().video, SlotState::Running);
    recorder.abort();
    never_over(&states);
    relayed_route.abort();
    driver.abort();
}

/// Video comes back after the publisher replaces it, and never reports ended.
///
/// In-process the old track usually ends before the new catalog arrives.
/// `select::tests::a_track_that_ended_is_asked_for_again` forces the other
/// order.
#[tokio::test]
async fn video_comes_back_after_the_publisher_replaces_it() {
    let broadcast = single(moq_video::Size::new(320, 180));
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video_frames();
    wait_for_size(&mut frames, moq_video::Size::new(320, 180)).await;
    let (states, recorder) = record_states(&player);
    // A camera switch: the same rendition name, another source.
    let size = moq_video::Size::new(640, 360);
    broadcast
        .set_video(
            VideoSource::test_pattern(size, fps(30)).expect("the pattern thread starts"),
            software(VideoEncoding::single(VideoRendition {
                size: Some(size),
                ..VideoRendition::new("video")
            })),
        )
        .expect("a valid encoding");
    wait_for_size(&mut frames, size).await;
    recorder.abort();
    never_over(&states);
}

#[tokio::test]
async fn turning_video_off_leaves_nothing_decoding() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video_frames();
    wait_for_size(&mut frames, moq_video::Size::new(640, 360)).await;
    player.set_rendition(RenditionMode::Off);
    until(player.status(), |status| {
        status.video == SlotState::Off && status.rendition.is_none()
    })
    .await;
    // And it comes back.
    player.set_rendition(RenditionMode::Auto);
    on_screen(&player, "high").await;
}

#[tokio::test]
async fn a_latency_whose_maximum_is_below_its_minimum_is_refused() {
    let (broadcast, _source) = ladder();
    let config = PlayerConfig {
        latency: Delay {
            min: Some(Duration::from_millis(500)),
            max: Some(Duration::from_millis(100)),
        },
        ..PlayerConfig::default()
    };
    let result = local(&broadcast).play(config);
    assert!(matches!(result, Err(Error::InvalidConfig { .. })));
}

/// Waits until the readers of `track` ask for `latency`, and nothing else.
async fn subscribed_with(track: &moq_net::track::Producer, wanted: Delay) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if track.subscription().is_some_and(|sub| sub.delay == wanted) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the subscription asks for {:?}, not {wanted:?}",
            track.subscription().map(|sub| sub.delay)
        )
    });
}

/// Waits until the player's playout clock runs `delay` behind live.
async fn clock_at(player: &Player, delay: Duration) {
    tokio::time::timeout(TIMEOUT, async {
        while player.stats().latency != delay {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the clock runs {:?} behind, not {delay:?}",
            player.stats().latency
        )
    });
}

/// Waits for `count` more pictures.
async fn more_frames(frames: &mut moq_ext_player::VideoFrames, count: usize) {
    tokio::time::timeout(TIMEOUT, async {
        for _ in 0..count {
            frames.next().await.expect("the player keeps playing");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fewer than {count} pictures arrived"));
}

/// A running player takes a new latency without ending its video.
///
/// The subscription asks for the new range once the rebuilt decoder has taken
/// over, and the playout clock runs the new minimum behind live, in both
/// directions and into an unpaced latency.
#[tokio::test]
async fn a_running_player_changes_its_latency() {
    let size = moq_video::Size::new(320, 180);
    let broadcast = moq_net::broadcast::Info::new().produce();
    let consumer = broadcast.consume();
    let (track, encode) = pattern(broadcast, size);
    let encode = tokio::spawn(encode);
    let player = RemoteBroadcast::from_consumer(consumer)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video_frames();
    wait_for_size(&mut frames, size).await;
    subscribed_with(&track, DEFAULT_LATENCY).await;
    clock_at(&player, Duration::from_millis(100)).await;
    let (states, recorder) = record_states(&player);

    let deeper = Delay {
        min: Some(Duration::from_millis(300)),
        max: Some(Duration::from_millis(400)),
    };
    player.set_latency(deeper).expect("a valid latency");
    clock_at(&player, Duration::from_millis(300)).await;
    subscribed_with(&track, deeper).await;
    more_frames(&mut frames, 10).await;

    player
        .set_latency(IMMEDIATE_LATENCY)
        .expect("a valid latency");
    clock_at(&player, Duration::ZERO).await;
    subscribed_with(&track, IMMEDIATE_LATENCY).await;
    more_frames(&mut frames, 10).await;

    // A refused latency changes nothing.
    let inverted = Delay {
        min: Some(Duration::from_millis(500)),
        max: Some(Duration::from_millis(100)),
    };
    assert!(matches!(
        player.set_latency(inverted),
        Err(Error::InvalidConfig { .. })
    ));
    more_frames(&mut frames, 5).await;
    assert_eq!(player.stats().latency, Duration::ZERO);
    subscribed_with(&track, IMMEDIATE_LATENCY).await;

    assert_eq!(player.status().borrow().video, SlotState::Running);
    recorder.abort();
    never_over(&states);
    encode.abort();
}

/// Audio decodes without a speaker.
///
/// It still runs the audio path and its clock.
#[tokio::test]
async fn audio_decodes_without_a_speaker() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
            audio_encoding::voice(),
        )
        .expect("a valid encoding");
    let player = local(&broadcast)
        .play(PlayerConfig {
            audio: true,
            ..PlayerConfig::default()
        })
        .expect("valid");
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if player.stats().audio.is_some_and(|audio| audio.frames > 10) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("audio frames played");
    assert_eq!(player.status().borrow().audio, SlotState::Running);
}

/// The decoded audio reaches an `AudioFrames` reader, with its format and rising timestamps.
#[tokio::test]
async fn the_decoded_audio_reaches_an_audio_frames_reader() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
            audio_encoding::voice(),
        )
        .expect("a valid encoding");
    let player = local(&broadcast)
        .play(PlayerConfig {
            audio: true,
            ..PlayerConfig::default()
        })
        .expect("valid");
    let mut audio = player.audio_frames();
    let mut previous = None;
    let mut peak = 0f32;
    for _ in 0..10 {
        let frame = tokio::time::timeout(TIMEOUT, audio.next())
            .await
            .expect("an audio frame")
            .expect("the player is alive");
        assert!(frame.format.sample_rate > 0);
        assert!(!frame.samples.is_empty());
        assert!(!frame.duration().is_zero());
        assert!(previous.is_none_or(|previous| frame.timestamp > previous));
        previous = Some(frame.timestamp);
        peak = frame
            .samples
            .iter()
            .fold(peak, |peak, sample| peak.max(sample.abs()));
    }
    assert!(
        peak > 0.01,
        "a 440 Hz tone decoded as silence (peak {peak})"
    );
    drop(player);
    // The handle still returns what was decoded before the drop, at most its
    // one-second backlog, and then ends.
    let leftover = tokio::time::timeout(TIMEOUT, async {
        let mut leftover = 0usize;
        while audio.next().await.is_some() {
            leftover += 1;
        }
        leftover
    })
    .await
    .expect("the frames outlived the player");
    assert!(
        leftover <= 50,
        "{leftover} frames came after the player was dropped"
    );
}

/// Closing the broadcast ends the player's audio.
#[tokio::test]
async fn closing_the_broadcast_ends_the_audio() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
            audio_encoding::voice(),
        )
        .expect("a valid encoding");
    let player = local(&broadcast)
        .play(PlayerConfig {
            audio: true,
            ..PlayerConfig::default()
        })
        .expect("valid");
    until(player.status(), |status| status.audio == SlotState::Running).await;
    broadcast.close();
    until(player.status(), |status| status.audio == SlotState::Ended).await;
}

/// A broadcast with only audio ends the player's video.
#[tokio::test]
async fn an_audio_only_broadcast_ends_the_video() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts"),
            audio_encoding::voice(),
        )
        .expect("a valid encoding");
    let player = local(&broadcast)
        .play(PlayerConfig {
            audio: true,
            ..PlayerConfig::default()
        })
        .expect("valid");
    let status = until(player.status(), |status| status.video == SlotState::Ended).await;
    assert_eq!(status.audio, SlotState::Running);
}

/// Audio comes back after the publisher replaces it.
#[tokio::test]
async fn audio_comes_back_after_the_publisher_replaces_it() {
    let broadcast = LocalBroadcast::new();
    let tone =
        || AudioSource::tone(440.0, moq_audio::Layout::Mono).expect("the tone thread starts");
    broadcast
        .set_audio(tone(), audio_encoding::voice())
        .expect("a valid encoding");
    let player = local(&broadcast)
        .play(PlayerConfig {
            audio: true,
            ..PlayerConfig::default()
        })
        .expect("valid");
    let frames_past = |count: u64| {
        let player = &player;
        async move {
            loop {
                if player
                    .stats()
                    .audio
                    .is_some_and(|audio| audio.frames > count)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };
    tokio::time::timeout(TIMEOUT, frames_past(10))
        .await
        .expect("the first track played");
    let played = player.stats().audio.map_or(0, |audio| audio.frames);
    broadcast
        .set_audio(tone(), audio_encoding::voice())
        .expect("a valid encoding");
    // The stats restart with every track, so a lower count means the first
    // track ended. This polls, because the status can pass through `Ended`
    // faster than a watcher sees.
    tokio::time::timeout(TIMEOUT, async {
        while player
            .stats()
            .audio
            .is_some_and(|audio| audio.frames >= played)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first track never ended");
    tokio::time::timeout(TIMEOUT, frames_past(10))
        .await
        .expect("the replacement never played");
    assert_eq!(player.status().borrow().audio, SlotState::Running);
}

/// The broadcast's status follows its video slot, back to off once cleared.
#[tokio::test]
async fn the_publish_status_follows_the_video_slot() {
    let (broadcast, _source) = ladder();
    let running = until(broadcast.status(), |status| {
        status.video == SlotState::Running
    })
    .await;
    assert_eq!(
        running.renditions.keys().collect::<Vec<_>>(),
        ["high", "low"]
    );
    broadcast.clear_video();
    let cleared = broadcast.status().borrow().clone();
    assert_eq!(cleared.video, SlotState::Off);
    assert!(cleared.renditions.is_empty());
}

/// A source that fails shows why in the broadcast's status.
#[tokio::test]
async fn a_source_that_fails_shows_in_the_status() {
    let format = VideoFormat {
        size: moq_video::Size::new(64, 48),
        rate: fps(30),
    };
    let source = VideoSource::spawn("failing", format, |_frames| {
        Err(moq_ext_sources::Error::from(std::io::Error::other(
            "the camera caught fire",
        )))
    })
    .expect("the thread starts");
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(source, VideoEncoding::single(VideoRendition::new("video")))
        .expect("valid");
    let status = until(broadcast.status(), |status| {
        matches!(status.video, SlotState::Failed(_))
    })
    .await;
    let SlotState::Failed(failed) = status.video else {
        unreachable!("waited for a failure")
    };
    assert!(format!("{failed:#}").contains("caught fire"), "{failed:#}");
}

#[tokio::test]
async fn an_empty_ladder_is_refused_at_once() {
    let source = VideoSource::test_pattern(moq_video::Size::new(64, 48), fps(30))
        .expect("the pattern thread starts");
    let broadcast = LocalBroadcast::new();
    let result = broadcast.set_video(source, VideoEncoding::ladder([]));
    assert!(matches!(result, Err(Error::InvalidConfig { .. })));
}

/// A pushed source reports demand while a player watches.
#[tokio::test]
async fn a_pushed_source_sees_demand_while_played() {
    let format = VideoFormat {
        size: moq_video::Size::new(64, 48),
        rate: fps(30),
    };
    let (sender, source) = VideoSource::push(format);
    let feeder = tokio::spawn(push_frames(sender.clone(), format));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::single(VideoRendition::new("video"))),
        )
        .expect("valid");
    assert!(!*sender.demand().borrow(), "nobody watches yet");
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    until(sender.demand(), |wanted| *wanted).await;
    drop(player);
    feeder.abort();
}

/// A source that pushes only on demand still gets played.
///
/// The broadcast creates the track from the source's format, before any frame.
#[tokio::test]
async fn a_source_that_waits_for_demand_is_played() {
    let format = VideoFormat {
        size: moq_video::Size::new(64, 48),
        rate: fps(30),
    };
    let (sender, source) = VideoSource::push(format);
    let feeder = tokio::spawn({
        let sender = sender.clone();
        async move {
            until(sender.demand(), |wanted| *wanted).await;
            push_frames(sender, format).await;
        }
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::single(VideoRendition::new("video"))),
        )
        .expect("valid");
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let frame = tokio::time::timeout(TIMEOUT, player.video_frames().next())
        .await
        .expect("a source waiting for demand was never watched")
        .expect("the video ended");
    assert_eq!(frame.size(), format.size);
    drop(player);
    feeder.abort();
}

/// A decoded frame's timestamp leads the publisher back to the source frame it came from.
#[tokio::test]
async fn a_decoded_timestamp_maps_back_to_its_source_frame() {
    let format = VideoFormat {
        size: moq_video::Size::new(64, 48),
        rate: fps(30),
    };
    let (sender, source) = VideoSource::push(format);
    let feeder = tokio::spawn(push_frames(sender, format));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::single(VideoRendition::new("video"))),
        )
        .expect("valid");
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video_frames();
    for _ in 0..3 {
        let frame = tokio::time::timeout(TIMEOUT, frames.next())
            .await
            .expect("a frame")
            .expect("the video plays");
        let source = broadcast
            .video_source_timestamp(frame.timestamp)
            .unwrap_or_else(|| panic!("{:?} was not published", frame.timestamp));
        // `push_frames` stamps frame n at n * 33_333 us.
        assert_eq!(
            source.as_micros() % 33_333,
            0,
            "{source:?} was never pushed"
        );
    }
    drop(player);
    feeder.abort();
}

/// A decoded timestamp leads the publisher back to when its source frame was pushed.
#[tokio::test]
async fn a_decoded_timestamp_maps_back_to_when_its_frame_was_pushed() {
    let format = VideoFormat {
        size: moq_video::Size::new(64, 48),
        rate: fps(30),
    };
    let (sender, source) = VideoSource::push(format);
    // The window around each push, by the source timestamp it carried.
    let pushed = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let feeder = tokio::spawn({
        let pushed = pushed.clone();
        async move {
            let rgba = vec![0x80u8; format.size.pixels() as usize * 4];
            let mut tick = tokio::time::interval(Duration::from_millis(33));
            for index in 0u64.. {
                tick.tick().await;
                let surface = moq_video::Surface::rgba(&rgba, format.size).expect("valid");
                let timestamp = moq_net::Timestamp::from_micros(index * 33_333).expect("in range");
                let before = std::time::Instant::now();
                let open = sender
                    .push(moq_video::Frame::new(surface, timestamp))
                    .is_ok();
                let after = std::time::Instant::now();
                pushed
                    .lock()
                    .expect("poisoned")
                    .insert(timestamp, (before, after));
                if !open {
                    return;
                }
            }
        }
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::single(VideoRendition::new("video"))),
        )
        .expect("valid");
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video_frames();
    for _ in 0..3 {
        let frame = tokio::time::timeout(TIMEOUT, frames.next())
            .await
            .expect("a frame")
            .expect("the video plays");
        let source = broadcast
            .video_source_timestamp(frame.timestamp)
            .expect("published here");
        let captured = broadcast
            .video_captured_at(frame.timestamp)
            .expect("a pushed frame has a send instant");
        let (before, after) = pushed.lock().expect("poisoned")[&source];
        assert!(
            before <= captured && captured <= after,
            "{captured:?} is outside the push of {source:?}"
        );
    }
    assert_eq!(
        broadcast.video_captured_at(moq_net::Timestamp::from_micros(1).expect("1")),
        None,
        "nothing was published at 1 us"
    );
    drop(player);
    feeder.abort();
}

/// A frame from the player's video looks up when its data arrived, before it was decoded.
#[tokio::test]
async fn a_presented_frame_knows_when_its_data_arrived() {
    let format = VideoFormat {
        size: moq_video::Size::new(64, 48),
        rate: fps(30),
    };
    let (sender, source) = VideoSource::push(format);
    let feeder = tokio::spawn(push_frames(sender, format));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::single(VideoRendition::new("video"))),
        )
        .expect("valid");
    let player = local(&broadcast)
        .play(PlayerConfig {
            record_arrivals: true,
            ..PlayerConfig::default()
        })
        .expect("valid");
    let mut frames = player.video_frames();
    let mut arrived = 0;
    for _ in 0..10 {
        let frame = tokio::time::timeout(TIMEOUT, frames.next())
            .await
            .expect("a frame")
            .expect("the video plays");
        let timing = player
            .video_timing(frame.timestamp.into())
            .expect("a presented frame is on the timeline");
        if let Some(at) = timing.arrived {
            assert!(at <= timing.decoded, "{timing:?} decoded before it arrived");
            arrived += 1;
        }
    }
    // A frame cached before the watch subscribed has no arrival, so allow
    // the first to miss.
    assert!(
        arrived >= 9,
        "only {arrived} of 10 frames knew their arrival"
    );
    drop(player);
    feeder.abort();
}

/// A player's frames end when the broadcast closes.
///
/// The video then reports `Ended`.
#[tokio::test]
async fn the_frames_end_when_the_broadcast_closes() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video_frames();
    tokio::time::timeout(TIMEOUT, frames.next())
        .await
        .expect("a first frame")
        .expect("the video plays");
    broadcast.close();
    tokio::time::timeout(TIMEOUT, async { while frames.next().await.is_some() {} })
        .await
        .expect("the frames went on after the broadcast closed");
    let status = player.status().borrow().clone();
    assert_eq!(status.video, SlotState::Ended, "{status:?}");
    assert_eq!(status.rendition, None);
}

/// A player's frames end when the player is dropped, while the broadcast goes on.
#[tokio::test]
async fn the_frames_end_when_the_player_is_dropped() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video_frames();
    tokio::time::timeout(TIMEOUT, frames.next())
        .await
        .expect("a first frame")
        .expect("the video plays");
    drop(player);
    tokio::time::timeout(TIMEOUT, async { while frames.next().await.is_some() {} })
        .await
        .expect("the frames went on after the player was dropped");
    drop(broadcast);
}

/// An audio frames handle of a player that plays no audio ends with the player.
#[tokio::test]
async fn the_audio_frames_of_a_player_without_audio_end_with_it() {
    let (broadcast, _source) = ladder();
    let player = local(&broadcast)
        .play(PlayerConfig {
            audio: false,
            ..PlayerConfig::default()
        })
        .expect("valid");
    let mut audio = player.audio_frames();
    drop(player);
    let leftover = tokio::time::timeout(TIMEOUT, async {
        let mut leftover = 0usize;
        while audio.next().await.is_some() {
            leftover += 1;
        }
        leftover
    })
    .await
    .expect("the audio frames outlived the player");
    assert_eq!(leftover, 0, "a player without audio decoded some");
    drop(broadcast);
}

/// A moq-mux export over the broadcast's source remuxes it into a container.
#[tokio::test]
async fn a_mux_source_exports_a_container() {
    let (broadcast, _source) = ladder();
    let remote = local(&broadcast);
    let source = tokio::time::timeout(TIMEOUT, remote.mux_source())
        .await
        .expect("the broadcast has a route and a catalog")
        .expect("the broadcast is open");
    let catalog = moq_mux::catalog::Consumer::<()>::new(&source.broadcast(), Default::default())
        .await
        .expect("the catalog reads")
        .select(
            moq_mux::select::Broadcast::default()
                .video(moq_mux::select::Video::default().name("low")),
        );
    let mut export = moq_mux::container::fmp4::Export::new(source, catalog);
    let mut bytes = Vec::new();
    tokio::time::timeout(TIMEOUT, async {
        while bytes.len() < 4_096 {
            let chunk = export
                .next()
                .await
                .expect("the export runs")
                .expect("the broadcast goes on");
            bytes.extend_from_slice(&chunk);
        }
    })
    .await
    .expect("the export wrote something");
    assert_eq!(
        &bytes[4..8],
        b"ftyp",
        "an MP4 starts with its file type box"
    );
}
