//! Android MediaCodec H.264 encoder and decoder.
//!
//! Uses the NDK `AMediaCodec` API in synchronous ByteBuffer mode for
//! Phase 1. Future phases will add Surface-based zero-copy paths.

pub(crate) mod decoder;
pub(crate) mod encoder;
pub(crate) mod format;

pub use decoder::AndroidDecoder;
pub use encoder::AndroidEncoder;
