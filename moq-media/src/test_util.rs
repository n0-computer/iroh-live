//! Test utilities for moq-media integration tests.
//!
//! Enabled by the `test-util` feature or `#[cfg(test)]`. Provides
//! deterministic video/audio sources and a null audio backend for
//! testing without hardware.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use n0_future::boxed::BoxFuture;

use crate::format::{AudioFormat, PixelFormat, VideoFormat, VideoFrame};
use crate::traits::{AudioSink, AudioSinkHandle, AudioSource, AudioStreamFactory, VideoSource};
use rusty_codecs::codec::test_util::make_test_pattern;

// ── TestVideoSource ────────────────────────────────────────────────

/// Deterministic video source that produces SMPTE-style test pattern frames.
///
/// Does not sleep — `pop_frame` returns immediately on every call.
/// Frame timestamps increment by `1/fps` each call.
#[derive(Debug)]
pub struct TestVideoSource {
    format: VideoFormat,
    started: bool,
    frame_index: u32,
    fps: f64,
}

impl TestVideoSource {
    /// Creates a new test source with the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [width, height],
            },
            started: false,
            frame_index: 0,
            fps: 30.0,
        }
    }

    /// Sets the frame rate used for timestamp generation.
    pub fn with_fps(mut self, fps: f64) -> Self {
        self.fps = fps;
        self
    }
}

impl VideoSource for TestVideoSource {
    fn name(&self) -> &str {
        "test"
    }

    fn format(&self) -> VideoFormat {
        self.format.clone()
    }

    fn start(&mut self) -> Result<()> {
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.started = false;
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        if !self.started {
            return Ok(None);
        }
        let [w, h] = self.format.dimensions;
        let mut frame = make_test_pattern(w, h, self.frame_index);
        frame.timestamp = Duration::from_secs_f64(self.frame_index as f64 / self.fps);
        self.frame_index += 1;
        Ok(Some(frame))
    }
}

// ── TestAudioSource ────────────────────────────────────────────────

/// Deterministic audio source that produces silence.
///
/// Returns zeroed buffers on every `pop_samples` call.
#[derive(Clone, Debug)]
pub struct TestAudioSource {
    format: AudioFormat,
}

impl TestAudioSource {
    /// Creates a new silent audio source with the given format.
    pub fn new(format: AudioFormat) -> Self {
        Self { format }
    }
}

impl AudioSource for TestAudioSource {
    fn cloned_boxed(&self) -> Box<dyn AudioSource> {
        Box::new(self.clone())
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>> {
        buf.fill(0.0);
        let frames = buf.len() / self.format.channel_count as usize;
        Ok(Some(frames))
    }
}

// ── NullAudioBackend ───────────────────────────────────────────────

/// Audio backend that discards output and produces silence for input.
///
/// Implements [`AudioStreamFactory`] without requiring audio hardware.
#[derive(Debug)]
pub struct NullAudioBackend;

impl AudioStreamFactory for NullAudioBackend {
    fn create_input(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSource>>> {
        let source = TestAudioSource::new(format);
        Box::pin(async move { Ok(Box::new(source) as Box<dyn AudioSource>) })
    }

    fn create_output(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSink>>> {
        let sink = NullAudioSink {
            format,
            paused: Arc::new(AtomicBool::new(false)),
        };
        Box::pin(async move { Ok(Box::new(sink) as Box<dyn AudioSink>) })
    }
}

struct NullAudioSink {
    format: AudioFormat,
    paused: Arc<AtomicBool>,
}

impl AudioSinkHandle for NullAudioSink {
    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(NullAudioSinkHandle {
            paused: self.paused.clone(),
        })
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    fn toggle_pause(&self) {
        let _ = self
            .paused
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(!v));
    }
}

impl AudioSink for NullAudioSink {
    fn format(&self) -> Result<AudioFormat> {
        Ok(self.format)
    }

    fn push_samples(&mut self, _buf: &[f32]) -> Result<()> {
        Ok(())
    }

    fn handle(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(NullAudioSinkHandle {
            paused: self.paused.clone(),
        })
    }
}

struct NullAudioSinkHandle {
    paused: Arc<AtomicBool>,
}

impl AudioSinkHandle for NullAudioSinkHandle {
    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(Self {
            paused: self.paused.clone(),
        })
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    fn toggle_pause(&self) {
        let _ = self
            .paused
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(!v));
    }
}
