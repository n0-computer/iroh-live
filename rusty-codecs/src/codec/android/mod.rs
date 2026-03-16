//! Android MediaCodec H.264 encoder and decoder.
//!
//! Provides two decoder variants:
//! - [`AndroidDecoder`]: ByteBuffer mode, CPU NV12-to-RGBA conversion.
//! - [`AndroidHwDecoder`]: ImageReader surface mode, zero-copy HardwareBuffer output.

pub(crate) mod decoder;
pub(crate) mod encoder;
pub(crate) mod format;
pub(crate) mod gpu_frame;
pub(crate) mod hw_decoder;

pub use decoder::AndroidDecoder;
pub use encoder::AndroidEncoder;
pub use hw_decoder::AndroidHwDecoder;
