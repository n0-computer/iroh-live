//! Switching the source of a published video while a player plays it.
//!
//! An application switches sources in one of three ways. It pushes the frames
//! of whatever source is selected into one pushed source, as a GPU application
//! does, it swaps the input of a `VideoSource::switchable` source, or it calls
//! `LocalBroadcast::set_video` with the new source. Either way the player must
//! go on with the new pictures within a keyframe or so.
//! The helpers in `moq_ext_player::test_util` explain how a switch is timed.

use std::{
    future::ready,
    time::{Duration, Instant},
};

use moq_ext_player::{
    PlayerConfig, RemoteBroadcast, VideoFrames,
    test_util::{
        CAMERA, CAMERA_OPENS, Clock, FIRST_PICTURE_TIMEOUT, Feed, HD, Look, PATTERN, SD,
        SWITCH_TIMEOUT, alternate_feeds, fed, feed, first_published, fps, shows, single,
    },
};
use moq_ext_publisher::{LocalBroadcast, SlotState};
use moq_ext_sources::{VideoFormat, VideoSource};

/// Plays `broadcast` in-process.
fn play(broadcast: &LocalBroadcast) -> (moq_ext_player::Player, VideoFrames) {
    let player = RemoteBroadcast::from_consumer(broadcast.consume())
        .play(PlayerConfig::default())
        .expect("a valid config");
    let frames = player.video_frames();
    (player, frames)
}

/// A GPU application's switch: one pushed source, fed from the camera, then the pattern, and so on.
///
/// The camera's timestamps restart near zero at every open, far behind the
/// pattern's. Its frames are larger and slower, and the rendition scales them.
#[tokio::test]
async fn switching_what_feeds_a_pushed_source_keeps_playing() {
    let (sender, source) = VideoSource::push(VideoFormat {
        size: SD,
        rate: fps(30),
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(source, single(Some(SD)))
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);

    let feeder = feed(sender.clone(), PATTERN);
    shows(
        &mut frames,
        PATTERN.look,
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;
    alternate_feeds(&broadcast, &mut frames, &sender, feeder, 3).await;
}

/// A pushed source that pauses, as when the selected camera fails to open, plays on once fed again.
///
/// The pattern's clock runs on through the pause, so its timestamps jump ahead.
#[tokio::test]
async fn a_pushed_source_plays_on_after_a_pause() {
    let (sender, source) = VideoSource::push(VideoFormat {
        size: SD,
        rate: fps(30),
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(source, single(Some(SD)))
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);

    let feeder = feed(sender.clone(), PATTERN);
    shows(
        &mut frames,
        PATTERN.look,
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;
    drop(feeder);
    // The pause is what the test is about: the source sends nothing for longer
    // than a keyframe interval while its clock runs on.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let start = first_published(&broadcast);
    let _feeder = feed(
        sender.clone(),
        Feed {
            look: Look::flat(SD, 120),
            fps: 30,
            clock: Clock::Pattern,
        },
    );
    shows(
        &mut frames,
        Look::flat(SD, 120),
        start,
        SWITCH_TIMEOUT,
        "after the pause",
    )
    .await;
}

/// `set_video` with the test pattern at another size, a camera, the same size, and back.
#[tokio::test]
async fn replacing_the_video_source_keeps_playing() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            VideoSource::test_pattern(SD, fps(30)).expect("the pattern thread starts"),
            single(None),
        )
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);
    shows(
        &mut frames,
        Look::pattern(SD),
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;

    for round in 0..2 {
        broadcast
            .set_video(
                VideoSource::test_pattern(HD, fps(30)).expect("the pattern thread starts"),
                single(None),
            )
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(HD),
            first_published(&broadcast),
            SWITCH_TIMEOUT,
            &format!("HD pattern in round {round}"),
        )
        .await;

        let (camera, _feeder) = fed(CAMERA);
        broadcast
            .set_video(camera, single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            CAMERA.look,
            first_published(&broadcast),
            SWITCH_TIMEOUT,
            &format!("camera in round {round}"),
        )
        .await;

        // The same size twice in a row, told apart by brightness.
        let (dark, _dark) = fed(PATTERN);
        broadcast
            .set_video(dark, single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            PATTERN.look,
            first_published(&broadcast),
            SWITCH_TIMEOUT,
            &format!("dark SD in round {round}"),
        )
        .await;
        let bright = Feed {
            look: Look::flat(SD, 200),
            fps: 30,
            clock: Clock::Pattern,
        };
        let (source, _bright) = fed(bright);
        broadcast
            .set_video(source, single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            bright.look,
            first_published(&broadcast),
            SWITCH_TIMEOUT,
            &format!("bright SD in round {round}"),
        )
        .await;

        broadcast
            .set_video(
                VideoSource::test_pattern(SD, fps(30)).expect("the pattern thread starts"),
                single(None),
            )
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(SD),
            first_published(&broadcast),
            SWITCH_TIMEOUT,
            &format!("SD pattern in round {round}"),
        )
        .await;
    }
}

/// A source that fails to open, set in between, does not keep the next one from playing.
#[tokio::test]
async fn a_source_that_fails_does_not_stop_the_next() {
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(
            VideoSource::test_pattern(SD, fps(30)).expect("the pattern thread starts"),
            single(None),
        )
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);
    shows(
        &mut frames,
        Look::pattern(SD),
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;

    let format = VideoFormat {
        size: HD,
        rate: fps(30),
    };
    let broken = VideoSource::spawn("broken-camera", format, |_sender| {
        Err(moq_ext_player::Error::device_msg("the camera is busy"))
    })
    .expect("the thread starts");
    broadcast
        .set_video(broken, single(None))
        .expect("a valid encoding");
    let mut status = broadcast.status();
    tokio::time::timeout(
        FIRST_PICTURE_TIMEOUT,
        status.wait_for(|status| matches!(status.video, SlotState::Failed(_))),
    )
    .await
    .expect("the broken source fails in time")
    .expect("the broadcast is alive");

    broadcast
        .set_video(
            VideoSource::test_pattern(HD, fps(30)).expect("the pattern thread starts"),
            single(None),
        )
        .expect("a valid encoding");
    shows(
        &mut frames,
        Look::pattern(HD),
        first_published(&broadcast),
        SWITCH_TIMEOUT,
        "pattern after the failure",
    )
    .await;
}

/// Prints how long each switch of a pushed source takes, from the switch to the new picture.
///
/// The other tests bound a switch from the new source's first frame. This one
/// measures what a viewer sees, from the moment the application switches.
/// It asserts nothing beyond the switch happening, since the time depends on
/// the machine's load. Run it with `--ignored --nocapture`.
#[tokio::test]
#[ignore = "prints timings, asserts no bound"]
async fn prints_the_switch_latency() {
    let (sender, source) = VideoSource::push(VideoFormat {
        size: SD,
        rate: fps(30),
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(source, single(Some(SD)))
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);
    let mut feeder = feed(sender.clone(), PATTERN);
    shows(
        &mut frames,
        PATTERN.look,
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;
    for round in 0..5 {
        for (name, source) in [("camera", CAMERA), ("pattern", PATTERN)] {
            drop(feeder);
            let switched = Instant::now();
            tokio::time::sleep(CAMERA_OPENS).await;
            feeder = feed(sender.clone(), source);
            let look = Look {
                size: SD,
                ..source.look
            };
            let latency = shows(
                &mut frames,
                look,
                ready(switched),
                FIRST_PICTURE_TIMEOUT,
                &format!("{name} in round {round}"),
            )
            .await;
            eprintln!(
                "switch latency, {name} in round {round}: {latency:?}, \
                 including a {CAMERA_OPENS:?} gap"
            );
        }
    }
}

/// A switchable source swaps its input behind one `set_video`.
///
/// It opens the next source in the background.
#[tokio::test]
async fn a_switchable_source_plays_each_source_it_switches_to() {
    let (switch, source) = VideoSource::switchable(VideoFormat {
        size: SD,
        rate: fps(30),
    });
    let broadcast = LocalBroadcast::new();
    broadcast
        .set_video(source, single(Some(SD)))
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);

    let (pattern, pattern_source) = VideoSource::push(VideoFormat {
        size: SD,
        rate: fps(30),
    });
    let _pattern = feed(pattern, PATTERN);
    switch.replace(pattern_source);
    shows(
        &mut frames,
        PATTERN.look,
        ready(Instant::now()),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;

    // The camera opens in the background, as `VideoSource::capture` would.
    let start = first_published(&broadcast);
    switch.open(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (camera, camera_source) = VideoSource::push(VideoFormat {
            size: HD,
            rate: fps(25),
        });
        // The feed stops with its source, once the switch drops it.
        std::mem::forget(feed(camera, CAMERA));
        Ok(camera_source)
    });
    // The rendition scales the camera's larger frames to its own size.
    let scaled = Look {
        size: SD,
        ..CAMERA.look
    };
    shows(&mut frames, scaled, start, SWITCH_TIMEOUT, "the camera").await;
    assert_eq!(*switch.status().borrow(), SlotState::Running);
}
