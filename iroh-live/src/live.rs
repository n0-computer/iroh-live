use iroh::{Endpoint, EndpointAddr};
use iroh_moq::{Moq, MoqProtocolHandler, MoqSession};
use moq_lite::BroadcastProducer;
use n0_error::Result;
use tracing::info;

use crate::{
    av::{AudioSink, Decoders, PlaybackConfig},
    subscribe::{AudioTrack, SubscribeBroadcast, WatchTrack},
};

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

// pub struct RemoteTrack {
//     pub broadcast: SubscribeBroadcast,
//     pub session: MoqSession,
// }

// impl RemoteTrack {
//     pub async fn connect(
//         moq: &Moq,
//         remote: impl Into<EndpointAddr>,
//         broadcast_name: &str,
//     ) -> Result<Self> {
//         let mut session = moq.connect(remote).await?;
//         info!(id=%session.conn().remote_id(), "new peer connected");
//         let broadcast = session.subscribe(broadcast_name).await?;
//         let broadcast = SubscribeBroadcast::new(broadcast).await?;
//         info!(id=%session.conn().remote_id(), "subscribed");
//         Ok(RemoteTrack { session, broadcast })
//     }

//     pub async fn start<D: Decoders>(
//         self,
//         audio_ctx: &AudioBackend,
//         config: PlaybackConfig,
//     ) -> Result<AvRemoteTrack> {
//         AvRemoteTrack::new::<D>(self, audio_ctx, config).await
//     }
// }

pub struct AvRemoteTrack {
    pub broadcast: SubscribeBroadcast,
    // pub session: Option<MoqSession>,
    pub video: Option<WatchTrack>,
    pub audio: Option<AudioTrack>,
}

impl AvRemoteTrack {
    // pub async fn new<D: Decoders>(
    //     broadcast: SubscribeBroadcast,
    //     // track: RemoteTrack,
    //     audio_ctx: &AudioBackend,
    //     config: PlaybackConfig,
    // ) -> Result<Self> {
    //     let audio_out = audio_ctx.default_output().await?;
    //     let audio = track
    //         .broadcast
    //         .listen_with::<D::Audio>(config.quality, audio_out)
    //         .inspect_err(|err| tracing::warn!("no audio track: {err}"))
    //         .ok();
    //     let video = track
    //         .broadcast
    //         .watch_with::<D::Video>(&config.playback, config.quality)
    //         .inspect_err(|err| tracing::warn!("no video track: {err}"))
    //         .ok();
    //     Ok(Self {
    //         broadcast: track.broadcast,
    //         session: Some(track.session),
    //         audio,
    //         video,
    //     })
    // }

    pub fn new<D: Decoders>(
        broadcast: SubscribeBroadcast,
        audio_out: impl AudioSink,
        config: PlaybackConfig,
    ) -> Result<Self> {
        let audio = broadcast
            .listen_with::<D::Audio>(config.quality, audio_out)
            .inspect_err(|err| tracing::warn!("no audio track: {err}"))
            .ok();
        let video = broadcast
            .watch_with::<D::Video>(&config.playback, config.quality)
            .inspect_err(|err| tracing::warn!("no video track: {err}"))
            .ok();
        Ok(Self {
            broadcast,
            // session: None,
            audio,
            video,
        })
    }
}
