//! Media sources for [`moq_video`] and [`moq_audio`] that upstream moq lacks.
//!
//! A source is an opened device, file or generator that produces raw frames,
//! on a thread of its own, for a publisher to encode. A [`VideoSource`] can
//! feed several encoders at once and offers a latest-wins preview through
//! [`VideoFrames`]. An [`AudioSource`] fans PCM out to every broadcast that
//! reads it. An [`EncodedVideoSource`] carries H.264 that needs no encoder.
//!
//! The crate has no dependency on iroh and none on a transport. Its [`Error`]
//! is the one `moq-ext-media-core` defines, which the player and the publisher
//! return too.
//!
//! # What is in it
//!
//! | Source | Constructor | Feature |
//! |---|---|---|
//! | Test picture: a sweeping bar, a frame counter, a clock, and a marker that flashes with the beep | [`VideoSource::test_pattern`], [`AudioSource::test_pattern`] | `pattern` |
//! | Sine tone | [`AudioSource::tone`] | `pattern` |
//! | WAV and MP3 file, decoded in real time | [`AudioSource::file`] | `file` |
//! | Camera, display, window | [`VideoSource::capture`] | `capture` |
//! | Microphone | [`AudioSource::microphone`] | `capture` |
//! | Raspberry Pi camera through `rpicam-vid` | [`VideoSource::rpicam`], [`EncodedVideoSource::rpicam`] | `rpicam` (Linux) |
//! | Pushed by the application | [`VideoSource::push`], [`AudioSource::push`], [`VideoSource::spawn`] | always |
//! | Pre-encoded Annex-B H.264 | [`EncodedVideoSource::annex_b`] | always |
//!
//! Also always available: [`FrameSender`], the push end of a source with the
//! demand it reports back; [`VideoFrames`] and [`frames::FrameSlot`], the
//! latest-wins frame stream; and [`local_task::spawn`], which runs a future on
//! a thread of its own for capture streams that are not `Send`.
//!
//! # Features
//!
//! | Feature | Default | Effect |
//! |---|---|---|
//! | `pattern` | yes | test pattern and tone generators (pulls `moq-mux`) |
//! | `file` | no | audio file source (pulls `symphonia`) |
//! | `capture` | no | camera, screen and microphone, through `moq_video::capture` and `moq_audio::capture` |
//! | `sound-server` | no | PipeWire and PulseAudio devices for the microphone |
//! | `aec` | no | echo cancellation for the microphone, implies `capture` |
//! | `pipewire` | no | Linux screen capture through the desktop portal, and PipeWire's default camera, implies `capture` |
//! | `rpicam` | no | the Raspberry Pi camera, on Linux only (pulls `tokio/process`) |
//!
//! # Upstreaming candidates
//!
//! These belong in moq itself, one change each:
//!
//! - the pattern and tone generators go to `moq-video`;
//! - the audio file source goes to `moq-audio`, which has no container reader;
//! - the rpicam source becomes a `moq-video` capture backend.
//!
//! # Cancellation safety
//!
//! Every constructor that is `async` is cancellation safe. Dropping the
//! future stops the thread or subprocess it started and releases the device.
//! [`VideoFrames::next`] and [`FrameSender::closed`] lose nothing when dropped.
//!
//! # Example
//!
//! ```
//! # #[cfg(feature = "pattern")]
//! # async fn example() {
//! use moq_ext_sources::VideoSource;
//!
//! let source = VideoSource::test_pattern(
//!     moq_video::Size::new(640, 360),
//!     moq_video::FrameRate::new(30, 1).expect("a valid rate"),
//! );
//! let mut frames = source.frames();
//! while let Some(frame) = frames.next().await {
//!     // draw `frame`
//! #   let _ = frame;
//! #   break;
//! }
//! # }
//! ```

pub mod frames;
#[cfg(any(feature = "capture", all(target_os = "linux", feature = "rpicam")))]
pub mod local_task;
mod source;

use moq_audio as audio;
use moq_video as video;

#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub use self::source::RpicamConfig;
#[cfg(feature = "capture")]
pub use self::source::{EchoReference, MicrophoneConfig};
pub use moq_ext_media_core::{Closed, Error};

pub use self::{
    frames::VideoFrames,
    source::{
        AudioFormat, AudioKind, AudioSource, Demand, DemandGuard, EncodedParts, EncodedVideoSource,
        FrameSender, PcmFanout, VideoFormat, VideoSource, WeakPcmFanout,
    },
};
