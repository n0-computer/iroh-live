//! Media that arrives encoded, for a broadcast to publish as it is.
//!
//! A hardware camera that encodes on its own and a media file both hand over
//! encoded frames, which a broadcast publishes without an encoder.
//! `MediaFile::encoded_video` lists the trade-offs.

use std::time::Instant;

use bytes::Bytes;
use n0_future::boxed::BoxStream;

use crate::Error;

/// One encoded frame and the time it presents at.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// The frame: one length-prefixed H.264 access unit, or one raw AAC frame.
    pub data: Bytes,
    /// The presentation time, on the producer's own timeline.
    ///
    /// Frames come in decode order, so with B-frames the times do not rise
    /// monotonically.
    pub timestamp: moq_net::Timestamp,
    /// The wall-clock instant `timestamp` stands for.
    ///
    /// A broadcast maps every timestamp of a stream with the offset of its
    /// first frame's `at`, so an audio and a video stream that agree on it
    /// stay in sync wherever each starts.
    pub at: Instant,
}

/// Encoded video for `LocalBroadcast::set_encoded_video`.
#[derive(derive_more::Debug)]
#[non_exhaustive]
pub enum EncodedVideo {
    /// An H.264 Annex-B byte stream, which may split anywhere.
    ///
    /// Access units are found by their start codes, and the stream's own
    /// parameter sets describe it, so it should repeat them before every
    /// keyframe. The broadcast stamps each access unit when it arrives.
    AnnexB(#[debug(skip)] BoxStream<Bytes>),
    /// H.264 access units in decode order, length-prefixed as an MP4 file stores them.
    Avc1 {
        /// The H.264 configuration record (avcC) that opens the stream.
        avcc: Bytes,
        /// The access units, which end with the first error.
        #[debug(skip)]
        frames: BoxStream<Result<EncodedFrame, Error>>,
    },
}

/// Encoded audio for `LocalBroadcast::set_encoded_audio`.
#[derive(derive_more::Debug)]
#[non_exhaustive]
pub enum EncodedAudio {
    /// Raw AAC frames, one per [`EncodedFrame`], without ADTS headers.
    Aac {
        /// The AudioSpecificConfig that opens the stream.
        config: Bytes,
        /// The frames, which end with the first error.
        #[debug(skip)]
        frames: BoxStream<Result<EncodedFrame, Error>>,
    },
}
