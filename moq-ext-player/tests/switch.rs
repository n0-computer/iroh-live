//! Switching the source of a published video while a player plays it.
//!
//! An application switches sources in one of two ways. It pushes the frames of
//! whatever source is selected into one pushed source, as an application does,
//! or it calls `LocalBroadcast::set_video` with the new source. Either way the
//! player must go on with the new pictures within a keyframe or so.
//!
//! The fed sources stand in for real devices. A camera starts its timestamps
//! near zero whenever it opens, while the test pattern reads one process-wide
//! clock. That difference is what a switch between them crosses.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use moq_ext_player::{PlayerConfig, RemoteBroadcast, VideoFrames};
use moq_ext_publisher::{LocalBroadcast, VideoEncoding, VideoRendition};
use moq_ext_sources::{FrameSender, VideoFormat, VideoSource};
use moq_video::{FrameRate, Size};
use n0_future::task::AbortOnDropHandle;

/// How long a switch may take until the player shows the new source.
///
/// A new source shows at its first keyframe, so this is mostly the time to
/// open an encoder and a decoder in a debug build.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the first picture may take, with every codec still to open.
const FIRST_PICTURE_TIMEOUT: Duration = Duration::from_secs(30);

/// How many frames the player must show after a switch to count as playing.
const FRAMES_AFTER_SWITCH: usize = 10;

/// The standard-definition size of the test sources.
const SD: Size = Size {
    width: 640,
    height: 360,
};
/// The high-definition size of the test sources.
const HD: Size = Size {
    width: 1280,
    height: 720,
};

/// Returns a frame rate of `frames_per_second`.
fn fps(frames_per_second: u32) -> FrameRate {
    FrameRate::new(frames_per_second, 1).expect("a valid rate")
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
    fn now(self, opened: Instant) -> moq_net::Timestamp {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let micros = match self {
            Self::Camera => opened.elapsed().as_micros(),
            Self::Pattern => EPOCH.get_or_init(Instant::now).elapsed().as_micros() + 10_000_000,
        };
        moq_net::Timestamp::from_micros(micros as u64).expect("in range")
    }
}

/// What a source shows: flat frames of one size and brightness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    size: Size,
    /// The luma of every pixel. `None` for the test pattern, which is not flat.
    luma: Option<u8>,
}

impl Look {
    const fn flat(size: Size, luma: u8) -> Self {
        Self {
            size,
            luma: Some(luma),
        }
    }

    const fn pattern(size: Size) -> Self {
        Self { size, luma: None }
    }

    /// Returns what `frame` shows, reading five pixels spread over it.
    ///
    /// A picture whose samples differ is not flat, so it reads as a pattern.
    fn of(frame: &moq_video::Frame) -> Self {
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
/// The test pattern as the demo sends it: 640x360 at 30 fps, dark.
const PATTERN: (Look, u32, Clock) = (Look::flat(SD, 40), 30, Clock::Pattern);

/// Pushes flat `look` frames at `rate` into `sender` until aborted.
fn feed(
    sender: FrameSender<moq_video::Frame>,
    (look, rate, clock): (Look, u32, Clock),
) -> AbortOnDropHandle<()> {
    let task = tokio::spawn(async move {
        let size = look.size;
        let luma = look.luma.expect("a flat look");
        let pixels = size.pixels() as usize;
        let mut planes = vec![luma; pixels];
        planes.resize(pixels * 3 / 2, 128);
        let opened = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_secs(1) / rate);
        loop {
            tick.tick().await;
            let image = moq_video::I420::new(size, planes.clone()).expect("valid planes");
            let frame = moq_video::Frame::new(moq_video::Surface::I420(image), clock.now(opened));
            if sender.push(frame).is_err() {
                return;
            }
        }
    });
    AbortOnDropHandle::new(task)
}

/// Returns a source that shows what `feed_with` describes, and the task that feeds it.
fn fed(feed_with: (Look, u32, Clock)) -> (VideoSource, AbortOnDropHandle<()>) {
    let (look, rate, _) = feed_with;
    let (sender, source) = VideoSource::push(VideoFormat {
        size: look.size,
        rate: fps(rate),
    });
    (source, feed(sender, feed_with))
}

/// Waits until `frames` shows `look`, then until it has shown it [`FRAMES_AFTER_SWITCH`] times.
///
/// # Panics
///
/// Panics if the first matching frame takes longer than `bound`, or if the
/// frames after it stop or show something else.
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
    for index in 0..FRAMES_AFTER_SWITCH {
        let frame = tokio::time::timeout(SWITCH_TIMEOUT, frames.next())
            .await
            .unwrap_or_else(|_| panic!("{what}: frame {index} after the switch never came"))
            .unwrap_or_else(|| panic!("{what}: the video ended {index} frames after the switch"));
        let seen = Look::of(&frame);
        assert!(
            look.matches(seen),
            "{what}: frame {index} showed {seen:?}, not {look:?}"
        );
    }
    eprintln!("{what}: showed {look:?} after {switched:?}");
}

/// Plays `broadcast` in-process.
fn play(broadcast: &LocalBroadcast) -> (moq_ext_player::Player, VideoFrames) {
    let player = RemoteBroadcast::from_consumer(broadcast.consume())
        .play(PlayerConfig::default())
        .expect("a valid config");
    let frames = player.video();
    (player, frames)
}

/// The demo's switch: one pushed source, fed from the camera, then the pattern, and so on.
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

    let mut feeder = feed(sender.clone(), PATTERN);
    shows(
        &mut frames,
        PATTERN.0,
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;
    for round in 0..3 {
        for (name, source) in [("camera", CAMERA), ("pattern", PATTERN)] {
            drop(feeder);
            // A camera takes a moment to open, and nothing is pushed meanwhile.
            tokio::time::sleep(Duration::from_millis(200)).await;
            feeder = feed(sender.clone(), source);
            // The rendition scales every source to its own size.
            let look = Look {
                size: SD,
                ..source.0
            };
            shows(
                &mut frames,
                look,
                SWITCH_TIMEOUT,
                &format!("{name} in round {round}"),
            )
            .await;
        }
    }
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
        PATTERN.0,
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;
    drop(feeder);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let _feeder = feed(sender.clone(), (Look::flat(SD, 120), 30, Clock::Pattern));
    shows(
        &mut frames,
        Look::flat(SD, 120),
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
        .set_video(VideoSource::test_pattern(SD, fps(30)), single(None))
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);
    shows(
        &mut frames,
        Look::pattern(SD),
        FIRST_PICTURE_TIMEOUT,
        "first pattern",
    )
    .await;

    for round in 0..2 {
        broadcast
            .set_video(VideoSource::test_pattern(HD, fps(30)), single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(HD),
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
            CAMERA.0,
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
            PATTERN.0,
            SWITCH_TIMEOUT,
            &format!("dark SD in round {round}"),
        )
        .await;
        let bright = (Look::flat(SD, 200), 30, Clock::Pattern);
        let (source, _bright) = fed(bright);
        broadcast
            .set_video(source, single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            bright.0,
            SWITCH_TIMEOUT,
            &format!("bright SD in round {round}"),
        )
        .await;

        broadcast
            .set_video(VideoSource::test_pattern(SD, fps(30)), single(None))
            .expect("a valid encoding");
        shows(
            &mut frames,
            Look::pattern(SD),
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
        .set_video(VideoSource::test_pattern(SD, fps(30)), single(None))
        .expect("a valid encoding");
    let (_player, mut frames) = play(&broadcast);
    shows(
        &mut frames,
        Look::pattern(SD),
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
    tokio::time::sleep(Duration::from_millis(500)).await;

    broadcast
        .set_video(VideoSource::test_pattern(HD, fps(30)), single(None))
        .expect("a valid encoding");
    shows(
        &mut frames,
        Look::pattern(HD),
        SWITCH_TIMEOUT,
        "pattern after the failure",
    )
    .await;
}
