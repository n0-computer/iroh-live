//! Encoder and decoder pipeline orchestration.
//!
//! Each pipeline runs on a dedicated OS thread and bridges between
//! sync codec APIs and async transport via channels. Pipelines are
//! created by the publish and subscribe modules — most callers do
//! not use this module directly.

use tokio::sync::mpsc;
use tracing::{debug, error};

use crate::{format::MediaPacket, transport::PacketSource};

mod audio_decode;
mod audio_encode;
mod video_decode;
mod video_encode;

pub use audio_decode::AudioDecoderPipeline;
pub use audio_encode::AudioEncoderPipeline;
pub use video_decode::{VideoDecoderFrames, VideoDecoderHandle, VideoDecoderPipeline};
pub use video_encode::{PreEncodedVideoPipeline, VideoEncoderPipeline};

/// Internal parameters for decoder pipelines.
///
/// Bundles shared state that decode threads need for stats and coordination.
/// Callers configure playback through [`crate::subscribe::RemoteBroadcast`];
/// they do not construct this directly except in tests and tools.
#[derive(Debug, Clone, Default)]
pub struct PipelineContext {
    /// Stats collectors for metrics and timeline.
    pub stats: crate::stats::DecodeStats,
}

/// Forwards packets from an async [`PacketSource`] into an mpsc channel.
pub(crate) async fn forward_packets(
    mut source: impl PacketSource,
    sender: mpsc::Sender<MediaPacket>,
) {
    loop {
        match source.read().await {
            Ok(Some(packet)) => {
                if sender.send(packet).await.is_err() {
                    debug!("forward_packets: decoder channel closed");
                    break;
                }
            }
            Ok(None) => {
                debug!("forward_packets: source ended");
                break;
            }
            Err(err) => {
                error!("forward_packets: failed to read from source: {err:#}");
                break;
            }
        }
    }
}
