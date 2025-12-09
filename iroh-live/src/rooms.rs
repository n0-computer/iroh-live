use std::{str::FromStr, time::Duration};

use crate::{Live, live::RemoteTrack};
use iroh::{Endpoint, EndpointId};
use iroh_gossip::{Gossip, TopicId};
use n0_error::{Result, StdResultExt};
use n0_future::{FuturesUnordered, StreamExt, task::AbortOnDropHandle};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, error::TryRecvError};
use tracing::{info, warn};

#[derive(Debug, Serialize, Deserialize)]
enum Message {
    Announce(EndpointId),
}

pub struct Room {
    handle: RoomHandle,
    rx: mpsc::Receiver<RemoteTrack>,
}

pub type RoomEvents = mpsc::Receiver<RemoteTrack>;

pub struct RoomHandle {
    me: EndpointId,
    ticket: RoomTicket,
    tasks: Vec<AbortOnDropHandle<Result<()>>>,
}

impl RoomHandle {
    pub fn shutdown(&mut self) {
        self.tasks.clear();
    }
    pub fn ticket(&self) -> RoomTicket {
        let mut ticket = self.ticket.clone();
        ticket.bootstrap = vec![self.me];
        ticket
    }
}

impl Room {
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
            let mut connect_futs = FuturesUnordered::new();
            loop {
                tokio::select! {
                    res = announce_rx.recv() => {
                        let Some(endpoint_id) = res else {
                            break;
                        };
                        connect_futs.push(live.connect_and_subscribe(endpoint_id, &broadcast_name));
                    }
                    Some(res) = connect_futs.next(), if !connect_futs.is_empty() => {
                        match res {
                            Err(err) => warn!(endpoint=%endpoint_id.fmt_short(), ?err, "failed to connect"),
                            Ok(track) => {
                                if let Err(err) = track_tx.send(track).await {
                                    warn!(?err, "failed to forward track, abort conect loop");
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            n0_error::Ok(())
        });
        tasks.push(AbortOnDropHandle::new(task));

        Ok(Self {
            handle: RoomHandle {
                ticket,
                tasks,
                me: endpoint_id,
            },
            rx: track_rx,
        })
    }

    pub async fn recv(&mut self) -> Result<RemoteTrack> {
        self.rx.recv().await.std_context("sender stopped")
    }

    pub fn try_recv(&mut self) -> Result<RemoteTrack, TryRecvError> {
        self.rx.try_recv()
    }

    pub fn ticket(&self) -> RoomTicket {
        self.handle.ticket()
    }

    pub fn shutdown(&mut self) {
        self.handle.shutdown();
    }

    pub fn split(self) -> (RoomEvents, RoomHandle) {
        (self.rx, self.handle)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, derive_more::Display)]
#[display("{}", iroh_tickets::Ticket::serialize(self))]
pub struct RoomTicket {
    pub bootstrap: Vec<EndpointId>,
    pub topic_id: TopicId,
    pub broadcast_name: String,
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

    pub fn generate(broadcast_name: impl ToString) -> Self {
        Self {
            bootstrap: vec![],
            topic_id: TopicId::from_bytes(rand::random()),
            broadcast_name: broadcast_name.to_string(),
        }
    }

    pub fn new_from_env(broadcast_name: impl ToString) -> Result<Self> {
        let topic_id = match std::env::var("IROH_TOPIC") {
            Ok(topic) => TopicId::from_bytes(
                data_encoding::HEXLOWER
                    .decode(topic.as_bytes())
                    .std_context("invalid hex")?
                    .as_slice()
                    .try_into()
                    .std_context("invalid length")?,
            ),
            Err(_) => {
                let topic = TopicId::from_bytes(rand::random());
                println!(
                    "Created new topic. Reuse with IROH_TOPIC={}",
                    data_encoding::HEXLOWER.encode(topic.as_bytes())
                );
                topic
            }
        };
        Ok(Self::new(topic_id, vec![], broadcast_name))
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
