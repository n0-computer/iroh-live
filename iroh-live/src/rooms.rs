use std::{str::FromStr, time::Duration};

use crate::{
    Live, LiveSession,
    audio::AudioBackend,
    av::{Decoders, Quality},
    subscribe::{AudioTrack, SubscribeBroadcast, WatchTrack},
};
use iroh::{Endpoint, EndpointId};
use iroh_gossip::{Gossip, TopicId};
use n0_error::{Result, StdResultExt};
use n0_future::{StreamExt, task::AbortOnDropHandle};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, error::TryRecvError};
use tracing::{info, warn};

#[derive(Debug, Serialize, Deserialize)]
enum Message {
    Announce(EndpointId),
}

pub struct Room {
    ticket: RoomTicket,
    me: EndpointId,
    tasks: Vec<AbortOnDropHandle<Result<()>>>,
    rx: mpsc::Receiver<RemoteTrack>,
}

impl Room {
    pub fn shutdown(&mut self) {
        self.tasks.clear();
    }

    pub async fn new(
        endpoint: &Endpoint,
        gossip: Gossip,
        live: Live,
        ticket: RoomTicket,
    ) -> Result<Self> {
        let broadcast_name = ticket.broadcast_name.clone();
        let endpoint_id = endpoint.id();

        let mut tasks = vec![];

        let (gossip_sender, mut gossip_receiver) = gossip
            .subscribe(ticket.topic_id, ticket.bootstrap.clone())
            .await?
            .split();

        // Announce ourselves on the gossip topic every other second.
        let task = tokio::task::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                // TODO: sign
                let message = Message::Announce(endpoint_id);
                let message = postcard::to_stdvec(&message).anyerr()?;
                if let Err(err) = gossip_sender.broadcast(message.into()).await {
                    warn!("failed to broadcast on gossip: {err:?}");
                    break;
                }
            }
            n0_error::Ok(())
        });
        tasks.push(AbortOnDropHandle::new(task));

        // Listen for announcements on the gossip topic.
        let (announce_tx, mut announce_rx) = mpsc::channel(16);
        let task = tokio::task::spawn(async move {
            while let Some(event) = gossip_receiver.next().await {
                let event = event?;
                match event {
                    iroh_gossip::api::Event::Received(message) => {
                        let Ok(message) = postcard::from_bytes::<Message>(&message.content) else {
                            continue;
                        };
                        match message {
                            Message::Announce(endpoint_id) => {
                                if let Err(_) = announce_tx.send(endpoint_id).await {
                                    break;
                                }
                            }
                        }
                    }
                    iroh_gossip::api::Event::NeighborUp(neighbor) => {
                        info!("gossip neighbor up: {neighbor}")
                    }
                    iroh_gossip::api::Event::NeighborDown(neighbor) => {
                        info!("gossip neighbor down: {neighbor}")
                    }
                    _ => {}
                }
            }
            n0_error::Ok(())
        });
        tasks.push(AbortOnDropHandle::new(task));

        // Connect and subscribe to all peers that announced themselves.
        let (track_tx, track_rx) = mpsc::channel(16);
        let task = tokio::task::spawn(async move {
            while let Some(endpoint_id) = announce_rx.recv().await {
                match RemoteTrack::connect(&live, endpoint_id, &broadcast_name).await {
                    Err(err) => {
                        warn!(endpoint=%endpoint_id.fmt_short(), ?err, "failed to connect");
                    }
                    Ok(track) => {
                        if let Err(err) = track_tx.send(track).await {
                            warn!(?err, "failed to forward track, abort conect loop");
                            break;
                        }
                    }
                }
            }
            n0_error::Ok(())
        });
        tasks.push(AbortOnDropHandle::new(task));

        Ok(Self {
            ticket,
            me: endpoint_id,
            rx: track_rx,
            tasks,
        })
    }

    pub async fn recv(&mut self) -> Result<RemoteTrack> {
        self.rx.recv().await.std_context("sender stopped")
    }

    pub fn try_recv(&mut self) -> Result<RemoteTrack, TryRecvError> {
        self.rx.try_recv()
    }

    pub fn ticket(&self) -> RoomTicket {
        let mut ticket = self.ticket.clone();
        ticket.bootstrap = vec![self.me];
        ticket
    }
}

pub struct RemoteTrack {
    pub broadcast: SubscribeBroadcast,
    pub session: LiveSession,
}

impl RemoteTrack {
    pub async fn connect(
        live: &Live,
        endpoint_id: EndpointId,
        broadcast_name: &str,
    ) -> Result<Self> {
        let mut session = live.connect(endpoint_id).await?;
        info!(id=%session.conn().remote_id(), "new peer connected");
        let broadcast = session.subscribe(broadcast_name).await?;
        let broadcast = SubscribeBroadcast::new(broadcast).await?;
        info!(id=%session.conn().remote_id(), "subscribed");
        Ok(RemoteTrack { session, broadcast })
    }

    pub async fn start<D: Decoders>(
        self,
        audio_ctx: &AudioBackend,
        quality: Quality,
    ) -> Result<AvRemoteTrack> {
        AvRemoteTrack::new::<D>(self, audio_ctx, quality).await
    }
}

pub struct AvRemoteTrack {
    pub broadcast: SubscribeBroadcast,
    pub session: LiveSession,
    pub video: Option<WatchTrack>,
    pub audio: Option<AudioTrack>,
}

impl AvRemoteTrack {
    pub async fn new<D: Decoders>(
        track: RemoteTrack,
        audio_ctx: &AudioBackend,
        quality: Quality,
    ) -> Result<Self> {
        let audio_out = audio_ctx.default_output().await?;
        let audio = track
            .broadcast
            .listen_with::<D::Audio>(quality, audio_out)
            .inspect_err(|err| tracing::warn!("no audio track: {err}"))
            .ok();
        let video = track
            .broadcast
            .watch_with::<D::Video>(&Default::default(), quality)
            .inspect_err(|err| tracing::warn!("no video track: {err}"))
            .ok();
        Ok(Self {
            broadcast: track.broadcast,
            session: track.session,
            audio,
            video,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, derive_more::Display)]
#[display("{}", iroh_tickets::Ticket::serialize(self))]
pub struct RoomTicket {
    bootstrap: Vec<EndpointId>,
    topic_id: TopicId,
    broadcast_name: String,
}

impl RoomTicket {
    pub fn new(
        topic_id: TopicId,
        bootstrap: impl IntoIterator<Item = EndpointId>,
        broadcast_name: impl ToString,
    ) -> Self {
        Self {
            bootstrap: bootstrap.into_iter().collect(),
            topic_id,
            broadcast_name: broadcast_name.to_string(),
        }
    }
}

impl FromStr for RoomTicket {
    type Err = iroh_tickets::ParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        iroh_tickets::Ticket::deserialize(s)
    }
}

impl iroh_tickets::Ticket for RoomTicket {
    const KIND: &'static str = "room";

    fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).unwrap()
    }

    fn from_bytes(bytes: &[u8]) -> std::result::Result<Self, iroh_tickets::ParseError> {
        let ticket = postcard::from_bytes(bytes)?;
        Ok(ticket)
    }
}
