//! A broadcast that encodes sources into renditions, apart from any transport.
//!
//! A [`LocalBroadcast`] holds a catalog, a media clock, one video slot and one
//! audio slot. The video slot opens its source once and encodes it into a
//! ladder of renditions, each encoding only while someone watches it. A
//! transport publishes the broadcast by creating the `broadcast::Producer` it
//! fills ([`LocalBroadcast::new_in`], or `TryFrom`), so readers get it without
//! a copy. [`LocalBroadcast::consume`] reads it in-process.
//!
//! The sources come from `moq-ext-sources`, and the codecs from `moq_video`
//! and `moq_audio`. Nothing here depends on iroh or on a player.
//!
//! # Example
//!
//! ```no_run
//! # fn example() -> Result<(), moq_ext_publisher::Error> {
//! use moq_ext_publisher::{LocalBroadcast, VideoEncoding, VideoRendition};
//! use moq_ext_sources::VideoSource;
//! use moq_video as video;
//!
//! let source = VideoSource::test_pattern(
//!     video::Size::new(1280, 720),
//!     video::FrameRate::new(30, 1).expect("a valid rate"),
//! )
//! .expect("the pattern thread starts");
//! let broadcast = LocalBroadcast::new();
//! broadcast.set_video(
//!     source,
//!     VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
//! )?;
//! # Ok(())
//! # }
//! ```
//!
//! A microphone is opened inside the publication that encodes it, so a device
//! that fails after `AudioSource::microphone` returned shows in
//! [`LocalBroadcast::status`]. A failed microphone is not opened again: the
//! audio slot stays failed until the audio is set again.

mod publish;
mod stats;

pub use moq_ext_media_core::{Error, SlotState, SourceClosed};

pub use self::{
    publish::{
        AudioEncoding, LocalBroadcast, PublishStatus, RenditionState, VideoEncoding, VideoRendition,
    },
    stats::{AudioEncodeStats, EncodeStats, PublishStats},
};
