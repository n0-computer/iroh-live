//! Test utilities for moq-media integration tests.
//!
//! Enabled by the `test-util` feature or `#[cfg(test)]`. Provides
//! deterministic video/audio sources and a null audio backend for
//! testing without hardware.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Result;
use n0_future::boxed::BoxFuture;

use crate::{
    format::AudioFormat,
    traits::{AudioSink, AudioSinkHandle, AudioSource, AudioStreamFactory},
};

// ── TestVideoSource ────────────────────────────────────────────────

/// Animated SMPTE test pattern with bouncing ball and beep indicator.
///
/// Re-exported from [`rusty_codecs::test_sources::TestPatternSource`].
/// Produces deterministic frame-index-based timestamps (`index / fps`).
pub type TestVideoSource = rusty_codecs::test_sources::TestPatternSource;

/// Periodic 880 Hz beep (100 ms on / 900 ms off), synchronized with
/// [`TestVideoSource`]'s yellow indicator square.
///
/// Re-exported from [`rusty_codecs::test_sources::TestToneSource`].
pub type TestToneSource = rusty_codecs::test_sources::TestToneSource;

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

// ── SineAudioSource ──────────────────────────────────────────────

/// Audio source that generates a sine wave at a fixed frequency.
///
/// Produces a continuous 440 Hz tone. Each call to `pop_samples` advances the
/// phase, so concatenated buffers form a seamless waveform.
#[derive(Clone, Debug)]
pub struct SineAudioSource {
    format: AudioFormat,
    phase: f32,
    frequency: f32,
}

impl SineAudioSource {
    /// Creates a sine source at 440 Hz with the given format.
    pub fn new(format: AudioFormat) -> Self {
        Self {
            format,
            phase: 0.0,
            frequency: 440.0,
        }
    }

    /// Sets the frequency in Hz.
    pub fn with_frequency(mut self, hz: f32) -> Self {
        self.frequency = hz;
        self
    }
}

impl AudioSource for SineAudioSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>> {
        let channels = self.format.channel_count as usize;
        let frames = buf.len() / channels;
        let phase_inc = self.frequency / self.format.sample_rate as f32;

        for i in 0..frames {
            let sample = (2.0 * std::f32::consts::PI * self.phase).sin() * 0.5;
            for ch in 0..channels {
                buf[i * channels + ch] = sample;
            }
            self.phase += phase_inc;
            // Keep phase in [0, 1) to avoid floating-point drift.
            self.phase -= self.phase.floor();
        }

        Ok(Some(frames))
    }
}

// ── CapturingAudioBackend ────────────────────────────────────────

/// Shared buffer of captured audio samples.
///
/// Retrieve with [`CapturingAudioBackend::captured_samples`] after the
/// pipeline has run.
pub type CapturedSamples = Arc<Mutex<Vec<f32>>>;

/// Audio backend that generates a sine wave on input and captures all output
/// samples into a shared buffer.
///
/// Use this in integration tests to verify that audio data actually flows
/// through the encode→transport→decode pipeline. After running the pipeline,
/// call [`captured_samples`](Self::captured_samples) and assert that the
/// buffer is non-empty and contains non-silent audio.
#[derive(Debug, Clone)]
pub struct CapturingAudioBackend {
    captured: CapturedSamples,
}

impl CapturingAudioBackend {
    /// Creates a new capturing backend with an empty sample buffer.
    pub fn new() -> Self {
        Self {
            captured: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns a handle to the captured output samples.
    ///
    /// The returned `Arc<Mutex<Vec<f32>>>` accumulates all samples pushed to
    /// any output sink created by this backend. Read it after the pipeline
    /// shuts down to verify audio data arrived.
    pub fn captured_samples(&self) -> CapturedSamples {
        self.captured.clone()
    }
}

impl Default for CapturingAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioStreamFactory for CapturingAudioBackend {
    fn create_input(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSource>>> {
        let source = SineAudioSource::new(format);
        Box::pin(async move { Ok(Box::new(source) as Box<dyn AudioSource>) })
    }

    fn create_output(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSink>>> {
        let sink = CapturingAudioSink {
            format,
            paused: Arc::new(AtomicBool::new(false)),
            captured: self.captured.clone(),
        };
        Box::pin(async move { Ok(Box::new(sink) as Box<dyn AudioSink>) })
    }
}

struct CapturingAudioSink {
    format: AudioFormat,
    paused: Arc<AtomicBool>,
    captured: CapturedSamples,
}

impl AudioSinkHandle for CapturingAudioSink {
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

impl AudioSink for CapturingAudioSink {
    fn format(&self) -> Result<AudioFormat> {
        Ok(self.format)
    }

    fn push_samples(&mut self, buf: &[f32]) -> Result<()> {
        if let Ok(mut captured) = self.captured.lock() {
            captured.extend_from_slice(buf);
        }
        Ok(())
    }

    fn handle(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(NullAudioSinkHandle {
            paused: self.paused.clone(),
        })
    }
}

// ── TimestampingAudioBackend ──────────────────────────────────────

/// Wall-clock instants when audio beep onsets are detected.
pub type BeepTimestamps = Arc<Mutex<Vec<std::time::Instant>>>;

/// Audio backend that detects beep onsets in decoded audio and records
/// their wall-clock timestamps. Used by A/V sync tests to measure the
/// time delta between video flash frames and audio beep arrival.
///
/// Beep detection uses a simple energy threshold: when the RMS of a
/// 10 ms chunk exceeds 0.05 after a quiet period, that is an onset.
#[derive(Debug, Clone)]
pub struct TimestampingAudioBackend {
    beep_timestamps: BeepTimestamps,
}

impl Default for TimestampingAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TimestampingAudioBackend {
    /// Creates a new backend with an empty timestamp buffer.
    pub fn new() -> Self {
        Self {
            beep_timestamps: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns a handle to the collected beep onset timestamps.
    pub fn beep_timestamps(&self) -> BeepTimestamps {
        self.beep_timestamps.clone()
    }
}

impl AudioStreamFactory for TimestampingAudioBackend {
    fn create_input(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSource>>> {
        let source = TestToneSource::new();
        // TestToneSource is mono 48 kHz; ignore the requested format since
        // the publish side controls the source format.
        let _ = format;
        Box::pin(async move { Ok(Box::new(source) as Box<dyn AudioSource>) })
    }

    fn create_output(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSink>>> {
        let sink = TimestampingAudioSink {
            format,
            paused: Arc::new(AtomicBool::new(false)),
            beep_timestamps: self.beep_timestamps.clone(),
            was_silent: true,
        };
        Box::pin(async move { Ok(Box::new(sink) as Box<dyn AudioSink>) })
    }
}

struct TimestampingAudioSink {
    format: AudioFormat,
    paused: Arc<AtomicBool>,
    beep_timestamps: BeepTimestamps,
    /// Tracks whether the previous chunk was below the beep threshold,
    /// so we only record the onset (rising edge), not every loud chunk.
    was_silent: bool,
}

/// RMS threshold for beep detection. The TestToneSource produces
/// samples at amplitude 0.3, so RMS of a beep chunk is ~0.21.
/// Silence is 0.0. Threshold at 0.05 catches the onset reliably.
const BEEP_RMS_THRESHOLD: f32 = 0.05;

impl AudioSinkHandle for TimestampingAudioSink {
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

impl AudioSink for TimestampingAudioSink {
    fn format(&self) -> Result<AudioFormat> {
        Ok(self.format)
    }

    fn push_samples(&mut self, buf: &[f32]) -> Result<()> {
        // Compute RMS of this chunk.
        if buf.is_empty() {
            return Ok(());
        }
        let rms = (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt();

        let is_loud = rms > BEEP_RMS_THRESHOLD;
        if is_loud && self.was_silent {
            // Rising edge: beep onset detected.
            if let Ok(mut ts) = self.beep_timestamps.lock() {
                ts.push(std::time::Instant::now());
            }
        }
        self.was_silent = !is_loud;
        Ok(())
    }

    fn handle(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(NullAudioSinkHandle {
            paused: self.paused.clone(),
        })
    }
}
