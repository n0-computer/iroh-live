use std::{collections::HashSet, pin::Pin, sync::Arc, time::Duration};

use bytes::Bytes;
use iroh::{EndpointId, SecretKey};
use iroh_gossip::Gossip;
use iroh_moq::MoqSession;
use iroh_smol_kv::{ExpiryConfig, Filter, SignedValue, Subscribe, SubscribeMode, WriteScope};
use moq_lite::BroadcastProducer;
use moq_media::subscribe::RemoteBroadcast;
use n0_error::{Result, StackResultExt, StdResultExt, anyerr};
use n0_future::{FuturesUnordered, StreamExt, task::AbortOnDropHandle};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc::{self, error::TryRecvError},
    task,
};
use tracing::{Instrument, debug, error_span, warn};

pub use self::{publisher::RoomPublisherSync, ticket::RoomTicket};
use crate::Live;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + Sync + 'static>>;

mod publisher;

/// Multi-party room backed by gossip-based peer discovery.
///
/// Peers announce their broadcasts via a shared gossip topic. When a
/// remote peer announces, the room automatically connects and subscribes,
/// emitting [`RoomEvent`]s for each successful subscription.
///
/// Use [`split`](Room::split) to separate the event stream from the
/// publish handle when you need to move them to different tasks.
#[derive(Debug)]
pub struct Room {
    handle: RoomHandle,
    events: mpsc::Receiver<RoomEvent>,
}

/// Receiver half of a room's event stream.
///
/// Obtained from [`Room::split`].
pub type RoomEvents = mpsc::Receiver<RoomEvent>;

/// Cloneable handle for publishing into a [`Room`].
///
/// Obtained from [`Room::split`]. Can be shared across tasks.
#[derive(Debug, Clone)]
pub struct RoomHandle {
    me: EndpointId,
    ticket: RoomTicket,
    tx: mpsc::Sender<ApiMessage>,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl RoomHandle {
    /// Returns a ticket that includes this peer as a bootstrap node.
    pub fn ticket(&self) -> RoomTicket {
        let mut ticket = self.ticket.clone();
        ticket.bootstrap = vec![self.me];
        ticket
    }

    /// Publishes a broadcast into the room, announcing it to all peers.
    pub async fn publish(&self, name: impl ToString, producer: BroadcastProducer) -> Result<()> {
        self.tx
            .send(ApiMessage::Publish {
                name: name.to_string(),
                producer,
            })
            .await
            .map_err(|_| anyerr!("room actor died"))
    }
}

impl Room {
    /// Joins a room using the given ticket.
    ///
    /// Requires gossip to be enabled on [`Live`]. Spawns an internal actor
    /// that handles peer discovery, connection, and subscription.
    pub async fn new(live: &Live, ticket: RoomTicket) -> Result<Self> {
        let gossip = live
            .gossip()
            .context("Cannot join room: Gossip is disabled")?
            .clone();
        let endpoint = live.endpoint();
        let endpoint_id = endpoint.id();
        let (actor_tx, actor_rx) = mpsc::channel(16);
        let (event_tx, event_rx) = mpsc::channel(16);

        let actor = Actor::new(
            endpoint.secret_key(),
            live.clone(),
            event_tx,
            gossip,
            ticket.clone(),
        )
        .await?;
        let actor_task = task::spawn(
            async move { actor.run(actor_rx).await }
                .instrument(error_span!("RoomActor", id = ticket.topic_id.fmt_short())),
        );

        Ok(Self {
            handle: RoomHandle {
                ticket,
                me: endpoint_id,
                tx: actor_tx,
                _actor_handle: Arc::new(AbortOnDropHandle::new(actor_task)),
            },
            events: event_rx,
        })
    }

    /// Waits for the next room event.
    pub async fn recv(&mut self) -> Result<RoomEvent> {
        self.events.recv().await.std_context("sender stopped")
    }

    /// Returns the next room event without blocking, or an error if none is ready.
    pub fn try_recv(&mut self) -> Result<RoomEvent, TryRecvError> {
        self.events.try_recv()
    }

    /// Returns a ticket for this room that includes this peer as a bootstrap node.
    pub fn ticket(&self) -> RoomTicket {
        self.handle.ticket()
    }

    /// Splits the room into its event stream and publish handle.
    ///
    /// Useful when the event loop and the publisher live in different tasks.
    pub fn split(self) -> (RoomEvents, RoomHandle) {
        (self.events, self.handle)
    }

    /// Publishes a broadcast into the room, announcing it to all peers.
    pub async fn publish(&self, name: impl ToString, producer: BroadcastProducer) -> Result<()> {
        self.handle.publish(name, producer).await
    }
}

enum ApiMessage {
    Publish {
        name: String,
        producer: BroadcastProducer,
    },
}

/// Events emitted by a [`Room`] as peers join and publish broadcasts.
#[derive(Debug)]
pub enum RoomEvent {
    /// A remote peer announced its available broadcasts via gossip.
    RemoteAnnounced {
        /// The announcing peer's endpoint ID.
        remote: EndpointId,
        /// Broadcast names the peer is publishing.
        broadcasts: Vec<String>,
    },
    /// Reserved for future use. Not currently emitted by the room actor.
    RemoteConnected {
        /// The connected session.
        session: MoqSession,
    },
    /// Successfully subscribed to a remote peer's broadcast.
    BroadcastSubscribed {
        /// The MoQ session with the remote peer.
        session: MoqSession,
        /// The subscribed broadcast, ready for video/audio decoding.
        broadcast: RemoteBroadcast,
    },
}

const PEER_STATE_KEY: &[u8] = b"s";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerState {
    broadcasts: Vec<String>,
}

type ConnectingFutures =
    FuturesUnordered<BoxFuture<(BroadcastId, Result<(MoqSession, RemoteBroadcast)>)>>;
type KvEntry = (EndpointId, Bytes, SignedValue);

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, derive_more::Display)]
#[display("{}:{}", _0.fmt_short(), _1)]
struct BroadcastId(EndpointId, String);

struct Actor {
    me: EndpointId,
    _gossip: Gossip,
    live: Live,
    active_subscribe: HashSet<BroadcastId>,
    active_publish: HashSet<String>,
    connecting: ConnectingFutures,
    subscribe_closed: FuturesUnordered<BoxFuture<BroadcastId>>,
    publish_closed: FuturesUnordered<BoxFuture<String>>,
    event_tx: mpsc::Sender<RoomEvent>,
    kv: iroh_smol_kv::Client,
    kv_writer: WriteScope,
}

impl Actor {
    async fn new(
        me: &SecretKey,
        live: Live,
        event_tx: mpsc::Sender<RoomEvent>,
        gossip: Gossip,
        ticket: RoomTicket,
    ) -> Result<Self> {
        let topic = gossip
            .subscribe(ticket.topic_id, ticket.bootstrap.clone())
            .await?;
        let kv = iroh_smol_kv::Client::local(
            topic,
            iroh_smol_kv::Config {
                anti_entropy_interval: Duration::from_secs(60),
                fast_anti_entropy_interval: Duration::from_secs(1),
                expiry: Some(ExpiryConfig {
                    check_interval: Duration::from_secs(10),
                    horizon: Duration::from_secs(60 * 2),
                }),
            },
        );
        let kv_writer = kv.write(me.clone());
        Ok(Self {
            me: me.public(),
            live,
            _gossip: gossip,
            active_subscribe: Default::default(),
            active_publish: Default::default(),
            connecting: Default::default(),
            subscribe_closed: Default::default(),
            publish_closed: Default::default(),
            event_tx,
            kv,
            kv_writer,
        })
    }

    pub(crate) async fn run(mut self, mut inbox: mpsc::Receiver<ApiMessage>) {
        let updates = self
            .kv
            .subscribe_with_opts(Subscribe {
                mode: SubscribeMode::Both,
                filter: Filter::ALL,
            })
            .stream();
        tokio::pin!(updates);

        loop {
            tokio::select! {
                Some(update) = updates.next() => {
                    match update {
                        Err(err) => warn!("gossip kv update failed: {err:#}"),
                        Ok(update) => self.handle_gossip_update(update).await,
                    }
                }
                msg = inbox.recv() => {
                    match msg {
                        None => break,
                        Some(msg) => self.handle_api_message(msg).await
                    }
                }
                Some((id, res)) = self.connecting.next(), if !self.connecting.is_empty() => {
                    match res {
                        Ok((session, broadcast)) => {
                            let closed_fut = broadcast.closed();
                            self.event_tx.send(RoomEvent::BroadcastSubscribed { session, broadcast }).await.ok();
                            self.subscribe_closed.push(Box::pin(async move {
                                closed_fut.await;
                                id
                            }))
                        }
                        Err(err) => {
                            self.active_subscribe.remove(&id);
                            warn!("Subscribing to broadcast {id} failed: {err:#}");
                        }
                    }
                }
                Some(id) = self.subscribe_closed.next(), if !self.subscribe_closed.is_empty() => {
                    debug!("broadcast closed: {id}");
                    self.active_subscribe.remove(&id);
                }
                Some(name) = self.publish_closed.next(), if !self.publish_closed.is_empty() => {
                    self.active_publish.remove(&name);
                    self.update_kv().await;
                }
            }
        }
    }

    async fn handle_api_message(&mut self, msg: ApiMessage) {
        match msg {
            ApiMessage::Publish { name, producer } => {
                let consume = producer.consume();
                self.live
                    .publish_producer(name.clone(), producer)
                    .await
                    .ok();
                self.active_publish.insert(name.clone());
                self.publish_closed.push(Box::pin(async move {
                    consume.closed().await;
                    name
                }));
                self.update_kv().await;
            }
        }
    }

    async fn handle_gossip_update(&mut self, entry: KvEntry) {
        let (remote, key, value) = entry;
        if remote == self.me || key != PEER_STATE_KEY {
            return;
        }
        let Ok(value) = postcard::from_bytes::<PeerState>(&value.value) else {
            return;
        };
        let PeerState { broadcasts } = value;
        for name in broadcasts.clone() {
            let id = BroadcastId(remote, name.clone());
            if !self.active_subscribe.insert(id.clone()) {
                continue;
            }
            let live = self.live.clone();
            self.connecting.push(Box::pin(async move {
                let session = live.subscribe(remote, &name).await;
                (id, session)
            }));
        }
        self.event_tx
            .send(RoomEvent::RemoteAnnounced { remote, broadcasts })
            .await
            .ok();
    }

    async fn update_kv(&self) {
        let state = PeerState {
            broadcasts: self.active_publish.iter().cloned().collect(),
        };
        if let Err(err) = self
            .kv_writer
            .put(
                PEER_STATE_KEY,
                postcard::to_stdvec(&state).expect("PeerState serialization is infallible"),
            )
            .await
        {
            warn!("failed to update gossip kv: {err:#}");
        }
    }
}

mod ticket {
    use std::{env, str::FromStr};

    use iroh::EndpointId;
    use iroh_gossip::TopicId;
    use n0_error::{Result, StdResultExt};
    use serde::{Deserialize, Serialize};

    /// Ticket for joining a room.
    ///
    /// Contains the gossip topic ID and optional bootstrap peer IDs.
    /// Serializes to a compact string via the `iroh_tickets` crate.
    #[derive(Debug, Serialize, Deserialize, Clone, derive_more::Display)]
    #[display("{}", iroh_tickets::Ticket::serialize(self))]
    pub struct RoomTicket {
        /// Peers to contact initially for gossip bootstrap.
        pub bootstrap: Vec<EndpointId>,
        /// The gossip topic that identifies this room.
        pub topic_id: TopicId,
    }

    impl RoomTicket {
        /// Creates a ticket with the given topic and bootstrap peers.
        pub fn new(topic_id: TopicId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
            Self {
                bootstrap: bootstrap.into_iter().collect(),
                topic_id,
            }
        }

        /// Generates a new room with a random topic ID and no bootstrap peers.
        pub fn generate() -> Self {
            Self {
                bootstrap: vec![],
                topic_id: TopicId::from_bytes(rand::random()),
            }
        }

        /// Creates a ticket from environment variables.
        ///
        /// Reads `IROH_LIVE_ROOM` for a full ticket string, or
        /// `IROH_LIVE_TOPIC` for a hex-encoded topic ID. Generates a
        /// random topic if neither is set.
        pub fn new_from_env() -> Result<Self> {
            if let Ok(value) = env::var("IROH_LIVE_ROOM") {
                value
                    .parse()
                    .std_context("failed to parse ticket from IROH_LIVE_ROOM environment variable")
            } else {
                let topic_id = match env::var("IROH_LIVE_TOPIC") {
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
                Ok(Self::new(topic_id, vec![]))
            }
        }
    }

    impl FromStr for RoomTicket {
        type Err = iroh_tickets::ParseError;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            iroh_tickets::Ticket::deserialize(s)
        }
    }

    impl iroh_tickets::Ticket for RoomTicket {
        const KIND: &'static str = "room";

        fn to_bytes(&self) -> Vec<u8> {
            postcard::to_stdvec(self).expect("RoomTicket serialization is infallible")
        }

        fn from_bytes(bytes: &[u8]) -> Result<Self, iroh_tickets::ParseError> {
            let ticket = postcard::from_bytes(bytes)?;
            Ok(ticket)
        }
    }
}
