//! The client's video path: source, frame-id stamp, publication.
//!
//! The stamp takes the capture instant of every frame, which is where the
//! latency measurement starts. The stamped frames go to a push source, which a
//! [`LocalBroadcast`] encodes.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use iroh_live::media::{
    LocalBroadcast, VideoEncoding, VideoFormat, VideoFrames, VideoRendition, VideoSource,
    video::{FrameRate, Size, capture},
};
use moq_net::bandwidth::Rate;
use n0_future::task::{AbortOnDropHandle, spawn};
use tracing::debug;

use crate::{
    i420,
    stamp::{self, SentLog},
};

/// Where the pictures come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The generated test pattern, which works everywhere.
    TestPattern,
    /// The default camera.
    Camera,
    /// The main display. On Linux this needs X11 or XWayland.
    Screen,
}

impl Source {
    /// Every source, in menu order.
    pub const ALL: [Self; 3] = [Self::TestPattern, Self::Camera, Self::Screen];

    /// Returns the name shown in the menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::TestPattern => "Test pattern",
            Self::Camera => "Camera",
            Self::Screen => "Screen",
        }
    }
}

/// The rendition the client encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// 320x180.
    P180,
    /// 640x360.
    P360,
    /// 1280x720.
    P720,
}

impl Preset {
    /// Every preset, in menu order.
    pub const ALL: [Self; 3] = [Self::P180, Self::P360, Self::P720];

    /// Returns the name shown in the menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::P180 => "180p",
            Self::P360 => "360p",
            Self::P720 => "720p",
        }
    }

    /// Returns the preset's size and default bitrate in kbit/s.
    pub fn size_and_kbps(self) -> (Size, u64) {
        match self {
            Self::P180 => (Size::new(320, 180), 150),
            Self::P360 => (Size::new(640, 360), 500),
            Self::P720 => (Size::new(1280, 720), 1800),
        }
    }
}

/// What the client publishes, and how it is encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSettings {
    /// The source.
    pub source: Source,
    /// The rendition size.
    pub preset: Preset,
    /// The target bitrate in kbit/s.
    pub kbps: u64,
}

impl Default for VideoSettings {
    fn default() -> Self {
        let preset = Preset::P360;
        Self {
            source: Source::TestPattern,
            preset,
            kbps: preset.size_and_kbps().1,
        }
    }
}

/// A running source, stamper and publication. Dropping it stops the source.
#[derive(Debug)]
pub struct Pipeline {
    /// The stamped frames, as sent: the local preview.
    pub local: VideoFrames,
    _task: AbortOnDropHandle<()>,
}

impl Pipeline {
    /// Opens the source and sets it on `broadcast`, replacing its video.
    ///
    /// `wake` runs after every frame, to repaint a preview. Stop an earlier
    /// pipeline first: a camera opens once.
    ///
    /// # Errors
    ///
    /// Fails if the source cannot open or the encoding is refused.
    pub async fn start(
        broadcast: &LocalBroadcast,
        settings: &VideoSettings,
        sent: Arc<SentLog>,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let (size, _) = settings.preset.size_and_kbps();
        let rate = FrameRate::new(30, 1).expect("a valid rate");
        let raw = match settings.source {
            Source::TestPattern => VideoSource::test_pattern(size, rate),
            Source::Camera => open(capture::Source::Camera(None), rate).await?,
            Source::Screen => open(capture::Source::Display(None), rate).await?,
        };
        let (sender, pushed) = VideoSource::push(VideoFormat {
            rate,
            ..raw.format()
        });
        let local = pushed.frames();
        let rendition = VideoRendition {
            size: Some(size),
            bitrate: Some(Rate::from_kbps(settings.kbps)),
            keyframe_interval: Duration::from_secs(1),
            ..VideoRendition::new(settings.preset.label())
        };
        broadcast.set_video(pushed, VideoEncoding::single(rendition))?;

        let task = spawn(async move {
            let mut frames = raw.frames();
            while let Some(frame) = frames.next().await {
                // The latency clock starts here, as the frame leaves the source.
                let id = sent.next_id(Instant::now());
                let stamped = i420::edit(&frame, |data, width, height| {
                    stamp::write(data, width, height, id);
                });
                match stamped {
                    Ok(frame) => {
                        if sender.push(frame).is_err() {
                            break;
                        }
                        wake();
                    }
                    Err(err) => debug!("cannot stamp a frame: {err:#}"),
                }
            }
        });
        Ok(Self {
            local,
            _task: AbortOnDropHandle::new(task),
        })
    }
}

/// Opens a capture device.
async fn open(source: capture::Source, rate: FrameRate) -> Result<VideoSource> {
    let mut config = capture::Config::default();
    config.source = source;
    config.framerate = Some(rate);
    VideoSource::capture(config)
        .await
        .context("cannot open the capture device")
}
