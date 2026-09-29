//! Audio outputs: an opened speaker, or one that discards.
//!
//! The application opens an output and passes it to every
//! [`PlayerConfig`](crate::PlayerConfig) whose audio should play there, and to
//! the `MicrophoneConfig` whose echo it should cancel. This keeps the choice of
//! device explicit.

use std::{sync::Arc, time::Duration};

#[cfg(feature = "playback")]
use crate::audio;
use crate::{AudioFormat, error::Error};

/// An opened audio output: one device and one mixer, or nothing at all.
///
/// Every player writing to it is mixed into one device stream. Cloning is
/// cheap. The device closes when the last clone and the last player using it
/// are gone.
#[derive(Debug, Clone)]
pub struct AudioOutput {
    inner: Arc<Inner>,
    /// Counts canceller requests, so a test can see them without a device.
    #[cfg(all(test, feature = "aec"))]
    cancellers: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(derive_more::Debug)]
enum Inner {
    #[cfg(feature = "playback")]
    Device(#[debug(skip)] audio::playback::Engine),
    Null,
}

impl AudioOutput {
    /// Opens an output device, or the system default when `device` is `None`.
    ///
    /// `device` is an id as [`devices`](Self::devices) reports it, such as
    /// `alsa:hw:0,0`. If the device fails later, it is reopened with backoff
    /// and players keep writing.
    ///
    /// Cancellation safe. Dropping the future closes the device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Device`] if there is no such device, or no device at all.
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

    /// Returns an output that discards what it is given, for headless use and tests.
    ///
    /// Players still decode their audio and report it in their stats. Nothing
    /// is queued at a speaker, so audio is not paced and video is held for the
    /// jitter allowance alone. A microphone asked to cancel a null output's
    /// echo gets no canceller.
    pub fn null() -> Self {
        Self {
            inner: Arc::new(Inner::Null),
            #[cfg(all(test, feature = "aec"))]
            cancellers: Arc::default(),
        }
    }

    /// Moves every player on this output to another device, or to the default for `None`.
    ///
    /// Players survive the move and are resampled to the new device's rate.
    ///
    /// Cancellation safe. The switch is queued before the first wait and
    /// completes on the output's own thread, so dropping the future only loses
    /// its result.
    ///
    /// # Errors
    ///
    /// Fails if the device cannot be opened. The output then plays to no device
    /// until a later switch succeeds. Fails at once for a [`null`](Self::null)
    /// output.
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
    /// Cancellation safe. Dropping the future abandons the query.
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

    /// Adds a stream to the mix, taking PCM in `format`.
    pub(crate) fn sink(&self, format: AudioFormat) -> Result<OutputSink, Error> {
        match &*self.inner {
            #[cfg(feature = "playback")]
            Inner::Device(engine) => {
                let mut device_input = audio::playback::Input::default();
                device_input.format = audio::Format::F32;
                device_input.sample_rate = format.sample_rate;
                device_input.layout = format.layout;
                let sink = engine.sink(device_input).map_err(Error::device)?;
                Ok(OutputSink::Device(Box::new(sink)))
            }
            Inner::Null => {
                let _ = format;
                Ok(OutputSink::Null)
            }
        }
    }

    /// Builds an echo canceller tapped off this output's mix.
    ///
    /// Returns `None` for a null output, which plays nothing and has no echo.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] while another microphone's canceller
    /// holds this output's reference, and [`Error::Device`] for other engine
    /// failures.
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

/// One player's stream into an output.
#[derive(derive_more::Debug)]
pub(crate) enum OutputSink {
    #[cfg(feature = "playback")]
    Device(#[debug(skip)] Box<audio::playback::Sink>),
    Null,
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

    /// Returns a handle that sets this stream's gain and reads its level.
    pub(crate) fn control(&self) -> OutputControl {
        match self {
            #[cfg(feature = "playback")]
            Self::Device(sink) => OutputControl::Device(sink.control()),
            Self::Null => OutputControl::Null,
        }
    }
}

/// Sets one stream's gain and reads its level, separately from the stream.
#[derive(derive_more::Debug, Clone)]
pub(crate) enum OutputControl {
    #[cfg(feature = "playback")]
    Device(#[debug(skip)] audio::playback::Control),
    Null,
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
    use crate::audio;

    #[test]
    fn a_null_output_takes_anything() {
        let output = AudioOutput::null();
        assert!(output.is_null());
        let mut sink = output
            .sink(AudioFormat {
                sample_rate: 48_000,
                layout: audio::Layout::Stereo,
            })
            .expect("a null sink always opens");
        sink.write(&[0; 64]).expect("discarded");
        assert_eq!(sink.buffered(), Duration::ZERO);
    }
}
