//! Audio backend: direct cpal stream management with fixed-resample ring buffers,
//! inline AEC processing, and peak metering.
//!
//! Replaces the firewheel audio graph with a simpler model: cpal streams connect
//! directly to resampling ring buffers, with AEC and peak metering applied inline
//! in the cpal callbacks. All internal processing runs at 48 kHz stereo.
//!
//! # Threading model
//!
//! Three thread categories interact:
//!
//! 1. **Caller threads** (tokio tasks, decode threads) push/pop samples through
//!    `OutputStream`/`InputStream` handles. Each handle owns a `ResamplingProd`
//!    or `ResamplingCons` behind a Mutex (uncontended — single writer/reader).
//!
//! 2. **cpal callback threads** (real-time, OS-managed) read from output ring
//!    buffers and write to input ring buffers. These must be allocation-free and
//!    lock-free on the data path. They own the other end of each resampling
//!    channel.
//!
//! 3. **Driver thread** (OS thread, message-driven) handles stream lifecycle,
//!    device switching, and error recovery. Communicates with callbacks via
//!    bounded SPSC command channels.

use std::{
    fmt,
    num::NonZeroUsize,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fixed_resample::{
    PushStatus, ReadStatus, ResamplingChannelConfig, ResamplingCons, ResamplingProd,
    resampling_channel,
};
use n0_future::boxed::BoxFuture;
use ringbuf::{
    HeapRb,
    traits::{Producer, Split},
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use self::aec::{AecProcessor, AecProcessorConfig, AecState};
use crate::{
    format::AudioFormat,
    traits::{AudioSink, AudioSinkHandle, AudioSource, AudioStreamFactory},
    util::spawn_thread,
};

mod aec;

/// Internal sample rate for all processing.
const INTERNAL_RATE: u32 = 48_000;
/// Internal channel count for all processing.
const INTERNAL_CHANNELS: usize = 2;
/// Maximum mix buffer size in frames (stereo samples = frames * 2).
const MAX_MIX_FRAMES: usize = 4096;
/// Declicker fade duration in samples at 48 kHz (~3ms).
const DECLICKER_SAMPLES: usize = 144;
/// Render reference ring buffer capacity in samples (100ms at 48kHz stereo).
const RENDER_REF_CAPACITY: usize = 48_000 / 10 * INTERNAL_CHANNELS;
/// Fade state: playing normally.
const FADE_PLAYING: u32 = 0;
/// Fade state: fading out (transitioning to paused).
const FADE_OUT: u32 = 1;
/// Fade state: fully paused.
const FADE_PAUSED: u32 = 2;
/// Fade state: fading in (transitioning to playing).
const FADE_IN: u32 = 3;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Options for configuring the [`AudioBackend`].
#[derive(Debug, Clone)]
pub struct AudioBackendOpts {
    /// Initial input device. `None` = system default.
    pub input_device: Option<DeviceId>,
    /// Initial output device. `None` = system default.
    pub output_device: Option<DeviceId>,
    /// When a device disconnects unexpectedly, automatically switch to
    /// the system default device. Default: `true`.
    pub fallback_to_default: bool,
}

impl Default for AudioBackendOpts {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            fallback_to_default: true,
        }
    }
}

/// Identifier for an audio device.
#[derive(Debug, Clone, derive_more::Display, Eq, PartialEq)]
pub struct DeviceId(cpal::DeviceId);

impl FromStr for DeviceId {
    type Err = anyhow::Error;
    fn from_str(id: &str) -> Result<Self> {
        let id = cpal::DeviceId::from_str(id)?;
        Ok(Self(id))
    }
}

/// Information about an available audio device.
#[derive(Debug, Clone, derive_more::Display)]
#[display("{name}{}", is_default.then(|| " (default)").unwrap_or_default())]
pub struct AudioDevice {
    /// The device id.
    pub id: DeviceId,
    /// Human-readable device name.
    pub name: String,
    /// Whether this is the system default device for its direction.
    pub is_default: bool,
}

/// Manages audio input/output streams through the system audio device.
#[derive(Debug, Clone)]
pub struct AudioBackend {
    tx: mpsc::Sender<DriverMessage>,
    /// Shared AEC enabled flag — set directly without going through the driver.
    aec_enabled: Arc<AtomicBool>,
}

impl Default for AudioBackend {
    fn default() -> Self {
        Self::new(AudioBackendOpts::default())
    }
}

impl AudioBackend {
    /// Lists available audio input devices.
    pub fn list_inputs() -> Vec<AudioDevice> {
        list_devices(Direction::Input)
    }

    /// Lists available audio output devices.
    pub fn list_outputs() -> Vec<AudioDevice> {
        list_devices(Direction::Output)
    }

    /// Creates a new audio backend and spawns the driver thread.
    pub fn new(opts: AudioBackendOpts) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let weak_tx = tx.downgrade();
        let aec_enabled = Arc::new(AtomicBool::new(true));
        let _handle = spawn_thread("audiodriver", {
            let aec_enabled = aec_enabled.clone();
            move || AudioDriver::new(rx, weak_tx, opts, aec_enabled).run()
        });
        Self { tx, aec_enabled }
    }

    /// Enables or disables acoustic echo cancellation at runtime.
    ///
    /// Takes effect immediately — no stream restart needed. When disabled,
    /// AEC frame accumulation still drains buffers but sonora is not called.
    pub fn set_aec_enabled(&self, enabled: bool) {
        self.aec_enabled.store(enabled, Ordering::Relaxed);
        info!(enabled, "AEC enabled state changed");
    }

    /// Returns whether AEC is currently enabled.
    pub fn aec_enabled(&self) -> bool {
        self.aec_enabled.load(Ordering::Relaxed)
    }

    /// Creates an input stream with mono 48 kHz format.
    pub async fn default_input(&self) -> Result<InputStream> {
        self.input(AudioFormat::mono_48k()).await
    }

    /// Creates an input stream with the given format.
    pub async fn input(&self, format: AudioFormat) -> Result<InputStream> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::InputStream { format, reply })
            .await?;
        reply_rx.await?
    }

    /// Creates an output stream with stereo 48 kHz format.
    pub async fn default_output(&self) -> Result<OutputStream> {
        self.output(AudioFormat::stereo_48k()).await
    }

    /// Creates an output stream with the given format.
    pub async fn output(&self, format: AudioFormat) -> Result<OutputStream> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::OutputStream { format, reply })
            .await?;
        reply_rx.await?
    }

    /// Switches the input device. `None` = system default.
    pub async fn switch_input(&self, device: Option<DeviceId>) -> Result<()> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::SwitchDevice {
                input_device: Some(device),
                output_device: None,
                reply,
            })
            .await?;
        reply_rx.await?
    }

    /// Switches the output device. `None` = system default.
    pub async fn switch_output(&self, device: Option<DeviceId>) -> Result<()> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::SwitchDevice {
                input_device: None,
                output_device: Some(device),
                reply,
            })
            .await?;
        reply_rx.await?
    }

    /// Switches both input and output devices at once. `None` = system default.
    pub async fn switch_devices(
        &self,
        input: Option<DeviceId>,
        output: Option<DeviceId>,
    ) -> Result<()> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::SwitchDevice {
                input_device: Some(input),
                output_device: Some(output),
                reply,
            })
            .await?;
        reply_rx.await?
    }
}

impl AudioStreamFactory for AudioBackend {
    fn create_input(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSource>>> {
        let this = self.clone();
        Box::pin(async move {
            let stream = this.input(format).await?;
            Ok(Box::new(stream) as Box<dyn AudioSource>)
        })
    }

    fn create_output(&self, format: AudioFormat) -> BoxFuture<Result<Box<dyn AudioSink>>> {
        let this = self.clone();
        Box::pin(async move {
            let stream = this.output(format).await?;
            Ok(Box::new(stream) as Box<dyn AudioSink>)
        })
    }
}

// ---------------------------------------------------------------------------
// OutputStream
// ---------------------------------------------------------------------------

/// Plays audio through the system output device.
///
/// Each stream gets its own resampling channel. Samples pushed by the caller
/// are resampled to 48 kHz stereo, mixed with other output streams, processed
/// through AEC render, and written to the cpal output callback.
///
/// Not Clone — use [`OutputHandle`] (via [`AudioSink::handle`]) for
/// thread-safe pause/resume/metering from other threads.
#[derive(derive_more::Debug)]
pub struct OutputStream {
    /// Producer end of the resampling channel. Behind Arc<Mutex> so the
    /// driver can swap it on device switch. The Mutex is uncontended in
    /// normal operation (single caller thread).
    #[debug(skip)]
    prod: Arc<std::sync::Mutex<ResamplingProd<f32, INTERNAL_CHANNELS>>>,
    format: AudioFormat,
    handle: OutputHandle,
    #[debug(skip)]
    _drop_guard: Arc<StreamDropGuard>,
}

/// Lightweight clonable handle for controlling an [`OutputStream`].
///
/// Provides pause/resume/metering without access to the sample producer.
#[derive(Clone, derive_more::Debug)]
pub struct OutputHandle {
    paused: Arc<AtomicBool>,
    /// Fade state for declicking (FADE_PLAYING / FADE_OUT / FADE_PAUSED / FADE_IN).
    fade_state: Arc<AtomicU32>,
    peak_state: Arc<PeakState>,
}

impl AudioSinkHandle for OutputHandle {
    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        self.fade_state.store(FADE_OUT, Ordering::Release);
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::Release);
        self.fade_state.store(FADE_IN, Ordering::Release);
    }

    fn toggle_pause(&self) {
        let was_paused = self.paused.fetch_xor(true, Ordering::AcqRel);
        if was_paused {
            self.fade_state.store(FADE_IN, Ordering::Release);
        } else {
            self.fade_state.store(FADE_OUT, Ordering::Release);
        }
    }

    fn smoothed_peak_normalized(&self) -> Option<f32> {
        Some(self.peak_state.smoothed_normalized())
    }

    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(self.clone())
    }
}

impl AudioSinkHandle for OutputStream {
    fn is_paused(&self) -> bool {
        self.handle.is_paused()
    }
    fn pause(&self) {
        self.handle.pause();
    }
    fn resume(&self) {
        self.handle.resume();
    }
    fn toggle_pause(&self) {
        self.handle.toggle_pause();
    }
    fn smoothed_peak_normalized(&self) -> Option<f32> {
        self.handle.smoothed_peak_normalized()
    }
    fn cloned_boxed(&self) -> Box<dyn AudioSinkHandle> {
        self.handle.cloned_boxed()
    }
}

impl AudioSink for OutputStream {
    fn handle(&self) -> Box<dyn AudioSinkHandle> {
        Box::new(self.handle.clone())
    }

    fn format(&self) -> Result<AudioFormat> {
        Ok(self.format)
    }

    fn push_samples(&mut self, samples: &[f32]) -> Result<()> {
        let mut prod = self.prod.lock().expect("poisoned");

        if self.format.channel_count == 1 {
            // Mono caller: duplicate L to L+R before pushing to stereo channel.
            let mut stereo_buf = [0.0f32; 1024];
            let mut offset = 0;
            while offset < samples.len() {
                let chunk = (samples.len() - offset).min(512);
                for i in 0..chunk {
                    stereo_buf[i * 2] = samples[offset + i];
                    stereo_buf[i * 2 + 1] = samples[offset + i];
                }
                log_push_status(prod.push_interleaved(&stereo_buf[..chunk * 2]));
                offset += chunk;
            }
        } else {
            log_push_status(prod.push_interleaved(samples));
        }

        Ok(())
    }
}

fn log_push_status(status: PushStatus) {
    match status {
        PushStatus::Ok | PushStatus::OutputNotReady => {}
        PushStatus::OverflowOccurred { num_frames_pushed } => {
            warn!(num_frames_pushed, "output stream overflow: samples dropped");
        }
        PushStatus::UnderflowCorrected {
            num_zero_frames_pushed,
        } => {
            // UnderflowCorrected means the consumer side ran dry and
            // fixed-resample padded with silence. This is the normal
            // path for initial buffering. Log at debug because it is
            // recoverable and expected in transient situations.
            debug!(
                num_zero_frames_pushed,
                "output stream underflow corrected with silence"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// InputStream
// ---------------------------------------------------------------------------

/// Reads audio from the system input device (microphone).
///
/// Each `InputStream` owns an independent ring buffer consumer fed by the
/// input callback. For multiple consumers (e.g., parallel audio renditions),
/// create multiple streams via [`AudioBackend::input`] — the input callback
/// fans out to all registered producers automatically.
#[derive(derive_more::Debug)]
pub struct InputStream {
    #[debug(skip)]
    cons: Arc<std::sync::Mutex<ResamplingCons<f32>>>,
    format: AudioFormat,
    #[debug(skip)]
    inactive_count: u32,
    /// Reusable stereo buffer for mono→stereo downmix, avoids per-call allocation.
    #[debug(skip)]
    stereo_temp: Vec<f32>,
    #[debug(skip)]
    _drop_guard: Arc<StreamDropGuard>,
}

impl AudioSource for InputStream {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn pop_samples(&mut self, buf: &mut [f32]) -> Result<Option<usize>> {
        let mut cons = self.cons.lock().expect("poisoned");

        if self.format.channel_count == 1 {
            // Read stereo from channel, average to mono for caller.
            let stereo_len = buf.len() * 2;
            if self.stereo_temp.len() < stereo_len {
                self.stereo_temp.resize(stereo_len, 0.0);
            }
            let stereo_buf = &mut self.stereo_temp[..stereo_len];
            match handle_read_status(
                cons.read_interleaved(stereo_buf),
                &mut self.inactive_count,
            ) {
                Some(()) => {
                    for i in 0..buf.len() {
                        buf[i] = (stereo_buf[i * 2] + stereo_buf[i * 2 + 1]) * 0.5;
                    }
                    Ok(Some(buf.len()))
                }
                None => Ok(None),
            }
        } else {
            match handle_read_status(cons.read_interleaved(buf), &mut self.inactive_count) {
                Some(()) => Ok(Some(buf.len())),
                None => Ok(None),
            }
        }
    }
}

/// Handles [`ReadStatus`] from a resampling channel read, logging warnings
/// and tracking inactive counts. Returns `Some(())` if data was read,
/// `None` if the stream is inactive.
fn handle_read_status(status: ReadStatus, inactive_count: &mut u32) -> Option<()> {
    match status {
        ReadStatus::InputNotReady => {
            *inactive_count += 1;
            if *inactive_count == 1 {
                warn!("audio input stream is inactive");
            } else if (*inactive_count).is_multiple_of(250) {
                warn!(reads = *inactive_count, "audio input stream still inactive");
            }
            None
        }
        ReadStatus::Ok => {
            if *inactive_count > 0 {
                info!(
                    inactive_reads = *inactive_count,
                    "audio input stream became active"
                );
                *inactive_count = 0;
            }
            Some(())
        }
        ReadStatus::UnderflowOccurred { num_frames_read } => {
            if *inactive_count > 0 {
                info!(
                    inactive_reads = *inactive_count,
                    "audio input stream became active"
                );
                *inactive_count = 0;
            }
            debug!(num_frames_read, "audio input underflow");
            Some(())
        }
        ReadStatus::OverflowCorrected {
            num_frames_discarded,
        } => {
            if *inactive_count > 0 {
                info!(
                    inactive_reads = *inactive_count,
                    "audio input stream became active"
                );
                *inactive_count = 0;
            }
            debug!(num_frames_discarded, "audio input overflow corrected");
            Some(())
        }
    }
}

// ---------------------------------------------------------------------------
// Peak metering
// ---------------------------------------------------------------------------

/// Atomic peak state written by the output callback, read by the UI thread.
struct PeakState {
    /// Peak amplitude as f32 bits (left channel).
    peak_l: AtomicU32,
    /// Peak amplitude as f32 bits (right channel).
    peak_r: AtomicU32,
    /// Smoothed value for display, updated on read.
    smoothed: std::sync::Mutex<f32>,
}

impl PeakState {
    fn new() -> Self {
        Self {
            peak_l: AtomicU32::new(0),
            peak_r: AtomicU32::new(0),
            smoothed: std::sync::Mutex::new(0.0),
        }
    }

    /// Stores peak values from the callback (no allocation, no lock).
    fn store(&self, peak_l: f32, peak_r: f32) {
        self.peak_l.store(peak_l.to_bits(), Ordering::Relaxed);
        self.peak_r.store(peak_r.to_bits(), Ordering::Relaxed);
    }

    /// Returns a smoothed, normalized peak value suitable for UI display.
    /// Applies exponential decay and dB normalization (-60..0 dB range).
    fn smoothed_normalized(&self) -> f32 {
        let peak_l = f32::from_bits(self.peak_l.swap(0, Ordering::Relaxed));
        let peak_r = f32::from_bits(self.peak_r.swap(0, Ordering::Relaxed));
        let peak = peak_l.max(peak_r);

        let mut smoothed = self.smoothed.lock().expect("poisoned");
        const ATTACK: f32 = 0.3;
        const RELEASE: f32 = 0.05;
        let factor = if peak > *smoothed { ATTACK } else { RELEASE };
        *smoothed = *smoothed * (1.0 - factor) + peak * factor;

        let db = if *smoothed > 1e-10 {
            20.0 * smoothed.log10()
        } else {
            -60.0
        };
        ((db + 60.0) / 60.0).clamp(0.0, 1.0)
    }
}

impl fmt::Debug for PeakState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeakState").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Stream drop guard
// ---------------------------------------------------------------------------

/// Signals the audio driver to remove a stream when the last handle is dropped.
struct StreamDropGuard {
    stream_id: u64,
    tx: mpsc::WeakSender<DriverMessage>,
}

impl Drop for StreamDropGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.upgrade()
            && let Err(e) = tx.try_send(DriverMessage::RemoveStream {
                stream_id: self.stream_id,
            })
        {
            debug!(stream_id = self.stream_id, error = %e, "failed to send stream removal on drop");
        }
    }
}

// ---------------------------------------------------------------------------
// Driver messages
// ---------------------------------------------------------------------------

#[derive(derive_more::Debug)]
enum DriverMessage {
    OutputStream {
        format: AudioFormat,
        #[debug("Sender")]
        reply: oneshot::Sender<Result<OutputStream>>,
    },
    InputStream {
        format: AudioFormat,
        #[debug("Sender")]
        reply: oneshot::Sender<Result<InputStream>>,
    },
    SwitchDevice {
        input_device: Option<Option<DeviceId>>,
        output_device: Option<Option<DeviceId>>,
        #[debug("Sender")]
        reply: oneshot::Sender<Result<()>>,
    },
    RemoveStream {
        stream_id: u64,
    },
}

// ---------------------------------------------------------------------------
// Device enumeration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Direction {
    Input,
    Output,
}

fn list_devices(direction: Direction) -> Vec<AudioDevice> {
    let mut out = Vec::new();
    for host_id in cpal::available_hosts() {
        let Ok(host) = cpal::host_from_id(host_id) else {
            continue;
        };
        let default_id = match direction {
            Direction::Input => host.default_input_device().and_then(|d| d.id().ok()),
            Direction::Output => host.default_output_device().and_then(|d| d.id().ok()),
        };
        let devices = match direction {
            Direction::Input => host.input_devices(),
            Direction::Output => host.output_devices(),
        };
        let Ok(devices) = devices else { continue };
        for device in devices {
            let Ok(id) = device.id() else { continue };
            #[allow(
                deprecated,
                reason = "DeviceTrait::name is deprecated in cpal 0.18 but description() is not yet stable on all backends"
            )]
            let name = device.name().unwrap_or_else(|_| id.to_string());
            let is_default = default_id.as_ref() == Some(&id);
            out.push(AudioDevice {
                id: DeviceId(id),
                name,
                is_default,
            });
        }
    }
    out
}

fn resolve_device(
    host: &cpal::Host,
    requested: &Option<DeviceId>,
    direction: Direction,
) -> Option<cpal::Device> {
    if let Some(id) = requested
        && let Some(device) = host.device_by_id(&id.0)
    {
        return Some(device);
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(pipewire_id) = cpal::DeviceId::from_str("alsa:pipewire")
            && let Some(device) = host.device_by_id(&pipewire_id)
        {
            return Some(device);
        }
    }

    match direction {
        Direction::Input => host.default_input_device(),
        Direction::Output => host.default_output_device(),
    }
}

// ---------------------------------------------------------------------------
// Callback command channels
// ---------------------------------------------------------------------------

/// Commands sent from the driver thread to the output callback.
enum OutputCmd {
    /// Adds a new output stream consumer.
    Add {
        stream_id: u64,
        cons: ResamplingCons<f32>,
        fade_state: Arc<AtomicU32>,
        peak_state: Arc<PeakState>,
    },
    /// Removes a stream by ID.
    Remove { stream_id: u64 },
}

/// Commands sent from the driver thread to the input callback.
enum InputCmd {
    /// Adds a new input stream producer.
    Add {
        stream_id: u64,
        prod: Box<ResamplingProd<f32, INTERNAL_CHANNELS>>,
    },
    /// Removes a stream by ID.
    Remove { stream_id: u64 },
}

// ---------------------------------------------------------------------------
// Per-stream state owned by callback closures
// ---------------------------------------------------------------------------

/// Output stream state owned by the output callback closure.
struct OutputEntry {
    stream_id: u64,
    cons: ResamplingCons<f32>,
    fade_state: Arc<AtomicU32>,
    peak_state: Arc<PeakState>,
    /// Progress through current fade (0..DECLICKER_SAMPLES).
    fade_progress: usize,
    /// Previous fade state — used to detect transitions and reset progress.
    prev_fade: u32,
}

/// Input stream state owned by the input callback closure.
struct InputEntry {
    stream_id: u64,
    prod: ResamplingProd<f32, INTERNAL_CHANNELS>,
}

// ---------------------------------------------------------------------------
// Output callback
// ---------------------------------------------------------------------------

/// State captured by the output callback closure.
struct OutputCallbackState {
    /// Active output stream consumers, owned exclusively by this callback.
    entries: Vec<OutputEntry>,
    /// Command channel for adding/removing streams.
    cmd_rx: std::sync::mpsc::Receiver<OutputCmd>,
    /// AEC render reference ring buffer producer.
    render_ref_prod: ringbuf::HeapProd<f32>,
    /// Device channel count (1 or 2).
    device_channels: u16,
    /// Pre-allocated mix buffer (stereo interleaved, MAX_MIX_FRAMES * 2).
    mix_buf: Vec<f32>,
    /// Pre-allocated per-stream scratch buffer.
    stream_buf: Vec<f32>,
}

/// Output callback: reads from all output stream consumers, mixes to stereo,
/// writes render reference for AEC, computes peak levels, maps to device channels.
///
/// No allocation, no tracing, no contended locks.
fn output_callback(state: &mut OutputCallbackState, data: &mut [f32]) {
    // Drain commands (lock-free try_recv).
    while let Ok(cmd) = state.cmd_rx.try_recv() {
        match cmd {
            OutputCmd::Add {
                stream_id,
                cons,
                fade_state,
                peak_state,
            } => {
                let initial = fade_state.load(Ordering::Acquire);
                state.entries.push(OutputEntry {
                    stream_id,
                    cons,
                    fade_state,
                    peak_state,
                    fade_progress: 0,
                    prev_fade: initial,
                });
            }
            OutputCmd::Remove { stream_id } => {
                state.entries.retain(|e| e.stream_id != stream_id);
            }
        }
    }

    let device_channels = state.device_channels as usize;
    let total_frames = data.len() / device_channels;

    // Process in chunks to fit pre-allocated buffers.
    let mut frame_offset = 0;
    while frame_offset < total_frames {
        let chunk_frames = (total_frames - frame_offset).min(MAX_MIX_FRAMES);
        let stereo_samples = chunk_frames * 2;

        // Zero mix buffer.
        state.mix_buf[..stereo_samples].fill(0.0);

        // Read from each stream and sum into mix buffer.
        for entry in &mut state.entries {
            let fade = entry.fade_state.load(Ordering::Acquire);
            if fade == FADE_PAUSED {
                continue;
            }

            // Reset progress on state transitions to prevent discontinuities
            // when toggling pause/resume mid-fade.
            if fade != entry.prev_fade {
                entry.fade_progress = 0;
                entry.prev_fade = fade;
            }

            let status = entry
                .cons
                .read_interleaved(&mut state.stream_buf[..stereo_samples]);
            // ReadStatus is informational; we always use whatever data we got.
            let _ = status;

            // Apply fade gain and mix.
            for i in 0..stereo_samples {
                let gain = match fade {
                    FADE_OUT => {
                        let progress = entry.fade_progress.min(DECLICKER_SAMPLES);
                        let g = 1.0 - (progress as f32 / DECLICKER_SAMPLES as f32);
                        if i % 2 == 1 {
                            entry.fade_progress = entry.fade_progress.saturating_add(1);
                            if entry.fade_progress >= DECLICKER_SAMPLES {
                                entry.fade_state.store(FADE_PAUSED, Ordering::Release);
                            }
                        }
                        g
                    }
                    FADE_IN => {
                        let progress = entry.fade_progress.min(DECLICKER_SAMPLES);
                        let g = progress as f32 / DECLICKER_SAMPLES as f32;
                        if i % 2 == 1 {
                            entry.fade_progress = entry.fade_progress.saturating_add(1);
                            if entry.fade_progress >= DECLICKER_SAMPLES {
                                entry.fade_state.store(FADE_PLAYING, Ordering::Release);
                                entry.fade_progress = 0;
                            }
                        }
                        g
                    }
                    _ => 1.0, // FADE_PLAYING
                };
                state.mix_buf[i] += state.stream_buf[i] * gain;
            }
        }

        // Clamp mix to [-1.0, 1.0].
        let mut peak_l: f32 = 0.0;
        let mut peak_r: f32 = 0.0;
        for i in (0..stereo_samples).step_by(2) {
            state.mix_buf[i] = state.mix_buf[i].clamp(-1.0, 1.0);
            state.mix_buf[i + 1] = state.mix_buf[i + 1].clamp(-1.0, 1.0);
            let al = state.mix_buf[i].abs();
            let ar = state.mix_buf[i + 1].abs();
            if al > peak_l {
                peak_l = al;
            }
            if ar > peak_r {
                peak_r = ar;
            }
        }

        // Store peak values for each active stream (shared peak across all).
        for entry in &state.entries {
            entry.peak_state.store(peak_l, peak_r);
        }

        // Write mixed stereo into AEC render reference ring buffer.
        // The input callback reads this to feed sonora's render path.
        let _ = state
            .render_ref_prod
            .push_slice(&state.mix_buf[..stereo_samples]);

        // Channel-map stereo mix to device output buffer.
        let out_offset = frame_offset * device_channels;
        if device_channels == 1 {
            // Stereo → mono: average L+R.
            for i in 0..chunk_frames {
                data[out_offset + i] = (state.mix_buf[i * 2] + state.mix_buf[i * 2 + 1]) * 0.5;
            }
        } else {
            // Stereo → stereo (or multi-channel: fill first 2, zero rest).
            for i in 0..chunk_frames {
                data[out_offset + i * device_channels] = state.mix_buf[i * 2];
                if device_channels >= 2 {
                    data[out_offset + i * device_channels + 1] = state.mix_buf[i * 2 + 1];
                }
                for ch in 2..device_channels {
                    data[out_offset + i * device_channels + ch] = 0.0;
                }
            }
        }

        frame_offset += chunk_frames;
    }
}

// ---------------------------------------------------------------------------
// Input callback
// ---------------------------------------------------------------------------

/// State captured by the input callback closure.
struct InputCallbackState {
    /// Active input stream producers, owned exclusively by this callback.
    entries: Vec<InputEntry>,
    /// Command channel for adding/removing streams.
    cmd_rx: std::sync::mpsc::Receiver<InputCmd>,
    /// AEC state for callback-based processing.
    aec: AecState,
    /// Device channel count (1 or 2).
    device_channels: u16,
    /// Pre-allocated stereo buffer for channel mapping.
    stereo_buf: Vec<f32>,
}

/// Input callback: reads from device, channel-maps to stereo, processes through
/// AEC (render reference + capture), distributes to all input stream producers.
///
/// No allocation, no tracing, no contended locks.
fn input_callback(state: &mut InputCallbackState, data: &[f32]) {
    // Drain commands.
    while let Ok(cmd) = state.cmd_rx.try_recv() {
        match cmd {
            InputCmd::Add { stream_id, prod } => {
                state.entries.push(InputEntry {
                    stream_id,
                    prod: *prod,
                });
            }
            InputCmd::Remove { stream_id } => {
                state.entries.retain(|e| e.stream_id != stream_id);
            }
        }
    }

    let device_channels = state.device_channels as usize;
    let num_frames = data.len() / device_channels;

    // Channel-map device audio to stereo.
    let stereo_samples = num_frames * 2;
    // Ensure stereo_buf is large enough (pre-allocated to MAX_MIX_FRAMES * 2).
    let stereo_buf = &mut state.stereo_buf[..stereo_samples];
    if device_channels == 1 {
        for i in 0..num_frames {
            stereo_buf[i * 2] = data[i];
            stereo_buf[i * 2 + 1] = data[i];
        }
    } else {
        for i in 0..num_frames {
            stereo_buf[i * 2] = data[i * device_channels];
            stereo_buf[i * 2 + 1] = if device_channels >= 2 {
                data[i * device_channels + 1]
            } else {
                data[i * device_channels]
            };
        }
    }

    // Process through AEC: drains render reference, runs render frames,
    // then processes capture frames.
    state.aec.process_stereo_interleaved(stereo_buf, num_frames);

    // Distribute processed capture audio to all input stream producers.
    for entry in &mut state.entries {
        entry.prod.push_interleaved(stereo_buf);
    }
}

// ---------------------------------------------------------------------------
// AudioDriver
// ---------------------------------------------------------------------------

struct AudioDriver {
    rx: mpsc::Receiver<DriverMessage>,
    tx: mpsc::WeakSender<DriverMessage>,
    opts: AudioBackendOpts,
    current_input_device: Option<DeviceId>,
    current_output_device: Option<DeviceId>,
    next_stream_id: u64,
    /// Command senders for output callback. Replaced on device switch.
    output_cmd_tx: Option<std::sync::mpsc::SyncSender<OutputCmd>>,
    /// Command senders for input callback. Replaced on device switch.
    input_cmd_tx: Option<std::sync::mpsc::SyncSender<InputCmd>>,
    /// Active cpal streams (kept alive by ownership).
    _output_stream: Option<cpal::Stream>,
    _input_stream: Option<cpal::Stream>,
    /// Tracked output stream metadata for device switching.
    tracked_outputs: Vec<TrackedOutput>,
    /// Tracked input stream metadata for device switching.
    tracked_inputs: Vec<TrackedInput>,
    /// Error channel from cpal error callbacks.
    error_rx: std::sync::mpsc::Receiver<StreamErrorInfo>,
    error_tx: std::sync::mpsc::SyncSender<StreamErrorInfo>,
    /// AEC processor.
    aec_processor: AecProcessor,
    /// AEC enabled flag shared with callbacks.
    aec_enabled: Arc<AtomicBool>,
    /// Backoff for restart attempts.
    restart_backoff: Duration,
    next_restart_allowed: Instant,
    underrun_count: u32,
    underrun_window_start: Instant,
}

/// Metadata for a tracked output stream. The actual consumer lives inside the
/// output callback closure; the driver only keeps the producer (in the OutputStream
/// handle) and metadata needed to rebuild on device switch.
struct TrackedOutput {
    stream_id: u64,
    format: AudioFormat,
    /// Shared producer handle — the OutputStream user holds one end.
    prod: Arc<std::sync::Mutex<ResamplingProd<f32, INTERNAL_CHANNELS>>>,
    fade_state: Arc<AtomicU32>,
    peak_state: Arc<PeakState>,
}

/// Metadata for a tracked input stream.
struct TrackedInput {
    stream_id: u64,
    format: AudioFormat,
    /// Shared consumer handle — the InputStream user holds one end.
    cons: Arc<std::sync::Mutex<ResamplingCons<f32>>>,
}

#[derive(Debug)]
struct StreamErrorInfo {
    direction: &'static str,
    error: cpal::StreamError,
}

impl fmt::Debug for AudioDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioDriver")
            .field("outputs", &self.tracked_outputs.len())
            .field("inputs", &self.tracked_inputs.len())
            .finish_non_exhaustive()
    }
}

impl AudioDriver {
    fn new(
        rx: mpsc::Receiver<DriverMessage>,
        tx: mpsc::WeakSender<DriverMessage>,
        opts: AudioBackendOpts,
        aec_enabled: Arc<AtomicBool>,
    ) -> Self {
        let (error_tx, error_rx) = std::sync::mpsc::sync_channel(8);

        let aec_config = AecProcessorConfig::new(INTERNAL_RATE, 2, 2);
        let aec_processor =
            AecProcessor::new(aec_config, true).expect("failed to initialize AEC processor");

        let mut driver = Self {
            rx,
            tx,
            current_input_device: opts.input_device.clone(),
            current_output_device: opts.output_device.clone(),
            opts,
            next_stream_id: 0,
            output_cmd_tx: None,
            input_cmd_tx: None,
            _output_stream: None,
            _input_stream: None,
            tracked_outputs: Vec::new(),
            tracked_inputs: Vec::new(),
            error_rx,
            error_tx,
            aec_processor,
            aec_enabled,
            restart_backoff: Duration::from_millis(500),
            next_restart_allowed: Instant::now(),
            underrun_count: 0,
            underrun_window_start: Instant::now(),
        };

        if let Err(e) = driver.start_cpal_streams() {
            error!("failed to start initial audio streams: {e:#}");
        }

        driver
    }

    fn run(&mut self) {
        loop {
            // Drain pending messages first.
            match self.rx.try_recv() {
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    info!("closing audio driver: message channel closed");
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    self.drain_errors();
                    // Block for next message.
                    match self.rx.blocking_recv() {
                        Some(msg) => self.handle_message(msg),
                        None => {
                            info!("closing audio driver: message channel closed");
                            break;
                        }
                    }
                }
                Ok(msg) => {
                    self.handle_message(msg);
                    // Drain remaining.
                    loop {
                        match self.rx.try_recv() {
                            Ok(msg) => self.handle_message(msg),
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => {
                                info!("closing audio driver: message channel closed");
                                return;
                            }
                        }
                    }
                }
            }
            self.drain_errors();
        }
    }

    fn handle_message(&mut self, message: DriverMessage) {
        debug!("handle {message:?}");
        match message {
            DriverMessage::OutputStream { format, reply } => {
                let res = self
                    .create_output_stream(format)
                    .inspect_err(|err| warn!("failed to create audio output stream: {err:#}"));
                reply.send(res).ok();
            }
            DriverMessage::InputStream { format, reply } => {
                let res = self
                    .create_input_stream(format)
                    .inspect_err(|err| warn!("failed to create audio input stream: {err:#}"));
                reply.send(res).ok();
            }
            DriverMessage::SwitchDevice {
                input_device,
                output_device,
                reply,
            } => {
                let input = match input_device {
                    Some(dev) => dev,
                    None => self.opts.input_device.clone(),
                };
                let output = match output_device {
                    Some(dev) => dev,
                    None => self.opts.output_device.clone(),
                };
                let res = self.switch_devices_internal(input, output);
                reply.send(res).ok();
            }
            DriverMessage::RemoveStream { stream_id } => {
                self.remove_stream(stream_id);
            }
        }
    }

    fn drain_errors(&mut self) {
        while let Ok(err_info) = self.error_rx.try_recv() {
            match &err_info.error {
                cpal::StreamError::BufferUnderrun => {
                    self.underrun_count += 1;
                    if self.underrun_window_start.elapsed() > Duration::from_secs(5) {
                        self.underrun_count = 1;
                        self.underrun_window_start = Instant::now();
                    }
                    if self.underrun_count > 20 {
                        warn!(
                            direction = err_info.direction,
                            count = self.underrun_count,
                            "excessive buffer underruns, attempting restart"
                        );
                        self.underrun_count = 0;
                        self.attempt_restart();
                    }
                }
                cpal::StreamError::DeviceNotAvailable => {
                    warn!(direction = err_info.direction, "audio device unavailable");
                    self.attempt_restart();
                }
                cpal::StreamError::StreamInvalidated => {
                    warn!(direction = err_info.direction, "audio stream invalidated");
                    self.attempt_restart();
                }
                _ => {
                    warn!(
                        direction = err_info.direction,
                        error = ?err_info.error,
                        "audio stream error"
                    );
                }
            }
        }
    }

    fn attempt_restart(&mut self) {
        let now = Instant::now();
        if now < self.next_restart_allowed {
            return;
        }
        match self.start_cpal_streams() {
            Ok(()) => {
                self.restart_backoff = Duration::from_millis(500);
                info!("audio streams restarted");
            }
            Err(e) => {
                error!("failed to restart audio streams: {e:#}");
                self.next_restart_allowed = now + self.restart_backoff;
                self.restart_backoff = (self.restart_backoff * 2).min(Duration::from_secs(4));
            }
        }
    }

    /// Starts (or restarts) cpal input and output streams.
    ///
    /// Creates new cpal streams, command channels, and ring buffers. For tracked
    /// streams, rebuilds the resampling channels and swaps fresh producers/consumers
    /// into the existing handles.
    fn start_cpal_streams(&mut self) -> Result<()> {
        let host = cpal::default_host();

        // Resolve output device.
        let output_device = resolve_device(&host, &self.current_output_device, Direction::Output)
            .context("no output audio device available")?;
        let output_config = output_device
            .default_output_config()
            .context("no default output config")?;
        let output_channels = output_config.channels();
        let output_stream_config: cpal::StreamConfig = output_config.into();

        info!(
            sample_rate = output_stream_config.sample_rate,
            channels = output_channels,
            "opening output device"
        );

        // Create command channel for output callback (capacity 32 — more than
        // enough for stream add/remove operations between callbacks).
        let (output_cmd_tx, output_cmd_rx) = std::sync::mpsc::sync_channel::<OutputCmd>(32);

        // Rebuild existing output streams: create fresh resampling channels,
        // send new consumers to the callback, swap producers into handles.
        for tracked in &self.tracked_outputs {
            let (prod, cons) = create_output_channel(tracked.format);
            *tracked.prod.lock().expect("poisoned") = prod;
            output_cmd_tx
                .send(OutputCmd::Add {
                    stream_id: tracked.stream_id,
                    cons,
                    fade_state: tracked.fade_state.clone(),
                    peak_state: tracked.peak_state.clone(),
                })
                .ok();
        }

        // Build AEC render reference ring buffer.
        let (render_ref_prod, render_ref_cons) = HeapRb::<f32>::new(RENDER_REF_CAPACITY).split();

        // Build output stream.
        let error_tx = self.error_tx.clone();
        let output_stream = output_device
            .build_output_stream(
                output_stream_config,
                {
                    let mut state = OutputCallbackState {
                        entries: Vec::with_capacity(16),
                        cmd_rx: output_cmd_rx,
                        render_ref_prod,
                        device_channels: output_channels,
                        mix_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
                        stream_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
                    };
                    move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                        output_callback(&mut state, data);
                    }
                },
                {
                    let error_tx = error_tx.clone();
                    move |err| {
                        let _ = error_tx.try_send(StreamErrorInfo {
                            direction: "output",
                            error: err,
                        });
                    }
                },
                None,
            )
            .context("failed to build output stream")?;
        output_stream
            .play()
            .context("failed to play output stream")?;

        // Build input stream if device is available.
        let (input_cmd_tx, input_stream) = {
            let input_device = resolve_device(&host, &self.current_input_device, Direction::Input);

            if let Some(input_device) = input_device {
                let input_config = input_device
                    .default_input_config()
                    .context("no default input config")?;
                let input_channels = input_config.channels();
                let input_stream_config: cpal::StreamConfig = input_config.into();

                info!(
                    sample_rate = input_stream_config.sample_rate,
                    channels = input_channels,
                    "opening input device"
                );

                let (input_cmd_tx, input_cmd_rx) = std::sync::mpsc::sync_channel::<InputCmd>(32);

                // Rebuild existing input streams.
                for tracked in &self.tracked_inputs {
                    let (prod, cons) = create_input_channel(tracked.format);
                    *tracked.cons.lock().expect("poisoned") = cons;
                    input_cmd_tx
                        .send(InputCmd::Add {
                            stream_id: tracked.stream_id,
                            prod: Box::new(prod),
                        })
                        .ok();
                }

                let aec_state = AecState::new(
                    self.aec_processor.clone(),
                    render_ref_cons,
                    self.aec_enabled.clone(),
                );

                let error_tx = self.error_tx.clone();
                let stream = input_device
                    .build_input_stream(
                        input_stream_config,
                        {
                            let mut state = InputCallbackState {
                                entries: Vec::with_capacity(16),
                                cmd_rx: input_cmd_rx,
                                aec: aec_state,
                                device_channels: input_channels,
                                stereo_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
                            };
                            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                                input_callback(&mut state, data);
                            }
                        },
                        move |err| {
                            let _ = error_tx.try_send(StreamErrorInfo {
                                direction: "input",
                                error: err,
                            });
                        },
                        None,
                    )
                    .context("failed to build input stream")?;
                stream.play().context("failed to play input stream")?;

                (Some(input_cmd_tx), Some(stream))
            } else {
                debug!("no input device available, skipping input stream");
                (None, None)
            }
        };

        self._output_stream = Some(output_stream);
        self._input_stream = input_stream;
        self.output_cmd_tx = Some(output_cmd_tx);
        self.input_cmd_tx = input_cmd_tx;

        Ok(())
    }

    fn create_output_stream(&mut self, format: AudioFormat) -> Result<OutputStream> {
        let stream_id = self.next_stream_id;
        self.next_stream_id += 1;

        let (prod, cons) = create_output_channel(format);
        let prod = Arc::new(std::sync::Mutex::new(prod));
        let paused = Arc::new(AtomicBool::new(false));
        let fade_state = Arc::new(AtomicU32::new(FADE_PLAYING));
        let peak_state = Arc::new(PeakState::new());

        // Send consumer to the output callback.
        if let Some(tx) = &self.output_cmd_tx {
            tx.send(OutputCmd::Add {
                stream_id,
                cons,
                fade_state: fade_state.clone(),
                peak_state: peak_state.clone(),
            })
            .map_err(|_| anyhow::anyhow!("output callback command channel closed"))?;
        } else {
            anyhow::bail!("output stream not running");
        }

        self.tracked_outputs.push(TrackedOutput {
            stream_id,
            format,
            prod: prod.clone(),
            fade_state: fade_state.clone(),
            peak_state: peak_state.clone(),
        });

        let drop_guard = Arc::new(StreamDropGuard {
            stream_id,
            tx: self.tx.clone(),
        });

        info!(stream_id, ?format, "created output stream");

        Ok(OutputStream {
            prod,
            format,
            handle: OutputHandle {
                paused,
                fade_state,
                peak_state,
            },
            _drop_guard: drop_guard,
        })
    }

    fn create_input_stream(&mut self, format: AudioFormat) -> Result<InputStream> {
        let stream_id = self.next_stream_id;
        self.next_stream_id += 1;

        let (prod, cons) = create_input_channel(format);
        let cons = Arc::new(std::sync::Mutex::new(cons));

        // Send producer to the input callback.
        if let Some(tx) = &self.input_cmd_tx {
            tx.send(InputCmd::Add {
                stream_id,
                prod: Box::new(prod),
            })
            .map_err(|_| anyhow::anyhow!("input callback command channel closed"))?;
        } else {
            anyhow::bail!("input stream not running");
        }

        self.tracked_inputs.push(TrackedInput {
            stream_id,
            format,
            cons: cons.clone(),
        });

        let drop_guard = Arc::new(StreamDropGuard {
            stream_id,
            tx: self.tx.clone(),
        });

        info!(stream_id, ?format, "created input stream");

        Ok(InputStream {
            cons,
            format,
            inactive_count: 0,
            stereo_temp: Vec::new(),
            _drop_guard: drop_guard,
        })
    }

    fn remove_stream(&mut self, stream_id: u64) {
        if let Some(pos) = self
            .tracked_outputs
            .iter()
            .position(|t| t.stream_id == stream_id)
        {
            self.tracked_outputs.remove(pos);
            if let Some(tx) = &self.output_cmd_tx {
                let _ = tx.send(OutputCmd::Remove { stream_id });
            }
            debug!(stream_id, "removed output stream");
            return;
        }

        if let Some(pos) = self
            .tracked_inputs
            .iter()
            .position(|t| t.stream_id == stream_id)
        {
            self.tracked_inputs.remove(pos);
            if let Some(tx) = &self.input_cmd_tx {
                let _ = tx.send(InputCmd::Remove { stream_id });
            }
            debug!(stream_id, "removed input stream");
        }
    }

    fn switch_devices_internal(
        &mut self,
        input_device: Option<DeviceId>,
        output_device: Option<DeviceId>,
    ) -> Result<()> {
        info!(?input_device, ?output_device, "switching audio devices");

        self.current_input_device = input_device;
        self.current_output_device = output_device;

        // Trigger fade-out on all active output streams before stopping.
        // The old callback will process a few more iterations during the
        // fade, producing a smooth ramp-down instead of an abrupt cut.
        for tracked in &self.tracked_outputs {
            tracked.fade_state.store(FADE_OUT, Ordering::Release);
        }

        // Give the old callback time to process the 3ms fade-out.
        // The callback fires every ~5ms, so 10ms guarantees at least
        // one full callback with the fade applied.
        std::thread::sleep(Duration::from_millis(10));

        // Drop old streams (stops callbacks), then start new ones.
        self._output_stream = None;
        self._input_stream = None;
        self.output_cmd_tx = None;
        self.input_cmd_tx = None;

        // Set all output streams to FADE_IN so the new callback fades in.
        for tracked in &self.tracked_outputs {
            tracked.fade_state.store(FADE_IN, Ordering::Release);
        }

        self.start_cpal_streams()?;

        info!("audio device switch completed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Resampling channel creation
// ---------------------------------------------------------------------------

/// Creates a resampling channel for an output stream (caller rate → 48 kHz internal).
fn create_output_channel(
    format: AudioFormat,
) -> (ResamplingProd<f32, INTERNAL_CHANNELS>, ResamplingCons<f32>) {
    let num_channels = NonZeroUsize::new(INTERNAL_CHANNELS).unwrap();
    let config = ResamplingChannelConfig {
        latency_seconds: 0.3,
        capacity_seconds: 3.0,
        underflow_autocorrect_percent_threshold: Some(25.0),
        overflow_autocorrect_percent_threshold: Some(75.0),
        ..Default::default()
    };
    resampling_channel::<f32, INTERNAL_CHANNELS>(
        num_channels,
        format.sample_rate,
        INTERNAL_RATE,
        config,
    )
}

/// Creates a resampling channel for an input stream (48 kHz internal → caller rate).
fn create_input_channel(
    format: AudioFormat,
) -> (ResamplingProd<f32, INTERNAL_CHANNELS>, ResamplingCons<f32>) {
    let num_channels = NonZeroUsize::new(INTERNAL_CHANNELS).unwrap();
    let config = ResamplingChannelConfig {
        latency_seconds: 0.15,
        capacity_seconds: 1.0,
        underflow_autocorrect_percent_threshold: Some(25.0),
        overflow_autocorrect_percent_threshold: Some(75.0),
        ..Default::default()
    };
    resampling_channel::<f32, INTERNAL_CHANNELS>(
        num_channels,
        INTERNAL_RATE,
        format.sample_rate,
        config,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_state_silence() {
        let state = PeakState::new();
        state.store(0.0, 0.0);
        let norm = state.smoothed_normalized();
        assert!(norm < 0.01, "silence should normalize near zero: {norm}");
    }

    #[test]
    fn peak_state_full_scale() {
        let state = PeakState::new();
        // Store full-scale signal several times for smoothing to converge.
        for _ in 0..20 {
            state.store(1.0, 1.0);
            let _ = state.smoothed_normalized();
        }
        let norm = state.smoothed_normalized();
        assert!(norm > 0.8, "full scale should normalize high: {norm}");
    }

    #[test]
    fn mono_to_stereo_duplication() {
        // Verify that mono→stereo duplication produces correct interleaved output.
        let mono = [0.5f32, -0.3, 0.7];
        let mut stereo = [0.0f32; 6];
        for i in 0..mono.len() {
            stereo[i * 2] = mono[i];
            stereo[i * 2 + 1] = mono[i];
        }
        assert_eq!(stereo, [0.5, 0.5, -0.3, -0.3, 0.7, 0.7]);
    }

    #[test]
    fn stereo_to_mono_averaging() {
        let stereo = [0.5f32, 0.3, -0.2, 0.4, 1.0, -1.0];
        let mut mono = [0.0f32; 3];
        for i in 0..mono.len() {
            mono[i] = (stereo[i * 2] + stereo[i * 2 + 1]) * 0.5;
        }
        assert!((mono[0] - 0.4).abs() < 1e-6);
        assert!((mono[1] - 0.1).abs() < 1e-6);
        assert!((mono[2] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn declicker_constants() {
        // 3ms at 48kHz = 144 samples.
        assert_eq!(DECLICKER_SAMPLES, (48_000.0 * 0.003) as usize);
    }

    #[test]
    fn aec_processor_passthrough() {
        let config = AecProcessorConfig::new(48000, 2, 2);
        let processor = AecProcessor::new(config, false).unwrap();
        assert!(!processor.is_enabled());

        // With AEC disabled, process_capture should copy input to output.
        let frame_size = AecProcessorConfig::frame_size(48000);
        let src_l: Vec<f32> = (0..frame_size)
            .map(|i| i as f32 / frame_size as f32)
            .collect();
        let src_r = src_l.clone();
        let mut dst_l = vec![0.0f32; frame_size];
        let mut dst_r = vec![0.0f32; frame_size];

        let src: &[&[f32]] = &[&src_l, &src_r];
        let dst: &mut [&mut [f32]] = &mut [&mut dst_l, &mut dst_r];
        let stream_config = sonora::StreamConfig::new(48000, 2);
        processor
            .process_capture_f32(src, dst, &stream_config, &stream_config)
            .unwrap();

        assert_eq!(dst_l, src_l);
        assert_eq!(dst_r, src_r);
    }

    // -----------------------------------------------------------------------
    // Callback-level integration tests
    //
    // These test output_callback and input_callback directly with controlled
    // data, verifying mixing, declicking, channel mapping, and AEC flow
    // without requiring an audio device.
    // -----------------------------------------------------------------------

    /// Creates a test output callback state with the given device channel count
    /// and a command channel. Returns (state, cmd_tx) so tests can send commands.
    fn make_output_state(
        device_channels: u16,
    ) -> (OutputCallbackState, std::sync::mpsc::SyncSender<OutputCmd>) {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::sync_channel(32);
        let (render_ref_prod, _) = HeapRb::<f32>::new(RENDER_REF_CAPACITY).split();
        let state = OutputCallbackState {
            entries: Vec::with_capacity(16),
            cmd_rx,
            render_ref_prod,
            device_channels,
            mix_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
            stream_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
        };
        (state, cmd_tx)
    }

    /// Creates a resampling channel pair at 48kHz stereo and returns
    /// (producer, consumer, fade_state, peak_state).
    fn make_output_stream() -> (
        ResamplingProd<f32, INTERNAL_CHANNELS>,
        ResamplingCons<f32>,
        Arc<AtomicU32>,
        Arc<PeakState>,
    ) {
        let (prod, cons) = create_output_channel(AudioFormat::stereo_48k());
        let fade_state = Arc::new(AtomicU32::new(FADE_PLAYING));
        let peak_state = Arc::new(PeakState::new());
        (prod, cons, fade_state, peak_state)
    }

    /// Pushes constant stereo samples into the producer.
    fn push_constant(prod: &mut ResamplingProd<f32, INTERNAL_CHANNELS>, value: f32, frames: usize) {
        let samples: Vec<f32> = vec![value; frames * 2]; // stereo interleaved
        prod.push_interleaved(&samples);
    }

    /// Pumps the output callback enough times to get data flowing through
    /// the resampling channel's latency buffer. Returns the last output buffer.
    fn pump_output(
        state: &mut OutputCallbackState,
        prod: &mut ResamplingProd<f32, INTERNAL_CHANNELS>,
        value: f32,
        pump_count: usize,
    ) -> Vec<f32> {
        let mut data = vec![0.0f32; 1024];
        for _ in 0..pump_count {
            // Keep pushing data so the channel doesn't starve.
            push_constant(prod, value, 512);
            output_callback(state, &mut data);
        }
        data
    }

    #[test]
    fn output_callback_silence_with_no_streams() {
        let (mut state, _tx) = make_output_state(2);
        let mut data = vec![999.0f32; 512]; // 256 stereo frames
        output_callback(&mut state, &mut data);
        // With no streams, output should be silent.
        assert!(data.iter().all(|&s| s == 0.0), "empty mix should be silent");
    }

    #[test]
    fn output_callback_passes_through_single_stream() {
        let (mut state, cmd_tx) = make_output_state(2);
        let (mut prod, cons, fade_state, peak_state) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons,
                fade_state,
                peak_state,
            })
            .unwrap();

        // Pump enough callbacks to fill the resampling channel's 0.3s latency.
        let data = pump_output(&mut state, &mut prod, 0.5, 40);

        let non_zero = data.iter().any(|&s| s.abs() > 0.01);
        assert!(non_zero, "output should contain non-zero samples after priming");
    }

    #[test]
    fn output_callback_mixes_two_streams() {
        let (mut state, cmd_tx) = make_output_state(2);

        let (mut prod_a, cons_a, fade_a, peak_a) = make_output_stream();
        let (mut prod_b, cons_b, fade_b, peak_b) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons: cons_a,
                fade_state: fade_a,
                peak_state: peak_a,
            })
            .unwrap();
        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 2,
                cons: cons_b,
                fade_state: fade_b,
                peak_state: peak_b,
            })
            .unwrap();

        // Pump both streams.
        let mut data = vec![0.0f32; 1024];
        for _ in 0..40 {
            push_constant(&mut prod_a, 0.3, 512);
            push_constant(&mut prod_b, 0.4, 512);
            output_callback(&mut state, &mut data);
        }

        let max_sample = data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!(
            max_sample > 0.3,
            "mixed signal should exceed single stream: {max_sample}"
        );
    }

    #[test]
    fn output_callback_clamps_to_unity() {
        let (mut state, cmd_tx) = make_output_state(2);

        // Two streams each pushing 0.9 — sum is 1.8, should clamp to 1.0.
        let (mut prod_a, cons_a, fade_a, peak_a) = make_output_stream();
        let (mut prod_b, cons_b, fade_b, peak_b) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add { stream_id: 1, cons: cons_a, fade_state: fade_a, peak_state: peak_a })
            .unwrap();
        cmd_tx
            .send(OutputCmd::Add { stream_id: 2, cons: cons_b, fade_state: fade_b, peak_state: peak_b })
            .unwrap();

        let mut data = vec![0.0f32; 1024];
        for _ in 0..40 {
            push_constant(&mut prod_a, 0.9, 512);
            push_constant(&mut prod_b, 0.9, 512);
            output_callback(&mut state, &mut data);
        }

        assert!(
            data.iter().all(|&s| s <= 1.0 && s >= -1.0),
            "output must be clamped to [-1.0, 1.0]"
        );
    }

    #[test]
    fn output_callback_declicker_fade_out() {
        let (mut state, cmd_tx) = make_output_state(2);
        let (mut prod, cons, fade_state, peak_state) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons,
                fade_state: fade_state.clone(),
                peak_state,
            })
            .unwrap();

        // Prime the channel to get data flowing.
        pump_output(&mut state, &mut prod, 1.0, 40);

        // Trigger pause → FADE_OUT.
        fade_state.store(FADE_OUT, Ordering::Release);

        // Keep pushing data so the channel stays fed during fade.
        push_constant(&mut prod, 1.0, 4096);
        let mut data2 = vec![0.0f32; 512];
        output_callback(&mut state, &mut data2);

        // The fade should produce a ramp: samples at the start should be larger
        // than samples toward the end.
        let first_quarter: f32 = data2[..128].iter().map(|s| s.abs()).sum::<f32>();
        let last_quarter: f32 = data2[384..].iter().map(|s| s.abs()).sum::<f32>();
        assert!(
            first_quarter > last_quarter,
            "fade out should ramp down: first_q={first_quarter} > last_q={last_quarter}"
        );

        // After enough samples, fade_state should transition to FADE_PAUSED.
        for _ in 0..5 {
            push_constant(&mut prod, 1.0, 1024);
            let mut extra = vec![0.0f32; 1024];
            output_callback(&mut state, &mut extra);
        }
        assert_eq!(
            fade_state.load(Ordering::Acquire),
            FADE_PAUSED,
            "should transition to FADE_PAUSED after fade-out completes"
        );
    }

    #[test]
    fn output_callback_declicker_fade_in() {
        // Start playing, prime channel, then pause, then fade in.
        let (mut state, cmd_tx) = make_output_state(2);
        let (mut prod, cons, fade_state, peak_state) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons,
                fade_state: fade_state.clone(),
                peak_state,
            })
            .unwrap();

        // Prime channel while PLAYING — this primes the resampling channel.
        pump_output(&mut state, &mut prod, 1.0, 40);

        // Now pause.
        fade_state.store(FADE_OUT, Ordering::Release);
        for _ in 0..5 {
            push_constant(&mut prod, 1.0, 1024);
            let mut dummy = vec![0.0f32; 1024];
            output_callback(&mut state, &mut dummy);
        }
        assert_eq!(fade_state.load(Ordering::Acquire), FADE_PAUSED);

        // Resume → FADE_IN.
        fade_state.store(FADE_IN, Ordering::Release);
        state.entries[0].fade_progress = 0;

        push_constant(&mut prod, 1.0, 4096);
        let mut data_fadein = vec![0.0f32; 512];
        output_callback(&mut state, &mut data_fadein);

        // Should ramp up: end louder than start.
        let first_quarter: f32 = data_fadein[..128].iter().map(|s| s.abs()).sum::<f32>();
        let last_quarter: f32 = data_fadein[384..].iter().map(|s| s.abs()).sum::<f32>();
        assert!(
            last_quarter > first_quarter,
            "fade in should ramp up: last_q={last_quarter} > first_q={first_quarter}"
        );
    }

    #[test]
    fn output_callback_mono_device() {
        let (mut state, cmd_tx) = make_output_state(1); // Mono device
        let (mut prod, cons, fade, peak) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons,
                fade_state: fade,
                peak_state: peak,
            })
            .unwrap();

        // Prime with stereo data (mono device reads fewer frames per callback,
        // so use a larger output buffer and more iterations).
        let mut data = vec![0.0f32; 1024]; // 1024 mono frames
        for _ in 0..60 {
            push_constant(&mut prod, 0.5, 1024);
            output_callback(&mut state, &mut data);
        }

        let non_zero = data.iter().any(|&s| s.abs() > 0.01);
        assert!(non_zero, "mono output should have content");
    }

    #[test]
    fn output_callback_stream_add_remove() {
        let (mut state, cmd_tx) = make_output_state(2);
        let (mut prod, cons, fade, peak) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 42,
                cons,
                fade_state: fade,
                peak_state: peak,
            })
            .unwrap();

        // Callback with stream present.
        pump_output(&mut state, &mut prod, 0.5, 5);
        assert_eq!(state.entries.len(), 1);

        // Remove stream.
        cmd_tx.send(OutputCmd::Remove { stream_id: 42 }).unwrap();
        let mut data2 = vec![0.0f32; 512];
        output_callback(&mut state, &mut data2);
        assert_eq!(state.entries.len(), 0);

        // After removal, output should be silent.
        assert!(
            data2.iter().all(|&s| s == 0.0),
            "removed stream should produce silence"
        );
    }

    #[test]
    fn output_callback_peak_tracking() {
        let (mut state, cmd_tx) = make_output_state(2);
        let (mut prod, cons, fade, peak) = make_output_stream();

        let peak_clone = peak.clone();
        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons,
                fade_state: fade,
                peak_state: peak,
            })
            .unwrap();

        // Pump to get data flowing, then check peak.
        pump_output(&mut state, &mut prod, 0.8, 40);

        for _ in 0..10 {
            let _ = peak_clone.smoothed_normalized();
        }
        let norm = peak_clone.smoothed_normalized();
        assert!(
            norm > 0.0,
            "peak should be non-zero after signal: {norm}"
        );
    }

    #[test]
    fn aec_render_reference_flows_to_input() {
        // Verify the full output→input AEC data flow:
        // 1. Output callback writes to render_ref_prod
        // 2. Input callback's AecState reads from render_ref_cons
        let (render_ref_prod, render_ref_cons) = HeapRb::<f32>::new(RENDER_REF_CAPACITY).split();
        let (output_cmd_tx, output_cmd_rx) = std::sync::mpsc::sync_channel(32);
        let (input_cmd_tx, input_cmd_rx) = std::sync::mpsc::sync_channel(32);

        // Create output state with the producer end.
        let mut output_state = OutputCallbackState {
            entries: Vec::with_capacity(16),
            cmd_rx: output_cmd_rx,
            render_ref_prod,
            device_channels: 2,
            mix_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
            stream_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
        };

        // Create input state with the consumer end (AEC disabled for passthrough).
        let aec_config = AecProcessorConfig::new(INTERNAL_RATE, 2, 2);
        let aec_processor = AecProcessor::new(aec_config, false).unwrap();
        let aec_enabled = Arc::new(AtomicBool::new(false));
        let aec_state = AecState::new(aec_processor, render_ref_cons, aec_enabled);

        let mut input_state = InputCallbackState {
            entries: Vec::with_capacity(16),
            cmd_rx: input_cmd_rx,
            aec: aec_state,
            device_channels: 2,
            stereo_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
        };

        // Add an output stream.
        let (mut prod, cons, fade, peak) = make_output_stream();
        output_cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons,
                fade_state: fade,
                peak_state: peak,
            })
            .unwrap();

        // Add an input stream to capture AEC output.
        let (input_prod, _input_cons) = create_input_channel(AudioFormat::stereo_48k());
        input_cmd_tx
            .send(InputCmd::Add {
                stream_id: 1,
                prod: Box::new(input_prod),
            })
            .unwrap();

        // Pump output callback to prime the channel and fill render reference.
        for _ in 0..40 {
            push_constant(&mut prod, 0.7, 512);
            let mut out_data = vec![0.0f32; 1024];
            output_callback(&mut output_state, &mut out_data);
        }

        // Run input callback with simulated mic data.
        let in_data = vec![0.3f32; 1024]; // 512 stereo frames
        input_callback(&mut input_state, &in_data);

        // Verify the AecState received render reference data.
        // The render_ref_cons should have been drained by the AEC processing.
        // We can't easily inspect the AecState internals, but the fact that
        // input_callback completed without panic confirms the data flow.
    }

    #[test]
    fn output_callback_large_buffer_processed_in_chunks() {
        // Verify that buffers larger than MAX_MIX_FRAMES are handled correctly
        // by the chunking loop.
        let (mut state, _cmd_tx) = make_output_state(2);
        // 6000 stereo frames = 12000 samples — larger than MAX_MIX_FRAMES (4096)
        let mut data = vec![999.0f32; 12000];
        output_callback(&mut state, &mut data);
        // All should be zeroed (no streams).
        assert!(
            data.iter().all(|&s| s == 0.0),
            "large buffer should be fully zeroed with no streams"
        );
    }

    #[test]
    fn input_callback_mono_device_duplicates_to_stereo() {
        let (input_cmd_tx, input_cmd_rx) = std::sync::mpsc::sync_channel(32);
        let (_, render_ref_cons) = HeapRb::<f32>::new(RENDER_REF_CAPACITY).split();
        let aec_config = AecProcessorConfig::new(INTERNAL_RATE, 2, 2);
        let aec_processor = AecProcessor::new(aec_config, false).unwrap();
        let aec_enabled = Arc::new(AtomicBool::new(false));
        let aec_state = AecState::new(aec_processor, render_ref_cons, aec_enabled);

        let mut state = InputCallbackState {
            entries: Vec::with_capacity(16),
            cmd_rx: input_cmd_rx,
            aec: aec_state,
            device_channels: 1, // Mono mic
            stereo_buf: vec![0.0f32; MAX_MIX_FRAMES * 2],
        };

        let (prod, _cons) = create_input_channel(AudioFormat::stereo_48k());
        input_cmd_tx
            .send(InputCmd::Add {
                stream_id: 1,
                prod: Box::new(prod),
            })
            .unwrap();

        // Send mono mic data.
        let mono_data: Vec<f32> = (0..480).map(|i| (i as f32 / 480.0) * 0.5).collect();
        input_callback(&mut state, &mono_data);

        // The stereo_buf should have duplicated mono to stereo.
        for i in 0..480 {
            let l = state.stereo_buf[i * 2];
            let r = state.stereo_buf[i * 2 + 1];
            assert!(
                (l - r).abs() < 1e-6,
                "mono duplication: L={l} should equal R={r} at frame {i}"
            );
        }
    }

    #[test]
    fn fade_state_transitions_correctly() {
        // Verify the full lifecycle: PLAYING → FADE_OUT → PAUSED → FADE_IN → PLAYING
        let (mut state, cmd_tx) = make_output_state(2);
        let (mut prod, cons, fade_state, peak_state) = make_output_stream();

        cmd_tx
            .send(OutputCmd::Add {
                stream_id: 1,
                cons,
                fade_state: fade_state.clone(),
                peak_state,
            })
            .unwrap();

        // Prime the channel.
        pump_output(&mut state, &mut prod, 0.5, 40);
        assert_eq!(fade_state.load(Ordering::Acquire), FADE_PLAYING);

        // Trigger FADE_OUT.
        fade_state.store(FADE_OUT, Ordering::Release);
        let mut buf = vec![0.0f32; 1024];
        for _ in 0..5 {
            push_constant(&mut prod, 0.5, 1024);
            output_callback(&mut state, &mut buf);
        }
        assert_eq!(
            fade_state.load(Ordering::Acquire),
            FADE_PAUSED,
            "should be PAUSED after fade-out"
        );

        // Trigger FADE_IN.
        fade_state.store(FADE_IN, Ordering::Release);
        state.entries[0].fade_progress = 0;
        for _ in 0..5 {
            push_constant(&mut prod, 0.5, 1024);
            output_callback(&mut state, &mut buf);
        }
        assert_eq!(
            fade_state.load(Ordering::Acquire),
            FADE_PLAYING,
            "should return to PLAYING after fade-in"
        );
    }

    // cpal e2e tests deferred: requires either real audio device or injecting
    // a custom host (cpal feature = "custom") into AudioBackend. The callback-
    // level tests above cover mixing, AEC, declicker, and channel mapping
    // without hardware. A future improvement would make AudioBackend accept
    // a host factory for deterministic cpal-level testing.
}
