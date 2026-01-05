use iroh::{Endpoint, EndpointAddr};
use iroh_moq::{Moq, MoqProtocolHandler, MoqSession};
use moq_lite::BroadcastProducer;
use moq_media::{
    av::{AudioSink, Decoders, PlaybackConfig},
    subscribe::{AvRemoteTrack, SubscribeBroadcast},
};
use n0_error::Result;
use tracing::info;

#[derive(Clone)]
pub struct Live {
    pub moq: Moq,
}

impl Live {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            moq: Moq::new(endpoint),
        }
    }

    pub async fn connect(&self, remote: impl Into<EndpointAddr>) -> Result<MoqSession> {
        self.moq.connect(remote).await
    }

    pub async fn connect_and_subscribe(
        &self,
        remote: impl Into<EndpointAddr>,
        broadcast_name: &str,
    ) -> Result<(MoqSession, SubscribeBroadcast)> {
        let mut session = self.connect(remote).await?;
        info!(id=%session.conn().remote_id(), "new peer connected");
        let broadcast = session.subscribe(broadcast_name).await?;
        let broadcast = SubscribeBroadcast::new(broadcast_name.to_string(), broadcast).await?;
        Ok((session, broadcast))
    }

    pub async fn watch_and_listen<D: Decoders>(
        &self,
        remote: impl Into<EndpointAddr>,
        broadcast_name: &str,
        audio_out: impl AudioSink,
        config: PlaybackConfig,
    ) -> Result<(MoqSession, AvRemoteTrack)> {
        let (session, broadcast) = self.connect_and_subscribe(remote, &broadcast_name).await?;
        let track = broadcast.watch_and_listen::<D>(audio_out, config)?;
        Ok((session, track))
    }

    pub fn protocol_handler(&self) -> MoqProtocolHandler {
        self.moq.protocol_handler()
    }

    pub async fn publish(&self, name: impl ToString, producer: BroadcastProducer) -> Result<()> {
        self.moq.publish(name, producer).await
    }

    pub fn shutdown(&self) {
        self.moq.shutdown();
    }
}
