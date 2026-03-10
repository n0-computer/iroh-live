use std::sync::mpsc::Receiver;

use anyhow::{Context, Result};
use tracing::{debug, info};
use xcap::{Monitor, VideoRecorder};

use crate::format::{PixelFormat, VideoFormat, VideoFrame};
use crate::traits::VideoSource;

#[derive(derive_more::Debug)]
pub struct ScreenCapturer {
    #[debug(skip)]
    pub(crate) _monitor: Monitor,
    pub(crate) width: u32,
    pub(crate) height: u32,
    #[debug(skip)]
    pub(crate) video_recorder: VideoRecorder,
    #[debug(skip)]
    pub(crate) rx: Receiver<xcap::Frame>,
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        self.video_recorder.stop().ok();
    }
}

impl ScreenCapturer {
    pub fn new() -> Result<Self> {
        debug!("initializing screen capturer (xcap)");

        let monitors = Monitor::all().context("Failed to get monitors")?;
        if monitors.is_empty() {
            return Err(anyhow::anyhow!("No monitors available"));
        }
        debug!("available monitors: {monitors:?}");

        let monitor = monitors.into_iter().next().unwrap();
        let width = monitor.width()?;
        let height = monitor.height()?;
        let name = monitor
            .name()
            .unwrap_or_else(|_| "Unknown Monitor".to_string());

        info!(monitor = %name, width, height, "capture start");

        let (video_recorder, rx) = monitor.video_recorder()?;

        Ok(Self {
            _monitor: monitor,
            video_recorder,
            rx,
            width,
            height,
        })
    }
}

impl VideoSource for ScreenCapturer {
    fn name(&self) -> &str {
        "screen"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [self.width, self.height],
        }
    }

    fn start(&mut self) -> Result<()> {
        self.video_recorder.start()?;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.video_recorder.stop()?;
        Ok(())
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        let mut raw_frame = None;
        // We are only interested in the latest frame.
        // Drain the channel to not build up memory.
        while let Ok(next) = self.rx.try_recv() {
            raw_frame = Some(next)
        }
        let raw_frame = match raw_frame {
            Some(frame) => frame,
            None => self
                .rx
                .recv()
                .context("Screen recorder did not produce new frame")?,
        };
        Ok(Some(VideoFrame {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [raw_frame.width, raw_frame.height],
            },
            raw: raw_frame.raw.into(),
        }))
    }
}
