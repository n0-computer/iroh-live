//! Media sources for [`moq_video`] and [`moq_audio`] that upstream moq lacks.
//!
//! A source is an opened device, file or generator that produces raw frames,
//! on a thread of its own, for a publisher to encode. A [`VideoSource`] can
//! feed several encoders at once and offers a latest-wins preview through
//! [`VideoFrames`]. An [`AudioSource`] fans PCM out to every broadcast that
//! reads it.
//!
//! The crate has no dependency on iroh and none on a transport. Its [`Error`]
//! is the one `moq-ext-media-core` defines, which the player and the publisher
//! return too.
//!
//! # What is in it
//!
//! | Source | Constructor | Feature |
//! |---|---|---|
//! | Test picture | [`VideoSource::test_pattern`], [`AudioSource::test_pattern`] | `pattern` |
//! | Moving gradient | [`VideoSource::test_gradient`] | `pattern` |
//! | Sine tone | [`AudioSource::tone`] | `pattern` |
//! | WAV and MP3 file | [`AudioSource::file`] | `file` |
//! | Camera, display, window | [`VideoSource::capture`] | `capture` |
//! | Microphone | [`AudioSource::microphone`] | `capture` |
//! | Raspberry Pi camera through `rpicam-vid` | [`VideoSource::rpicam`], `rpicam_h264` | `rpicam` (Linux) |
//! | Pushed by the application | [`VideoSource::push`], [`AudioSource::push`], [`VideoSource::spawn`] | always |
//!
//! The crate README lists every feature flag. [`local_task::spawn`] runs a
//! future on a thread of its own for capture streams that are not `Send`.
//!
//! # Upstreaming candidates
//!
//! The pattern and tone generators belong in `moq-video`, the audio file
//! source in `moq-audio`, and the rpicam source as a `moq-video` capture
//! backend.
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
//! )
//! .expect("the pattern thread starts");
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
pub use moq_ext_media_core::{Error, SourceClosed};
use moq_video as video;

#[cfg(all(target_os = "linux", feature = "rpicam"))]
pub use self::source::{RpicamConfig, rpicam_h264};
pub use self::{
    frames::VideoFrames,
    source::{
        AudioFormat, AudioKind, AudioSource, Demand, DemandGuard, FrameSender, PcmFanout,
        VideoFormat, VideoSource, WeakPcmFanout,
    },
};
