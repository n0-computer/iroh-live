//! Encoder and decoder pipeline orchestration.
//!
//! Each pipeline runs on a dedicated OS thread and bridges between
//! sync codec APIs and async transport via channels. Pipelines are
//! created by the publish and subscribe modules — most callers do
//! not use this module directly.

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, error, trace};

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

    /// Shared playout clock for A/V synchronization.
    ///
    /// When `Some`, the video decode loop calls [`Sync::received`] on
    /// packet arrival and [`Sync::wait`] before emitting each decoded
    /// frame, replacing PTS-cadence pacing. When `None`, the legacy
    /// `FramePacer` is used instead.
    pub sync: Option<crate::sync::Sync>,
}

/// Forwards packets from an async [`PacketSource`] into an mpsc channel.
pub(crate) async fn forward_packets(
    mut source: impl PacketSource,
    sender: mpsc::Sender<MediaPacket>,
) {
    let mut count: u64 = 0;
    let start = std::time::Instant::now();
    loop {
        trace!("forward_packets: waiting for source.read()");
        let read_start = std::time::Instant::now();
        match source.read().await {
            Ok(Some(packet)) => {
                count += 1;
                let wait_ms = read_start.elapsed().as_millis();
                throttled_tracing::trace_every!(
                    Duration::from_secs(2),
                    count,
                    wait_ms,
                    keyframe = packet.is_keyframe,
                    pts_ms = packet.timestamp.as_millis(),
                    elapsed_s = start.elapsed().as_secs(),
                    "forward_packets: read packet"
                );
                if wait_ms > 1000 {
                    debug!(count, wait_ms, "forward_packets: source.read() was slow");
                }
                if sender.send(packet).await.is_err() {
                    debug!("forward_packets: decoder channel closed");
                    break;
                }
            }
            Ok(None) => {
                debug!(count, "forward_packets: source ended");
                break;
            }
            Err(err) => {
                error!(
                    count,
                    "forward_packets: failed to read from source: {err:#}"
                );
                break;
            }
        }
    }
}
