//! Media pipelines for capture, encode, decode, publish, and subscribe.
//!
//! This crate has no iroh dependency — it works with any transport that
//! implements [`transport::PacketSource`] and [`transport::PacketSink`].
//! The [`publish`] module handles encoding and writing, the [`subscribe`]
//! module handles reading and decoding, and [`pipeline`] orchestrates
//! encoder/decoder threads.

#[cfg(any_video_codec)]
pub mod adaptive;
pub mod audio_backend;
pub mod frame_channel;
pub mod net;
pub mod pipeline;
pub mod playout;
mod processing;
pub mod publish;
pub mod stats;
pub mod subscribe;
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;
pub mod transport;
mod util;

pub use audio_backend::{AudioBackend, AudioBackendOpts, AudioDevice};
// Re-export from rusty-capture
#[cfg(any(feature = "capture-camera", feature = "capture-screen"))]
pub use rusty_capture as capture;
#[cfg(feature = "wgpu")]
pub use rusty_codecs::render;
pub use rusty_codecs::{codec, config, format, test_sources, traits};
