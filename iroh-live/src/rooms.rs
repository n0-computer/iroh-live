use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use iroh::{Endpoint, EndpointId, SecretKey};
use iroh_gossip::Gossip;
use iroh_moq::MoqSession;
use iroh_smol_kv::{ExpiryConfig, Filter, SignedValue, Subscribe, SubscribeMode, WriteScope};
use moq_lite::BroadcastProducer;
use moq_media::subscribe::SubscribeBroadcast;
use n0_error::{Result, StdResultExt, anyerr};
use n0_future::FuturesUnordered;
use n0_future::{StreamExt, task::AbortOnDropHandle};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, error::TryRecvError};
use tokio::task;
use tracing::{Instrument, debug, error_span, warn};

use crate::Live;

pub use self::publisher::{PublishOpts, RoomPublisherSync, StreamKind};
pub use self::ticket::RoomTicket;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + Sync + 'static>>;

mod publisher;

#[derive(Debug)]
pub struct Room {
    handle: RoomHandle,
    events: mpsc::Receiver<RoomEvent>,
}

pub type RoomEvents = mpsc::Receiver<RoomEvent>;

#[derive(Debug, Clone)]
pub struct RoomHandle {
    me: EndpointId,
    ticket: RoomTicket,
    tx: mpsc::Sender<ApiMessage>,
    _actor_handle: Arc<AbortOnDropHandle<()>>,
}

impl RoomHandle {
    pub fn ticket(&self) -> RoomTicket {
        let mut ticket = self.ticket.clone();
        ticket.bootstrap = vec![self.me];
        ticket
    }

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
    pub async fn new(
        endpoint: &Endpoint,
        gossip: Gossip,
        live: Live,
        ticket: RoomTicket,
    ) -> Result<Self> {
        let endpoint_id = endpoint.id();
        let (actor_tx, actor_rx) = mpsc::channel(16);
        let (event_tx, event_rx) = mpsc::channel(16);

        let actor = Actor::new(
            endpoint.secret_key(),
            live,
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

    pub async fn recv(&mut self) -> Result<RoomEvent> {
        self.events.recv().await.std_context("sender stopped")
    }

    pub fn try_recv(&mut self) -> Result<RoomEvent, TryRecvError> {
        self.events.try_recv()
    }

    pub fn ticket(&self) -> RoomTicket {
        self.handle.ticket()
    }

    pub fn split(self) -> (RoomEvents, RoomHandle) {
        (self.events, self.handle)
    }

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

#[derive(Debug)]
pub enum RoomEvent {
    RemoteAnnounced {
        remote: EndpointId,
        broadcasts: Vec<String>,
    },
    RemoteConnected {
        session: MoqSession,
    },
    BroadcastSubscribed {
        session: MoqSession,
        broadcast: SubscribeBroadcast,
    },
}

const PEER_STATE_KEY: &[u8] = b"s";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerState {
    broadcasts: Vec<String>,
}

type ConnectingFutures =
    FuturesUnordered<BoxFuture<(BroadcastId, Result<(MoqSession, SubscribeBroadcast)>)>>;
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
                let closed = producer.consume().closed();
                self.live.publish(name.clone(), producer).await.ok();
                self.active_publish.insert(name.clone());
                self.publish_closed.push(Box::pin(async move {
                    closed.await;
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
                let session = live.connect_and_subscribe(remote, &name).await;
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
            .put(PEER_STATE_KEY, postcard::to_stdvec(&state).unwrap())
            .await
        {
            warn!("failed to update gossip kv: {err:#}");
        }
    }
}

mod ticket {
    use std::env;
    use std::str::FromStr;

    use iroh::EndpointId;
    use iroh_gossip::TopicId;
    use n0_error::{Result, StdResultExt};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, Clone, derive_more::Display)]
    #[display("{}", iroh_tickets::Ticket::serialize(self))]
    pub struct RoomTicket {
        pub bootstrap: Vec<EndpointId>,
        pub topic_id: TopicId,
    }

    impl RoomTicket {
        pub fn new(topic_id: TopicId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
            Self {
                bootstrap: bootstrap.into_iter().collect(),
                topic_id,
            }
        }

        pub fn generate() -> Self {
            Self {
                bootstrap: vec![],
                topic_id: TopicId::from_bytes(rand::random()),
            }
        }

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
            postcard::to_stdvec(self).unwrap()
        }

        fn from_bytes(bytes: &[u8]) -> Result<Self, iroh_tickets::ParseError> {
            let ticket = postcard::from_bytes(bytes)?;
            Ok(ticket)
        }
    }
}
