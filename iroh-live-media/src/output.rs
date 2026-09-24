//! Audio outputs: an opened speaker, or one that discards.
//!
//! The application opens an output and passes it where it is used: to every
//! [`PlayerConfig`](crate::PlayerConfig) whose audio should play there, and to
//! the [`MicrophoneConfig`](crate::MicrophoneConfig) whose echo it should
//! cancel. That makes device choice, tests and echo cancellation explicit,
//! where a process-wide engine let whichever caller got there first choose the
//! device for everyone.

use std::{fmt, sync::Arc, time::Duration};

use crate::{audio, error::Error};

/// An opened audio output: one device and one mixer, or nothing at all.
///
/// Every player writing to it is mixed into one device stream. Cheap to clone;
/// the device closes when the last clone and the last player using it are
/// gone.
#[derive(Clone)]
pub struct AudioOutput {
    inner: Arc<Inner>,
    /// How many echo cancellers were asked of this output, shared by clones,
    /// so a test can see a publication ask without an output device.
    #[cfg(all(test, feature = "aec"))]
    cancellers: Arc<std::sync::atomic::AtomicU64>,
}

enum Inner {
    #[cfg(feature = "playback")]
    Device(audio::playback::Engine),
    Null,
}

impl fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *self.inner {
            #[cfg(feature = "playback")]
            Inner::Device(_) => "AudioOutput(Device)",
            Inner::Null => "AudioOutput(Null)",
        })
    }
}

impl AudioOutput {
    /// Opens an output device, or the system default when `device` is `None`.
    ///
    /// `device` is an id as [`devices`](Self::devices) reports it, such as
    /// `alsa:hw:0,0`. Later failures of the device are handled underneath: it
    /// is reopened with backoff, and players keep writing throughout.
    ///
    /// Cancellation safe: dropping the future closes the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Device`] if there is no such device, or none at all.
    #[cfg(feature = "playback")]
    pub async fn open(device: Option<String>) -> Result<Self, Error> {
        let mut config = audio::playback::Config::default();
        config.device = device;
        let engine = audio::playback::Engine::open(config)
            .await
            .map_err(Error::device)?;
        Ok(Self {
            inner: Arc::new(Inner::Device(engine)),
            #[cfg(all(test, feature = "aec"))]
            cancellers: Arc::default(),
        })
    }

    /// Returns an output that discards what it is given, for headless use and
    /// tests.
    ///
    /// Players still decode their audio and report it in their stats. Nothing
    /// is queued at a speaker, though, so audio is not paced and video is held
    /// for the jitter allowance alone. A microphone asked to cancel a null
    /// output's echo gets no canceller, since nothing plays.
    pub fn null() -> Self {
        Self {
            inner: Arc::new(Inner::Null),
            #[cfg(all(test, feature = "aec"))]
            cancellers: Arc::default(),
        }
    }

    /// Moves every player on this output to another device, or back to the
    /// default when `device` is `None`.
    ///
    /// Players survive the move and are resampled to the new device's rate.
    ///
    /// # Errors
    ///
    /// Fails if the device cannot be opened, in which case the output plays to
    /// no device until a later switch succeeds, and fails at once for a
    /// [`null`](Self::null) output, which has no device to move.
    #[cfg(feature = "playback")]
    pub async fn switch(&self, device: Option<String>) -> Result<(), Error> {
        match &*self.inner {
            Inner::Device(engine) => {
                let mut config = audio::playback::Config::default();
                config.device = device;
                engine.switch(config).await.map_err(Error::device)
            }
            Inner::Null => Err(Error::invalid(
                "a null audio output has no device to switch",
            )),
        }
    }

    /// Lists the output devices the host offers.
    ///
    /// # Errors
    ///
    /// Fails if the host audio API cannot be queried.
    #[cfg(feature = "playback")]
    pub async fn devices() -> Result<Vec<audio::playback::Device>, Error> {
        audio::playback::devices().await.map_err(Error::device)
    }

    /// Reports whether this output discards what it is given.
    pub fn is_null(&self) -> bool {
        matches!(*self.inner, Inner::Null)
    }

    /// Adds a stream to the mix, taking PCM in the layout `input` describes.
    pub(crate) fn sink(&self, input: SinkInput) -> Result<OutputSink, Error> {
        match &*self.inner {
            #[cfg(feature = "playback")]
            Inner::Device(engine) => {
                let mut device_input = audio::playback::Input::default();
                device_input.format = audio::Format::F32;
                device_input.sample_rate = input.sample_rate;
                device_input.layout = input.layout;
                let sink = engine.sink(device_input).map_err(Error::device)?;
                Ok(OutputSink::Device(Box::new(sink)))
            }
            Inner::Null => {
                let _ = input;
                Ok(OutputSink::Null)
            }
        }
    }

    /// Builds an echo canceller tapped off this output's mix, or `None` for a
    /// null output, which plays nothing and so has no echo to cancel.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] while another microphone's canceller
    /// holds this output's reference, and [`Error::Device`] if the engine
    /// refuses for another reason.
    #[cfg(feature = "aec")]
    pub(crate) fn canceller(&self) -> Result<Option<audio::aec::Control>, Error> {
        #[cfg(all(test, feature = "aec"))]
        self.cancellers
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match &*self.inner {
            Inner::Device(engine) => engine
                .canceller(audio::aec::Config::default())
                .map(Some)
                .map_err(|err| match err {
                    audio::Error::Busy(reason) => Error::invalid(format!(
                        "this output already cancels the echo of another microphone ({reason})"
                    )),
                    other => Error::device(other),
                }),
            Inner::Null => Ok(None),
        }
    }

    /// Returns how many echo cancellers were asked of this output.
    #[cfg(all(test, feature = "aec"))]
    pub(crate) fn cancellers_requested(&self) -> u64 {
        self.cancellers.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The PCM a player writes into an output.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    not(feature = "playback"),
    expect(dead_code, reason = "only a device output reads the format")
)]
pub(crate) struct SinkInput {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// The channels and their order.
    pub layout: audio::Layout,
}

/// One player's stream into an output.
pub(crate) enum OutputSink {
    #[cfg(feature = "playback")]
    Device(Box<audio::playback::Sink>),
    Null,
}

impl fmt::Debug for OutputSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            #[cfg(feature = "playback")]
            Self::Device(_) => "OutputSink(Device)",
            Self::Null => "OutputSink(Null)",
        })
    }
}

impl OutputSink {
    /// Writes interleaved `f32` samples.
    pub(crate) fn write(&mut self, samples: &[u8]) -> Result<(), Error> {
        match self {
            #[cfg(feature = "playback")]
            Self::Device(sink) => sink.write(samples).map(drop).map_err(Error::device),
            Self::Null => {
                let _ = samples;
                Ok(())
            }
        }
    }

    /// Returns how much audio is queued ahead of the speaker.
    pub(crate) fn buffered(&self) -> Duration {
        match self {
            #[cfg(feature = "playback")]
            Self::Device(sink) => sink.buffered(),
            Self::Null => Duration::ZERO,
        }
    }

    /// Returns a handle that sets this stream's gain and reads its level from
    /// anywhere.
    pub(crate) fn control(&self) -> OutputControl {
        match self {
            #[cfg(feature = "playback")]
            Self::Device(sink) => OutputControl::Device(sink.control()),
            Self::Null => OutputControl::Null,
        }
    }
}

/// Sets one stream's gain and reads its level, apart from the stream itself.
#[derive(Clone)]
pub(crate) enum OutputControl {
    #[cfg(feature = "playback")]
    Device(audio::playback::Control),
    Null,
}

impl fmt::Debug for OutputControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OutputControl")
    }
}

impl OutputControl {
    /// Sets the stream's gain, where 1.0 is unattenuated.
    pub(crate) fn set_volume(&self, volume: f32) {
        match self {
            #[cfg(feature = "playback")]
            Self::Device(control) => control.set_volume(volume),
            Self::Null => {
                let _ = volume;
            }
        }
    }

    /// Returns the most recent peak level, for a meter.
    pub(crate) fn peak(&self) -> f32 {
        match self {
            #[cfg(feature = "playback")]
            Self::Device(control) => control.peak(),
            Self::Null => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_output_takes_anything() {
        let output = AudioOutput::null();
        assert!(output.is_null());
        let mut sink = output
            .sink(SinkInput {
                sample_rate: 48_000,
                layout: audio::Layout::Stereo,
            })
            .expect("a null sink always opens");
        sink.write(&[0; 64]).expect("discarded");
        assert_eq!(sink.buffered(), Duration::ZERO);
    }
}
