//! Publishing and playing back in-process, with no transport.
//!
//! Each test builds a [`LocalBroadcast`] from a generated or pushed source and
//! plays it through [`RemoteBroadcast::local`]. That covers the whole pipeline
//! from source to decoded frame, encoders and decoders included.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh_live_media::{
    AudioEncoding, AudioOutput, AudioSource, Bitrate, Error, FrameSender, LocalBroadcast,
    NetworkSample, PlayerConfig, RecordConfig, RecordFormat, RemoteBroadcast, RenditionMode,
    SlotState, SwitchError, VideoEncoding, VideoFormat, VideoRendition, VideoSource, audio, video,
};
use n0_watcher::Watcher;

/// How long any one wait may take.
///
/// Generous, because debug-build software codecs share the machine with the
/// rest of the suite.
const TIMEOUT: Duration = Duration::from_secs(30);

fn fps(n: u32) -> video::Rate {
    video::Rate::new(n, 1).expect("a valid rate")
}

/// Returns `encoding` on the software encoder, which every build has.
fn software(encoding: VideoEncoding) -> VideoEncoding {
    VideoEncoding {
        prefer_hardware: false,
        ..encoding
    }
}

/// Waits until `watcher` holds a value `done` accepts, and returns it.
async fn until<W: Watcher>(mut watcher: W, done: impl Fn(&W::Value) -> bool) -> W::Value {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let value = watcher.get();
            if done(&value) {
                return value;
            }
            watcher.updated().await.expect("the watched value is alive");
        }
    })
    .await
    .expect("the awaited value never came")
}

/// Pushes flat grey frames at 30 fps until the source is gone.
async fn push_frames(sender: FrameSender<video::Frame>, format: VideoFormat) {
    let rgba = vec![0x80u8; format.size.pixels() as usize * 4];
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    for index in 0u64.. {
        tick.tick().await;
        let surface = video::Surface::rgba(&rgba, format.size).expect("valid");
        let timestamp = moq_net::Timestamp::from_micros(index * 33_333).expect("in range");
        if sender.push(video::Frame::new(surface, timestamp)).is_err() {
            return;
        }
    }
}

/// Returns a two-rung ladder of the test pattern.
fn ladder() -> (LocalBroadcast, VideoSource) {
    let source = VideoSource::test_pattern(video::Size::new(640, 360), fps(30));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source.clone(),
            software(VideoEncoding::ladder([
                VideoRendition {
                    size: Some(video::Size::new(640, 360)),
                    ..VideoRendition::new("high")
                },
                VideoRendition {
                    size: Some(video::Size::new(320, 180)),
                    ..VideoRendition::new("low")
                },
            ])),
        )
        .expect("a valid ladder");
    (broadcast, source)
}

/// Waits until `frames` yields a picture of `size`.
async fn wait_for_size(frames: &mut iroh_live_media::VideoFrames, size: video::Size) {
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
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("low"),
            ..PlayerConfig::default()
        })
        .expect("a valid config");
    player
        .wait_for_rendition("low")
        .await
        .expect("the pinned rendition lands");
    let mut frames = player.video();
    wait_for_size(&mut frames, video::Size::new(320, 180)).await;
    let status = player.status().get();
    assert_eq!(status.video, SlotState::Running);
    assert_eq!(status.rendition.as_deref(), Some("low"));
    assert!(status.decoder.is_some());
    // The timeline has the pictures shown, none presented before it decoded.
    let timeline = player.timeline();
    assert!(
        timeline
            .iter()
            .any(|timing| timing.kind == iroh_live_media::MediaKind::Video),
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
    let remote = RemoteBroadcast::local(&broadcast);
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
    tokio::time::timeout(TIMEOUT, player.video().next())
        .await
        .expect("a player of a described broadcast never chose a rendition")
        .expect("the video plays");
}

/// A bandwidth shortfall that holds moves the player down a rendition.
#[tokio::test]
async fn a_held_shortfall_moves_the_player_down() {
    let source = VideoSource::test_pattern(video::Size::new(640, 360), fps(30));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::ladder([
                VideoRendition {
                    size: Some(video::Size::new(640, 360)),
                    bitrate: Some(Bitrate::from_bps(2_000_000)),
                    ..VideoRendition::new("high")
                },
                VideoRendition {
                    size: Some(video::Size::new(320, 180)),
                    bitrate: Some(Bitrate::from_bps(200_000)),
                    ..VideoRendition::new("low")
                },
            ])),
        )
        .expect("a valid ladder");
    let sample = Arc::new(Mutex::new(NetworkSample {
        delivery: Some(Bitrate::from_bps(10_000_000)),
        ..NetworkSample::default()
    }));
    let reader = sample.clone();
    let player = RemoteBroadcast::local(&broadcast)
        .with_network(move || *reader.lock().expect("poisoned"))
        .play(PlayerConfig::default())
        .expect("valid");
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("high"))
        .await
        .expect("in time")
        .expect("a healthy link plays the top rendition");
    // Room for `low` but not for `high`, with no loss, so only the hold delays
    // the switch.
    *sample.lock().expect("poisoned") = NetworkSample {
        delivery: Some(Bitrate::from_bps(300_000)),
        ..NetworkSample::default()
    };
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("low"))
        .await
        .expect("the downgrade never landed")
        .expect("the switch to low lands");
}

/// Two players of one broadcast each keep their own rendition.
#[tokio::test]
async fn two_players_of_one_broadcast_do_not_interfere() {
    let (broadcast, _source) = ladder();
    let remote = RemoteBroadcast::local(&broadcast);
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
    let mut high_frames = high.video();
    let mut low_frames = low.video();
    wait_for_size(&mut high_frames, video::Size::new(640, 360)).await;
    wait_for_size(&mut low_frames, video::Size::new(320, 180)).await;
    assert_eq!(high.status().get().rendition.as_deref(), Some("high"));
    assert_eq!(low.status().get().rendition.as_deref(), Some("low"));
}

#[tokio::test]
async fn a_pin_switches_without_ending_the_frames() {
    let (broadcast, _source) = ladder();
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig {
            rendition: RenditionMode::pinned("high"),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let mut frames = player.video();
    wait_for_size(&mut frames, video::Size::new(640, 360)).await;

    player.set_rendition(RenditionMode::pinned("low"));
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("low"))
        .await
        .expect("the switch lands in time")
        .expect("the switch lands");
    // The same handle keeps reading across the switch.
    wait_for_size(&mut frames, video::Size::new(320, 180)).await;
}

#[tokio::test]
async fn waiting_for_a_rendition_the_catalog_lacks_fails() {
    let (broadcast, _source) = ladder();
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let result = tokio::time::timeout(TIMEOUT, player.wait_for_rendition("4k"))
        .await
        .expect("the wait ends");
    assert!(
        matches!(result, Err(SwitchError::UnknownRendition { .. })),
        "{result:?}"
    );
}

/// A pin to a missing rendition falls back to automatic selection.
///
/// The status says why.
#[tokio::test]
async fn a_pin_that_cannot_be_honoured_falls_back_and_says_why() {
    let (broadcast, _source) = ladder();
    let player = RemoteBroadcast::local(&broadcast)
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
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig {
            decoder: video::decode::Kind::Named("no-such-decoder".to_string()),
            ..PlayerConfig::default()
        })
        .expect("valid");
    let result = tokio::time::timeout(TIMEOUT, player.wait_for_rendition("high"))
        .await
        .expect("the wait ends");
    assert!(
        matches!(result, Err(SwitchError::Failed { .. })),
        "{result:?}"
    );
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
    let source = VideoSource::test_pattern(video::Size::new(320, 180), fps(30));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            software(VideoEncoding::single(VideoRendition::new("video"))),
        )
        .expect("valid");
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig {
            decoder: video::decode::Kind::Named("no-such-decoder".to_string()),
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
            .is_some_and(|error| !Arc::ptr_eq(error, &first))
    })
    .await;
}

/// One rendition of the test pattern at `size`, called `video`.
fn single(size: video::Size) -> LocalBroadcast {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            VideoSource::test_pattern(size, fps(30)),
            software(VideoEncoding::single(VideoRendition {
                size: Some(size),
                ..VideoRendition::new("video")
            })),
        )
        .expect("a valid encoding");
    broadcast
}

/// Records every video state the player reports until dropped.
fn record_states(
    player: &iroh_live_media::Player,
) -> (Arc<Mutex<Vec<SlotState>>>, tokio::task::JoinHandle<()>) {
    let states = Arc::new(Mutex::new(Vec::new()));
    let mut status = player.status();
    let written = states.clone();
    let task = tokio::spawn(async move {
        loop {
            written.lock().expect("poisoned").push(status.get().video);
            if status.updated().await.is_err() {
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

/// A player keeps playing when its route goes and another serves the path.
///
/// The transport ends a broadcast when its session closes, so `from_origin`
/// asks for the path again. The player takes the new broadcast as a switch and
/// never reports the video ended.
#[tokio::test]
async fn a_player_keeps_playing_when_its_route_goes() {
    let (origin, driver) = moq_net::origin::Producer::new(Default::default());
    let driver = tokio::spawn(moq_net::time::run(driver));
    // The same path twice, as a direct peer and a relay would carry it. The
    // cheaper direct route serves first.
    let direct = single(video::Size::new(320, 180));
    let relayed = single(video::Size::new(640, 360));
    let serve = |cost: u64, broadcast: &LocalBroadcast| {
        let route = origin
            .dynamic(
                "live/cam",
                moq_net::origin::Route::default().with_cost(cost),
            )
            .expect("a route");
        let consumer = moq_net::Consume::consume(broadcast);
        tokio::spawn(async move {
            while let Ok(request) = route.requested_broadcast().await {
                request.accept(consumer.clone());
            }
        })
    };
    let direct_route = serve(1, &direct);
    let relayed_route = serve(10, &relayed);

    let remote = RemoteBroadcast::from_origin(origin.consume(), "live/cam");
    let player = remote.play(PlayerConfig::default()).expect("valid");
    let mut frames = player.video();
    wait_for_size(&mut frames, video::Size::new(320, 180)).await;
    let (states, recorder) = record_states(&player);

    // The direct session goes: its route is withdrawn and its broadcast ends.
    direct_route.abort();
    let _ = direct_route.await;
    direct.close();

    wait_for_size(&mut frames, video::Size::new(640, 360)).await;
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("video"))
        .await
        .expect("in time")
        .expect("the relayed broadcast plays");
    assert!(!remote.is_closed());
    assert_eq!(player.status().get().video, SlotState::Running);
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
    let broadcast = single(video::Size::new(320, 180));
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video();
    wait_for_size(&mut frames, video::Size::new(320, 180)).await;
    let (states, recorder) = record_states(&player);
    // A camera switch: the same rendition name, another source.
    let size = video::Size::new(640, 360);
    broadcast
        .set_video(
            VideoSource::test_pattern(size, fps(30)),
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
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video();
    wait_for_size(&mut frames, video::Size::new(640, 360)).await;
    player.set_rendition(RenditionMode::Off);
    until(player.status(), |status| {
        status.video == SlotState::Off && status.rendition.is_none()
    })
    .await;
    // And it comes back.
    player.set_rendition(RenditionMode::auto());
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("high"))
        .await
        .expect("in time")
        .expect("video came back");
}

#[tokio::test]
async fn a_latency_below_its_own_minimum_is_refused() {
    let (broadcast, _source) = ladder();
    let config = PlayerConfig {
        latency: iroh_live_media::Latency {
            min: Duration::from_millis(500),
            max: Duration::from_millis(100),
        },
        ..PlayerConfig::default()
    };
    let result = RemoteBroadcast::local(&broadcast).play(config);
    assert!(matches!(result, Err(Error::InvalidConfig { .. })));
}

/// Audio plays through a null output.
///
/// A null output still runs the audio path and its clock.
#[tokio::test]
async fn audio_plays_through_a_null_output() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, audio::Layout::Mono),
            AudioEncoding::voice(),
        )
        .expect("a valid encoding");
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig {
            audio: Some(AudioOutput::null()),
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
    assert_eq!(player.status().get().audio, SlotState::Running);
}

/// A broadcast with only audio ends the player's video.
#[tokio::test]
async fn an_audio_only_broadcast_ends_the_video() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_audio(
            AudioSource::tone(440.0, audio::Layout::Mono),
            AudioEncoding::voice(),
        )
        .expect("a valid encoding");
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig {
            audio: Some(AudioOutput::null()),
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
    let tone = || AudioSource::tone(440.0, audio::Layout::Mono);
    broadcast
        .set_audio(tone(), AudioEncoding::voice())
        .expect("a valid encoding");
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig {
            audio: Some(AudioOutput::null()),
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
        .set_audio(tone(), AudioEncoding::voice())
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
    assert_eq!(player.status().get().audio, SlotState::Running);
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
    let cleared = broadcast.status().get();
    assert_eq!(cleared.video, SlotState::Off);
    assert!(cleared.renditions.is_empty());
}

/// A source that fails shows why in the broadcast's status.
#[tokio::test]
async fn a_source_that_fails_shows_in_the_status() {
    let format = VideoFormat {
        size: video::Size::new(64, 48),
        rate: fps(30),
    };
    let source = VideoSource::spawn("failing", format, |_frames| {
        Err(iroh_live_media::Error::from(std::io::Error::other(
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
    let source = VideoSource::test_pattern(video::Size::new(64, 48), fps(30));
    let broadcast = LocalBroadcast::new();
    let result = broadcast.set_video(source, VideoEncoding::ladder([]));
    assert!(matches!(result, Err(Error::InvalidConfig { .. })));
}

/// A pushed source reports demand while a player watches.
#[tokio::test]
async fn a_pushed_source_sees_demand_while_played() {
    let format = VideoFormat {
        size: video::Size::new(64, 48),
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
    assert!(!sender.demand().get(), "nobody watches yet");
    let player = RemoteBroadcast::local(&broadcast)
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
        size: video::Size::new(64, 48),
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
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let frame = tokio::time::timeout(TIMEOUT, player.video().next())
        .await
        .expect("a source waiting for demand was never watched")
        .expect("the video ended");
    assert_eq!(frame.size(), format.size);
    drop(player);
    feeder.abort();
}

/// A player's frames end when the broadcast closes.
///
/// The video then reports `Ended`.
#[tokio::test]
async fn the_frames_end_when_the_broadcast_closes() {
    let (broadcast, _source) = ladder();
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    let mut frames = player.video();
    tokio::time::timeout(TIMEOUT, frames.next())
        .await
        .expect("a first frame")
        .expect("the video plays");
    broadcast.close();
    tokio::time::timeout(TIMEOUT, async { while frames.next().await.is_some() {} })
        .await
        .expect("the frames went on after the broadcast closed");
    let status = player.status().get();
    assert_eq!(status.video, SlotState::Ended, "{status:?}");
    assert_eq!(status.rendition, None);
}

/// A recording remuxes the broadcast into a container without decoding it.
#[tokio::test]
async fn a_recording_writes_a_container() {
    let (broadcast, _source) = ladder();
    let remote = RemoteBroadcast::local(&broadcast);
    let dir = std::env::temp_dir().join(format!("iroh-live-media-record-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let path = dir.join("out.mp4");
    let file = tokio::fs::File::create(&path)
        .await
        .expect("the file opens");
    let mut recording = remote
        .record(
            file,
            RecordConfig {
                format: RecordFormat::Fmp4,
                rendition: Some("low".into()),
                ..RecordConfig::default()
            },
        )
        .expect("a valid config");
    tokio::time::timeout(TIMEOUT, async {
        while recording.written() < 4_096 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the recording wrote something");
    let written = recording.stop().await.expect("the recording finishes");
    let bytes = std::fs::read(&path).expect("the file is there");
    assert_eq!(bytes.len() as u64, written);
    assert_eq!(
        &bytes[4..8],
        b"ftyp",
        "an MP4 starts with its file type box"
    );
    std::fs::remove_file(&path).ok();
}
