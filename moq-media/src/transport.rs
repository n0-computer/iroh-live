use std::fmt;
use std::future::Future;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::format::MediaPacket;

/// Async source of encoded media packets.
/// Implemented by [`MoqPacketSource`] (network) and [`PipeSource`] (local).
pub trait PacketSource: Send + 'static {
    fn read(&mut self) -> impl Future<Output = Result<Option<MediaPacket>>> + Send;
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

/// Create an in-memory media pipe for local encode→decode without network.
pub fn media_pipe(capacity: usize) -> (PipeSink, PipeSource) {
    let (tx, rx) = mpsc::channel(capacity);
    (PipeSink(tx), PipeSource(rx))
}

/// Sending end of an in-memory media pipe.
#[derive(Debug)]
pub struct PipeSink(mpsc::Sender<MediaPacket>);

impl PipeSink {
    /// Send a packet into the pipe. Blocks until space is available.
    pub fn send_blocking(&self, packet: MediaPacket) -> Result<()> {
        self.0
            .blocking_send(packet)
            .map_err(|_| anyhow::anyhow!("pipe closed"))
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
