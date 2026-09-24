//! Publishing and playing back in-process, with no transport.
//!
//! Every test here builds a [`LocalBroadcast`] from a generated source and
//! plays it through [`RemoteBroadcast::local`], which is the path the crate
//! promises works without a network: the whole pipeline from source to
//! decoded frame, encoders and decoders included.

use std::time::Duration;

use iroh_live_media::{
    AudioEncoding, AudioOutput, AudioSource, Error, LocalBroadcast, PlayerConfig, RecordConfig,
    RecordFormat, RemoteBroadcast, RenditionMode, SlotState, SwitchError, VideoEncoding,
    VideoFormat, VideoRendition, VideoSource, audio, video,
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
    let recording = remote
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
