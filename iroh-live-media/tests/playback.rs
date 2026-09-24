//! Publishing and playing back in-process, with no transport.
//!
//! Every test here builds a [`LocalBroadcast`] from a generated source and
//! plays it through [`RemoteBroadcast::local`], which is the path the crate
//! promises works without a network: the whole pipeline from source to
//! decoded frame, encoders and decoders included.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh_live_media::{
    AudioEncoding, AudioOutput, AudioSource, Bitrate, Error, LocalBroadcast, NetworkSample,
    PlayerConfig, RecordConfig, RecordFormat, RemoteBroadcast, RenditionMode, SlotState,
    SwitchError, VideoEncoding, VideoFormat, VideoRendition, VideoSource, audio, video,
};
use n0_watcher::Watcher as _;

/// Generous: software encoding and decoding in a debug build share the
/// machine with the rest of the suite.
const TIMEOUT: Duration = Duration::from_secs(30);

fn fps(n: u32) -> video::Rate {
    video::Rate::new(n, 1).expect("a valid rate")
}

/// A two-rung ladder of the test pattern.
fn ladder() -> (LocalBroadcast, VideoSource) {
    let source = VideoSource::test_pattern(video::Size::new(640, 360), fps(30));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source.clone(),
            VideoEncoding::ladder([
                VideoRendition::new("high").with_size(video::Size::new(640, 360)),
                VideoRendition::new("low").with_size(video::Size::new(320, 180)),
            ])
            .with_prefer_hardware(false),
        )
        .expect("a valid ladder");
    (broadcast, source)
}

/// Waits until `player`'s frames have a picture of `size`.
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
        .play(PlayerConfig::default().with_rendition(RenditionMode::pinned("low")))
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
    // The timeline behind the overlay's TIME panel has the pictures shown,
    // each held for the clock rather than presented before it decoded.
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

/// A player started after the catalog arrived used to wait for something to
/// change before it chose a rendition, which with no audio and no network
/// signals never happened: `irl watch` and every patchbay test, which wait for
/// the catalog before playing, got no picture.
#[tokio::test]
async fn a_player_started_after_the_catalog_plays() {
    let (broadcast, _source) = ladder();
    let remote = RemoteBroadcast::local(&broadcast);
    let mut catalog = remote.catalog();
    tokio::time::timeout(TIMEOUT, async {
        while catalog
            .get()
            .is_none_or(|catalog| catalog.video.renditions.len() < 2)
        {
            catalog.updated().await.expect("the broadcast is alive");
        }
    })
    .await
    .expect("the catalog arrives");
    let player = remote
        .play(PlayerConfig::default().with_rendition(RenditionMode::pinned("low")))
        .expect("valid");
    tokio::time::timeout(TIMEOUT, player.video().next())
        .await
        .expect("a player of a described broadcast never chose a rendition")
        .expect("the video plays");
}

/// A shortfall that has to hold before the player steps down used to be
/// taken back on the next pass: the selector weighed its target against the
/// rendition on screen, which is still the old one while the replacement warms
/// up, so the hold restarted and the old rendition was asked for again. On a
/// real link no bandwidth downgrade ever landed.
#[tokio::test]
async fn a_held_shortfall_moves_the_player_down() {
    let source = VideoSource::test_pattern(video::Size::new(640, 360), fps(30));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            VideoEncoding::ladder([
                VideoRendition::new("high")
                    .with_size(video::Size::new(640, 360))
                    .with_bitrate(Bitrate::from_bps(2_000_000)),
                VideoRendition::new("low")
                    .with_size(video::Size::new(320, 180))
                    .with_bitrate(Bitrate::from_bps(200_000)),
            ])
            .with_prefer_hardware(false),
        )
        .expect("a valid ladder");
    let sample = Arc::new(Mutex::new(
        NetworkSample::default().with_delivery(Bitrate::from_bps(10_000_000)),
    ));
    let reader = sample.clone();
    let player = RemoteBroadcast::local(&broadcast)
        .with_network(move || *reader.lock().expect("poisoned"))
        .play(PlayerConfig::default())
        .expect("valid");
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("high"))
        .await
        .expect("in time")
        .expect("a healthy link plays the top rendition");
    // Room for `low` and not for `high`, without any loss: only the bound's
    // hold stands between the shortfall and the switch.
    *sample.lock().expect("poisoned") =
        NetworkSample::default().with_delivery(Bitrate::from_bps(300_000));
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("low"))
        .await
        .expect("the downgrade never landed")
        .expect("the switch to low lands");
}

/// R12: two players of one broadcast used to share one playout clock and
/// one policy, so a second view overwrote the first. Each player now owns
/// its own.
#[tokio::test]
async fn two_players_of_one_broadcast_do_not_interfere() {
    let (broadcast, _source) = ladder();
    let remote = RemoteBroadcast::local(&broadcast);
    let high = remote
        .play(PlayerConfig::default().with_rendition(RenditionMode::pinned("high")))
        .expect("valid");
    let low = remote
        .play(PlayerConfig::default().with_rendition(RenditionMode::pinned("low")))
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
        .play(PlayerConfig::default().with_rendition(RenditionMode::pinned("high")))
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

/// A pin to a rendition that is not there falls back to what automatic
/// selection would play, and says why.
#[tokio::test]
async fn a_pin_that_cannot_be_honoured_falls_back_and_says_why() {
    let (broadcast, _source) = ladder();
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default().with_rendition(RenditionMode::pinned("4k")))
        .expect("valid");
    let mut status = player.status();
    let fell_back = tokio::time::timeout(TIMEOUT, async {
        loop {
            let current = status.get();
            if current.rendition.is_some() && current.switch_error.is_some() {
                return current;
            }
            status.updated().await.expect("the player keeps running");
        }
    })
    .await
    .expect("the player fell back");
    assert_eq!(fell_back.rendition.as_deref(), Some("high"));
}

/// C2: a decoder that failed on its first open used to leave the player
/// `Ended`, with no error and nothing else tried. It is a failed switch, and
/// the video says so.
#[tokio::test]
async fn a_decoder_that_will_not_open_fails_the_video() {
    let (broadcast, _source) = ladder();
    let player = RemoteBroadcast::local(&broadcast)
        .play(
            PlayerConfig::default()
                .with_decoder(video::decode::Kind::Named("no-such-decoder".to_string())),
        )
        .expect("valid");
    let result = tokio::time::timeout(TIMEOUT, player.wait_for_rendition("high"))
        .await
        .expect("the wait ends");
    assert!(
        matches!(result, Err(SwitchError::Failed { .. })),
        "{result:?}"
    );
    let mut status = player.status();
    let failed = tokio::time::timeout(TIMEOUT, async {
        loop {
            let current = status.get();
            if matches!(current.video, SlotState::Failed(_)) {
                return current;
            }
            status.updated().await.expect("the player keeps running");
        }
    })
    .await
    .expect("the video shows the failure");
    assert!(failed.rendition.is_none());
    assert!(failed.switch_error.is_some());
}

/// A first decoder that failed is tried again once its backoff is over, even
/// when the broadcast has no other rendition to step to: nothing changed in
/// what the selector wanted, which used to mean nothing was asked for again.
#[tokio::test]
async fn a_failed_first_decoder_is_tried_again() {
    let source = VideoSource::test_pattern(video::Size::new(320, 180), fps(30));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            VideoEncoding::single(VideoRendition::new("video")).with_prefer_hardware(false),
        )
        .expect("valid");
    let player = RemoteBroadcast::local(&broadcast)
        .play(
            PlayerConfig::default()
                .with_decoder(video::decode::Kind::Named("no-such-decoder".to_string())),
        )
        .expect("valid");
    let mut status = player.status();
    let first = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(error) = status.get().switch_error {
                return error;
            }
            status.updated().await.expect("the player keeps running");
        }
    })
    .await
    .expect("the first open fails");
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Some(error) = status.get().switch_error
                && !std::sync::Arc::ptr_eq(&error, &first)
            {
                return;
            }
            status.updated().await.expect("the player keeps running");
        }
    })
    .await
    .expect("the decoder was never tried again");
}

/// S2 and N4: a decoder change that failed used to exclude the rendition
/// playing, and once that was fixed the broken decoder still stayed the one
/// asked for, so the next switch opened under it, failed, and walked the ladder
/// down one failure at a time. The player goes back to the decoder that works,
/// and the next switch lands under it.
#[tokio::test]
async fn a_failed_decoder_change_falls_back_to_the_one_that_works() {
    let source = VideoSource::test_pattern(video::Size::new(640, 360), fps(30));
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            VideoEncoding::ladder([
                VideoRendition::new("high")
                    .with_size(video::Size::new(640, 360))
                    .with_bitrate(Bitrate::from_bps(2_000_000)),
                VideoRendition::new("low")
                    .with_size(video::Size::new(320, 180))
                    .with_bitrate(Bitrate::from_bps(200_000)),
            ])
            .with_prefer_hardware(false),
        )
        .expect("a valid ladder");
    let sample = Arc::new(Mutex::new(
        NetworkSample::default().with_delivery(Bitrate::from_bps(10_000_000)),
    ));
    let reader = sample.clone();
    let player = RemoteBroadcast::local(&broadcast)
        .with_network(move || *reader.lock().expect("poisoned"))
        .play(PlayerConfig::default())
        .expect("valid");
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("high"))
        .await
        .expect("in time")
        .expect("the top rendition plays");
    let working = player.status().get().decoder;

    player.set_decoder(video::decode::Kind::Named("no-such-decoder".to_string()));
    let mut status = player.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get().switch_error.is_none() {
            status.updated().await.expect("the player keeps running");
        }
    })
    .await
    .expect("the failed change shows in the status");
    let current = status.get();
    assert_eq!(current.rendition.as_deref(), Some("high"), "{current:?}");
    assert_eq!(current.video, SlotState::Running);

    // The link narrows: the switch to `low` opens under the decoder that
    // works, where under the broken one it would fail.
    *sample.lock().expect("poisoned") =
        NetworkSample::default().with_delivery(Bitrate::from_bps(300_000));
    tokio::time::timeout(TIMEOUT, player.wait_for_rendition("low"))
        .await
        .expect("the switch never landed")
        .expect("the switch to low opened under the decoder that works");
    assert_eq!(player.status().get().decoder, working);
}

/// One rendition of the test pattern at `size`, called `video`.
fn single(size: video::Size) -> LocalBroadcast {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            VideoSource::test_pattern(size, fps(30)),
            VideoEncoding::single(VideoRendition::new("video").with_size(size))
                .with_prefer_hardware(false),
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

/// A player following a path through a route table keeps playing when the
/// route it plays through goes and another serves the path.
///
/// The transport ends a broadcast when the session carrying it closes, rather
/// than failing over inside it, so `from_origin` asks for the path again, and
/// the player takes the new broadcast as a switch: the picture goes on and the
/// video is never reported ended.
#[tokio::test]
async fn a_player_keeps_playing_when_its_route_goes() {
    let (origin, driver) = moq_net::origin::Producer::new(Default::default());
    let driver = tokio::spawn(moq_net::time::run(driver));
    // The same broadcast twice, as a direct peer and a relay would carry it:
    // the direct route is the cheaper one and serves first.
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

/// N2: a publisher that replaced its video used to leave the player's video
/// ended when the old track's end arrived after the new catalog, since only a
/// later catalog update would revive it. The player waits for what follows a
/// clean end, and asks again after a backoff whatever the order. In-process
/// the order is usually the kind one, so this covers the recovery and the
/// status on the way; `select::tests::a_track_that_ended_is_asked_for_again`
/// forces the other order.
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
            VideoEncoding::single(VideoRendition::new("video").with_size(size))
                .with_prefer_hardware(false),
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
    let mut status = player.status();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let current = status.get();
            if current.video == SlotState::Off && current.rendition.is_none() {
                return;
            }
            status.updated().await.expect("the player keeps running");
        }
    })
    .await
    .expect("video turned off");
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
    let config = PlayerConfig::default().with_latency(iroh_live_media::Latency::range(
        Duration::from_millis(500),
        Duration::from_millis(100),
    ));
    let result = RemoteBroadcast::local(&broadcast).play(config);
    assert!(matches!(result, Err(Error::InvalidConfig { .. })));
}

/// Audio plays through a null output, which still runs the audio path and
/// the clock off it.
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
        .play(PlayerConfig::default().with_audio(&AudioOutput::null()))
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

/// S7: audio a publisher replaced used to stay `Ended` when the new catalog
/// arrived before the old track's end, since a catalog update was ignored
/// while the old track still read. The player looks at the catalog again
/// after an end, so the new track plays whichever arrived first.
#[tokio::test]
async fn audio_comes_back_after_the_publisher_replaces_it() {
    let broadcast = LocalBroadcast::new();
    let tone = || AudioSource::tone(440.0, audio::Layout::Mono);
    broadcast
        .set_audio(tone(), AudioEncoding::voice())
        .expect("a valid encoding");
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default().with_audio(&AudioOutput::null()))
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
    // The stats start over with every track, so a count below the one the
    // first track had reached means it ended; polled, since the status can
    // pass through `Ended` faster than a watcher looks.
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

/// The broadcast's status follows its slots: starting, running, and back to
/// off once cleared.
#[tokio::test]
async fn the_publish_status_follows_the_video_slot() {
    let (broadcast, _source) = ladder();
    let mut status = broadcast.status();
    tokio::time::timeout(TIMEOUT, async {
        while status.get().video != SlotState::Running {
            status.updated().await.expect("the broadcast is alive");
        }
    })
    .await
    .expect("the video slot started");
    assert_eq!(
        status.get().renditions.keys().collect::<Vec<_>>(),
        ["high", "low"]
    );
    broadcast.clear_video();
    assert_eq!(status.get().video, SlotState::Off);
    assert!(status.get().renditions.is_empty());
}

/// A pushed source that fails says why in the broadcast's status, rather
/// than in a log line from a task that retries forever (R08).
#[tokio::test]
async fn a_source_that_fails_shows_in_the_status() {
    let format = VideoFormat::new(video::Size::new(64, 48), fps(30));
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
    let mut status = broadcast.status();
    let failed = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let SlotState::Failed(err) = status.get().video {
                return err;
            }
            status.updated().await.expect("the broadcast is alive");
        }
    })
    .await
    .expect("the failure was reported");
    assert!(format!("{failed:#}").contains("caught fire"), "{failed:#}");
}

#[tokio::test]
async fn an_empty_ladder_is_refused_at_once() {
    let source = VideoSource::test_pattern(video::Size::new(64, 48), fps(30));
    let broadcast = LocalBroadcast::new();
    let result = broadcast.set_video(source, VideoEncoding::ladder([]));
    assert!(matches!(result, Err(Error::InvalidConfig { .. })));
}

/// A pushed camera reports demand while someone watches, so an application
/// can idle it when nobody does.
#[tokio::test]
async fn a_pushed_source_sees_demand_while_played() {
    let format = VideoFormat::new(video::Size::new(64, 48), fps(30));
    let (sender, source) = VideoSource::push(format);
    let feeder = tokio::spawn({
        let sender = sender.clone();
        async move {
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
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            VideoEncoding::single(VideoRendition::new("video")).with_prefer_hardware(false),
        )
        .expect("valid");
    let mut demand = sender.demand();
    assert!(!demand.get(), "nobody watches yet");
    let player = RemoteBroadcast::local(&broadcast)
        .play(PlayerConfig::default())
        .expect("valid");
    tokio::time::timeout(TIMEOUT, async {
        while !demand.get() {
            demand.updated().await.expect("the source is alive");
        }
    })
    .await
    .expect("demand arrived with the player");
    drop(player);
    feeder.abort();
}

/// C1: a producer that pushes only while somebody watches, as a phone camera
/// does, used to be never watched: the broadcast waited for a first frame
/// before it advertised a track, and nothing could subscribe to a track that
/// did not exist. The track is created from the source's format.
#[tokio::test]
async fn a_source_that_waits_for_demand_is_played() {
    let format = VideoFormat::new(video::Size::new(64, 48), fps(30));
    let (sender, source) = VideoSource::push(format);
    let feeder = tokio::spawn({
        let sender = sender.clone();
        async move {
            let mut demand = sender.demand();
            while !demand.get() {
                if demand.updated().await.is_err() {
                    return;
                }
            }
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
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            source,
            VideoEncoding::single(VideoRendition::new("video")).with_prefer_hardware(false),
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

/// S9: a player's frames used to end only when the player was dropped, so a
/// reader looping on `next()` waited forever once the publisher had gone.
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

/// A recording remuxes what the broadcast carries into a container without
/// decoding it.
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
            RecordConfig::default()
                .with_format(RecordFormat::Fmp4)
                .with_rendition("low"),
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
