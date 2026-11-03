//! Where a player's audio goes: a `moq_audio::playback::Engine`, or nowhere.
//!
//! The application opens an [`Engine`](moq_audio::playback::Engine) and passes
//! it to every [`PlayerConfig`](crate::PlayerConfig) whose audio should play
//! there, and to the `MicrophoneConfig` whose echo it should cancel. This keeps
//! the choice of device explicit. [`AudioTarget::null`] is the output for
//! headless runs and tests.

use std::time::Duration;

#[cfg(feature = "playback")]
use moq_audio::playback;

use crate::{AudioFormat, error::Error};

/// Where a player writes its audio: an opened device engine, or nowhere.
///
/// Every player writing to one engine is mixed into one device stream. Cloning
/// is cheap. The engine closes when the last clone and the last player using
/// it are gone.
#[derive(Debug, Clone)]
pub struct AudioTarget {
    inner: Inner,
    /// Counts canceller requests, so a test can see them without a device.
    #[cfg(all(test, feature = "aec"))]
    cancellers: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[derive(derive_more::Debug, Clone)]
enum Inner {
    #[cfg(feature = "playback")]
    Engine(#[debug(skip)] playback::Engine),
    Null,
}

#[cfg(feature = "playback")]
impl From<playback::Engine> for AudioTarget {
    fn from(engine: playback::Engine) -> Self {
        Self {
            inner: Inner::Engine(engine),
            #[cfg(all(test, feature = "aec"))]
            cancellers: Default::default(),
        }
    }
}

impl AudioTarget {
    /// Returns a target that discards what it is given, for headless use and tests.
    ///
    /// Players still decode their audio and report it in their stats. Nothing
    /// is queued at a speaker, so audio is not paced and video is held for the
    /// jitter allowance alone. A microphone asked to cancel a null target's
    /// echo gets no canceller.
    pub fn null() -> Self {
        Self {
            inner: Inner::Null,
            #[cfg(all(test, feature = "aec"))]
            cancellers: Default::default(),
        }
    }

    /// Returns the engine behind this target, `None` for a [null](Self::null) one.
    #[cfg(feature = "playback")]
    pub fn engine(&self) -> Option<&playback::Engine> {
        match &self.inner {
            Inner::Engine(engine) => Some(engine),
            Inner::Null => None,
        }
    }

    /// Reports whether this target discards what it is given.
    pub fn is_null(&self) -> bool {
        matches!(self.inner, Inner::Null)
    }

    /// Adds a stream to the mix, taking PCM in `format`.
    pub(crate) fn sink(&self, format: AudioFormat) -> Result<OutputSink, Error> {
        match &self.inner {
            #[cfg(feature = "playback")]
            Inner::Engine(engine) => {
                let mut device_input = playback::Input::default();
                device_input.format = moq_audio::Format::F32;
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

    /// Returns how many echo cancellers were asked of this target.
    #[cfg(all(test, feature = "aec"))]
    pub(crate) fn cancellers_requested(&self) -> u64 {
        self.cancellers.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(feature = "aec")]
impl moq_ext_sources::EchoReference for AudioTarget {
    /// Returns `None` for a null target, which plays nothing and has no echo.
    fn canceller(&self) -> Result<Option<moq_audio::aec::Control>, moq_ext_sources::Error> {
        use moq_ext_sources::Error;
        #[cfg(test)]
        self.cancellers
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match &self.inner {
            Inner::Engine(engine) => engine
                .canceller(moq_audio::aec::Config::default())
                .map(Some)
                .map_err(|err| match err {
                    moq_audio::Error::Busy(reason) => Error::invalid(format!(
                        "this output already cancels the echo of another microphone ({reason})"
                    )),
                    other => Error::device(other),
                }),
            Inner::Null => Ok(None),
        }
    }
}

#[cfg(all(feature = "capture", not(feature = "aec")))]
impl moq_ext_sources::EchoReference for AudioTarget {}

/// One player's stream into an output.
#[derive(derive_more::Debug)]
pub(crate) enum OutputSink {
    #[cfg(feature = "playback")]
    Device(#[debug(skip)] Box<playback::Sink>),
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
    Device(#[debug(skip)] playback::Control),
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
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_null_output_takes_anything() {
        let output = AudioTarget::null();
        assert!(output.is_null());
        let mut sink = output
            .sink(AudioFormat {
                sample_rate: 48_000,
                layout: moq_audio::Layout::Stereo,
            })
            .expect("a null sink always opens");
        sink.write(&[0; 64]).expect("discarded");
        assert_eq!(sink.buffered(), Duration::ZERO);
    }

    /// The microphone config builds its echo canceller from its output.
    ///
    /// It needs an output device, so it runs by hand. `publish::tests` covers
    /// the publication side without a device.
    #[cfg(all(feature = "aec", feature = "playback"))]
    #[tokio::test]
    #[ignore = "needs an audio output device"]
    async fn echo_cancellation_attaches_the_canceller() {
        let engine = playback::Engine::open(playback::Config::default())
            .await
            .expect("an audio output device");
        let capture = crate::MicrophoneConfig {
            echo_reference: Some(Arc::new(AudioTarget::from(engine))),
            ..crate::MicrophoneConfig::default()
        }
        .resolve()
        .expect("the canceller builds");
        assert!(
            capture.aec.is_some(),
            "the microphone config carries no canceller"
        );
        let plain = crate::MicrophoneConfig::default()
            .resolve()
            .expect("resolves");
        assert!(plain.aec.is_none());
    }

    #[cfg(all(feature = "capture", not(feature = "aec")))]
    #[test]
    fn echo_cancellation_without_the_feature_is_refused() {
        let config = crate::MicrophoneConfig {
            echo_reference: Some(Arc::new(AudioTarget::null())),
            ..crate::MicrophoneConfig::default()
        };
        assert!(matches!(
            config.resolve(),
            Err(moq_ext_sources::Error::InvalidConfig { .. })
        ));
    }
}
