//! Helpers for tests that switch the source of a published video while a player plays it.
//!
//! The fed sources stand in for real devices. A camera starts its timestamps
//! near zero whenever it opens, while the test pattern reads one process-wide
//! clock. That difference is what a switch between them crosses.
//!
//! A switch is bounded from the first frame the new source publishes, not
//! from the switch call. Opening an encoder takes a load-dependent time that
//! says nothing about whether the player follows the switch.

use std::{
    cell::Cell,
    future::Future,
    sync::OnceLock,
    time::{Duration, Instant},
};

use moq_ext_publisher::{LocalBroadcast, SlotState, VideoEncoding, VideoRendition};
use moq_ext_sources::{FrameSender, VideoFormat, VideoFrames, VideoSource};
use moq_video::{Frame, FrameRate, I420, Size, Surface};
use n0_future::task::AbortOnDropHandle;
use tracing::info;

/// How long the player may take to show the new source after it publishes its first frame.
///
/// A new source shows at its first keyframe, which the encoder makes of its
/// first frame, so this is the time to decode it and swap the decoder.
pub const SWITCH_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the first picture may take, with every codec and connection still to open.
///
/// Also bounds how long a new source may take to publish its first frame, and
/// how long the frames after a switch may stall, so a broken switch fails
/// rather than hangs.
pub const FIRST_PICTURE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often [`first_published`] reads the publisher's frame counter.
const COUNTER_POLL: Duration = Duration::from_millis(5);

/// How long a camera takes to open, during which a pushed source gets no frames.
pub const CAMERA_OPENS: Duration = Duration::from_millis(200);

/// How many frames the player must show after a switch to count as playing.
const FRAMES_AFTER_SWITCH: usize = 10;

/// The standard-definition size of the test sources.
pub const SD: Size = Size {
    width: 640,
    height: 360,
};

/// The high-definition size of the test sources.
pub const HD: Size = Size {
    width: 1280,
    height: 720,
};

/// Returns a frame rate of `frames_per_second`.
#[must_use]
pub fn fps(frames_per_second: u32) -> FrameRate {
    FrameRate::new(frames_per_second, 1).expect("a valid rate")
}

/// Returns one software-encoded rendition named `video`, at `size` or the source's.
#[must_use]
pub fn single(size: Option<Size>) -> VideoEncoding {
    let mut rendition = VideoRendition {
        size,
        ..VideoRendition::new("video")
    };
    rendition.config.kind = moq_video::encode::Kind::Software;
    rendition.config.join = Duration::from_secs(1);
    VideoEncoding::single(rendition)
}

/// Where a fed source's timestamps start.
#[derive(Debug, Clone, Copy)]
pub enum Clock {
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

/// What a source shows: flat frames of one size and brightness, or a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// The size of the frames.
    pub size: Size,
    /// The luma of every pixel. `None` for a picture that is not flat.
    pub luma: Option<u8>,
}

impl Look {
    /// Returns a flat look of `size` with every pixel at `luma`.
    #[must_use]
    pub const fn flat(size: Size, luma: u8) -> Self {
        Self {
            size,
            luma: Some(luma),
        }
    }

    /// Returns the look of the test pattern at `size`.
    #[must_use]
    pub const fn pattern(size: Size) -> Self {
        Self { size, luma: None }
    }

    /// Returns what `frame` shows, reading five pixels spread over it.
    ///
    /// A picture whose samples differ is not flat, so it reads as a pattern.
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

/// What a fed source shows, at how many frames per second, and on which clock.
#[derive(Debug, Clone, Copy)]
pub struct Feed {
    /// What every frame shows.
    pub look: Look,
    /// Frames per second.
    pub fps: u32,
    /// Where the timestamps start.
    pub clock: Clock,
}

/// A camera at 25 fps: 1280x720, bright, timestamps from zero.
pub const CAMERA: Feed = Feed {
    look: Look::flat(HD, 200),
    fps: 25,
    clock: Clock::Camera,
};

/// The test pattern as a GPU application sends it: 640x360 at 30 fps, dark.
pub const PATTERN: Feed = Feed {
    look: Look::flat(SD, 40),
    fps: 30,
    clock: Clock::Pattern,
};

/// Pushes the flat frames `feed` describes into `sender` until aborted.
pub fn feed(sender: FrameSender<Frame>, feed: Feed) -> AbortOnDropHandle<()> {
    let Feed {
        look,
        fps: rate,
        clock,
    } = feed;
    let task = tokio::spawn(async move {
        let luma = look.luma.expect("a flat look");
        let pixels = look.size.pixels() as usize;
        let mut planes = vec![luma; pixels];
        planes.resize(pixels * 3 / 2, 128);
        let opened = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_secs(1) / rate);
        loop {
            tick.tick().await;
            let image = I420::new(look.size, planes.clone()).expect("valid planes");
            let frame = Frame::new(Surface::I420(image), clock.now(opened));
            if sender.push(frame).is_err() {
                return;
            }
        }
    });
    AbortOnDropHandle::new(task)
}

/// Returns a source that shows what `with` describes, and the task that feeds it.
pub fn fed(with: Feed) -> (VideoSource, AbortOnDropHandle<()>) {
    let (sender, source) = VideoSource::push(VideoFormat {
        size: with.look.size,
        rate: fps(with.fps),
    });
    (source, feed(sender, with))
}

/// Returns the frames and bytes `broadcast` has published on its `video` rendition.
fn published(broadcast: &LocalBroadcast) -> Option<(u64, u64)> {
    let stats = broadcast.stats();
    let track = &stats.renditions.get("video")?.track;
    let sent = stats.sent.get(track).cloned().unwrap_or_default();
    Some((sent.frames, sent.bytes))
}

/// Returns a future that resolves when the source set last publishes its first frame.
///
/// Call it right after the switch: after `set_video`, or before a pushed source
/// is fed again. After `set_video` the counter is read again once the video
/// slot is `Running`, so a frame the old source still had in flight does not
/// count.
pub fn first_published(broadcast: &LocalBroadcast) -> impl Future<Output = Instant> + use<> {
    let broadcast = broadcast.clone();
    let mut status = broadcast.status();
    let mut before = published(&broadcast);
    async move {
        if status.borrow_and_update().video != SlotState::Running {
            status
                .wait_for(|status| status.video == SlotState::Running)
                .await
                .expect("the broadcast is alive");
            before = published(&broadcast);
        }
        while published(&broadcast) == before {
            tokio::time::sleep(COUNTER_POLL).await;
        }
        Instant::now()
    }
}

/// Waits until `frames` shows `look`, then checks the frames after it.
///
/// The first matching frame must come within `bound` of the moment `start`
/// resolves. The next ten frames must all show `look`, in timestamp order.
/// Returns how long the player took after `start`.
///
/// # Panics
///
/// Panics if `start` takes longer than [`FIRST_PICTURE_TIMEOUT`], if the first
/// matching frame misses its bound, or if the frames after it stop, show
/// something else, or go back in time.
pub async fn shows(
    frames: &mut VideoFrames,
    look: Look,
    start: impl Future<Output = Instant>,
    bound: Duration,
    what: &str,
) -> Duration {
    let mut last = None;
    let started = Cell::new(None);
    let outcome = {
        let found = async {
            loop {
                let frame = frames.next().await?;
                let seen = Look::of(&frame);
                if look.matches(seen) {
                    return Some((Instant::now(), frame.timestamp));
                }
                last = Some(seen);
            }
        };
        let deadline = async {
            let at = start.await;
            started.set(Some(at));
            tokio::time::sleep_until((at + bound).into()).await;
        };
        tokio::pin!(found, deadline);
        tokio::time::timeout(FIRST_PICTURE_TIMEOUT + bound, async {
            tokio::select! {
                biased;
                found = &mut found => Ok(found),
                () = &mut deadline => Err(()),
            }
        })
        .await
    };
    let (shown_at, mut previous) = match outcome {
        Err(_) if started.get().is_none() => {
            panic!("{what}: the new source published nothing within {FIRST_PICTURE_TIMEOUT:?}")
        }
        Err(_) | Ok(Err(())) => {
            panic!("{what}: no {look:?} within {bound:?} of the first frame, last saw {last:?}")
        }
        Ok(Ok(None)) => panic!("{what}: the video ended while waiting for {look:?}"),
        Ok(Ok(Some(shown))) => shown,
    };
    // Bounded only so that a stall fails rather than hangs. What is checked is
    // that the new pictures keep coming, in order.
    for index in 0..FRAMES_AFTER_SWITCH {
        let frame = tokio::time::timeout(FIRST_PICTURE_TIMEOUT, frames.next())
            .await
            .unwrap_or_else(|_| panic!("{what}: frame {index} after the switch never came"))
            .unwrap_or_else(|| panic!("{what}: the video ended {index} frames after the switch"));
        let seen = Look::of(&frame);
        assert!(
            look.matches(seen),
            "{what}: frame {index} showed {seen:?}, not {look:?}"
        );
        assert!(
            frame.timestamp > previous,
            "{what}: frame {index} at {:?} is not after {previous:?}",
            frame.timestamp
        );
        previous = frame.timestamp;
    }
    // The counter is read every few milliseconds, so a fast player can show
    // the frame before its publication is seen.
    let latency = started
        .get()
        .map_or(Duration::ZERO, |at| shown_at.saturating_duration_since(at));
    info!(what, ?latency, ?look, "switch shown");
    latency
}

/// Feeds `sender` the camera and the pattern in turn, and checks that `frames` follows.
///
/// `feeder` is the task that feeds `sender` now, and stops at the first switch.
/// Each switch leaves a [`CAMERA_OPENS`] gap with no frames, as a camera
/// takes to open, and the rendition of `broadcast` scales every source to
/// [`SD`].
pub async fn alternate_feeds(
    broadcast: &LocalBroadcast,
    frames: &mut VideoFrames,
    sender: &FrameSender<Frame>,
    mut feeder: AbortOnDropHandle<()>,
    rounds: usize,
) {
    for round in 0..rounds {
        for (name, source) in [("camera", CAMERA), ("pattern", PATTERN)] {
            drop(feeder);
            // The gap is part of the switch under test, not a wait.
            tokio::time::sleep(CAMERA_OPENS).await;
            let start = first_published(broadcast);
            feeder = feed(sender.clone(), source);
            let look = Look {
                size: SD,
                ..source.look
            };
            let what = format!("{name} in round {round}");
            shows(frames, look, start, SWITCH_TIMEOUT, &what).await;
        }
    }
}
