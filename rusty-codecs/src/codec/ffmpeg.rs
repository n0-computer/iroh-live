//! H.264 encoder and decoder backed by FFmpeg (libavcodec).
//!
//! Requires a system-installed FFmpeg with development headers. Enabled via
//! the `ffmpeg` feature flag (off by default).
//!
//! The encoder probes for hardware backends (VAAPI, VideoToolbox, NVENC, QSV,
//! AMF) at construction time, falling back to software `libx264`.

mod decoder;
mod encoder;

pub use decoder::FfmpegVideoDecoder;
pub use encoder::FfmpegH264Encoder;
