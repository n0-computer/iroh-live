//! Hardware tests against the machine's real cameras.
//!
//! Ignored by default, since CI has no camera. Run them on a machine with one:
//!
//! ```sh
//! cargo test -p moq-ext-sources --all-features --test camera -- --ignored --test-threads 1
//! ```

#![cfg(feature = "capture")]

use std::time::{Duration, Instant};

use moq_ext_sources::VideoSource;
use moq_video as video;

/// How long a reopened camera may take to deliver a frame.
const FRAME_PATIENCE: Duration = Duration::from_secs(10);

/// Opens `source` and waits for a frame, `rounds` times in a row.
///
/// This is how an application behaves when it switches sources.
async fn reopen(source: video::capture::Source, rounds: usize) {
    for round in 0..rounds {
        let started = Instant::now();
        let mut config = video::capture::Config::default();
        config.source = source.clone();
        let camera = VideoSource::capture(config)
            .await
            .unwrap_or_else(|err| panic!("round {round}: the camera did not open: {err:#}"));
        let mut frames = camera.frames();
        let frame = tokio::time::timeout(FRAME_PATIENCE, frames.next())
            .await
            .unwrap_or_else(|_| panic!("round {round}: no frame within {FRAME_PATIENCE:?}"));
        assert!(frame.is_some(), "round {round}: the camera ended");
        eprintln!(
            "round {round}: {} opened and delivered a frame in {:?}",
            camera.format().size,
            started.elapsed()
        );
        drop(frames);
        drop(camera);
    }
}

/// The default camera reopens right after it was dropped, every time.
#[tokio::test]
#[ignore = "needs a camera"]
async fn the_default_camera_reopens_right_after_a_drop() {
    reopen(video::capture::Source::Camera(None), 5).await;
}

/// The first V4L2 camera reopens right after it was dropped, every time.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "needs a V4L2 camera at /dev/video0"]
async fn a_v4l2_camera_reopens_right_after_a_drop() {
    reopen(
        video::capture::Source::Camera(Some("/dev/video0".to_string())),
        5,
    )
    .await;
}
