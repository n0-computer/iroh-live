use std::{fmt, future::Future};

use anyhow::Result;
use hang::container::OrderedProducer;
use moq_lite::TrackProducer;
use tokio::sync::mpsc;

use crate::format::{EncodedFrame, MediaPacket};

/// Provides async reads of encoded media packets.
///
/// Implemented by [`MoqPacketSource`] (network) and [`PipeSource`] (local).
pub trait PacketSource: Send + 'static {
    fn read(&mut self) -> impl Future<Output = Result<Option<MediaPacket>>> + Send;
}

/// Accepts encoded media packets synchronously.
///
/// Implemented by [`MoqPacketSink`] (network) and [`PipeSink`] (local).
pub trait PacketSink: Send + 'static {
    /// Writes an encoded frame. Keyframe grouping is handled internally
    /// (e.g., MoQ transport starts a new group on keyframes).
    fn write(&mut self, packet: EncodedFrame) -> Result<()>;

    /// Signals that no more packets will be written.
    fn finish(&mut self) -> Result<()>;
}

/// Wraps a hang [`OrderedConsumer`](hang::container::OrderedConsumer) as a [`PacketSource`].
pub struct MoqPacketSource(pub hang::container::OrderedConsumer);

impl fmt::Debug for MoqPacketSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqPacketSource").finish()
    }
}

impl PacketSource for MoqPacketSource {
    async fn read(&mut self) -> Result<Option<MediaPacket>> {
        match self.0.read().await {
            Ok(Some(frame)) => Ok(Some(frame.into())),
            Ok(None) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Wraps a hang [`OrderedProducer`] as a [`PacketSink`].
///
/// Handles keyframe grouping: calls `keyframe()` on the underlying producer
/// when an `EncodedFrame` with `is_keyframe = true` is written.
pub struct MoqPacketSink(OrderedProducer);

impl MoqPacketSink {
    /// Creates a new sink from a [`TrackProducer`].
    pub fn new(producer: TrackProducer) -> Self {
        Self(OrderedProducer::new(producer))
    }
}

impl fmt::Debug for MoqPacketSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqPacketSink").finish()
    }
}

impl PacketSink for MoqPacketSink {
    fn write(&mut self, packet: EncodedFrame) -> Result<()> {
        if packet.is_keyframe {
            self.0.keyframe()?;
        }
        self.0.write(packet.to_hang_frame())?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.0.finish()?;
        Ok(())
    }
}

/// Creates an in-memory media pipe for local encode→decode without network.
pub fn media_pipe(capacity: usize) -> (PipeSink, PipeSource) {
    let (tx, rx) = mpsc::channel(capacity);
    (PipeSink(tx), PipeSource(rx))
}

/// Sending end of an in-memory media pipe.
#[derive(Debug)]
pub struct PipeSink(mpsc::Sender<MediaPacket>);

impl PipeSink {
    /// Sends a packet into the pipe. Blocks until space is available.
    pub fn send_blocking(&self, packet: MediaPacket) -> Result<()> {
        self.0
            .blocking_send(packet)
            .map_err(|_| anyhow::anyhow!("pipe closed"))
    }
}

impl PacketSink for PipeSink {
    fn write(&mut self, packet: EncodedFrame) -> Result<()> {
        let media_pkt = MediaPacket {
            timestamp: packet.timestamp,
            payload: packet.payload.into(),
            is_keyframe: packet.is_keyframe,
        };
        self.send_blocking(media_pkt)
    }

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Receiving end of an in-memory media pipe.
#[derive(Debug)]
pub struct PipeSource(mpsc::Receiver<MediaPacket>);

impl PacketSource for PipeSource {
    async fn read(&mut self) -> Result<Option<MediaPacket>> {
        Ok(self.0.recv().await)
    }
}
