//! The room itself: its handle, its event stream, and the actor behind both.

use std::{
    collections::{HashMap, HashSet, hash_map},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use iroh::{Endpoint, EndpointId, SecretKey};
use iroh_gossip::{Gossip, TopicId};
use iroh_moq::{Moq, MoqSession};
use iroh_smol_kv::{ExpiryConfig, Filter, SignedValue, Subscribe, SubscribeMode, WriteScope};
use moq_net::broadcast;
use n0_error::{AnyError, Result, e, stack_error};
use n0_future::{
    FuturesUnordered, StreamExt,
    task::{AbortOnDropHandle, JoinSet, spawn},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{
    mpsc::{self, error::TryRecvError},
    oneshot,
};
use tracing::{Instrument, debug, error_span, info, trace, warn};

use crate::{
    chat::{ChatMessage, ChatPublisher, ChatSubscriber},
    ticket::RoomTicket,
};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Errors returned by [`Room`] operations.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum Error {
    /// The room's gossip topic could not be joined.
    #[error(transparent)]
    Gossip(iroh_gossip::api::ApiError),
    /// The broadcast could not be created on the local MoQ origin.
    #[error(transparent)]
    Moq(iroh_moq::Error),
    /// The room actor stopped, which happens once the [`Room`] is dropped.
    #[error("the room actor stopped")]
    ActorStopped,
}

impl<T> From<mpsc::error::SendError<T>> for Error {
    fn from(_value: mpsc::error::SendError<T>) -> Self {
        e!(Self::ActorStopped)
    }
}

/// Multi-party room backed by gossip-based peer discovery.
///
/// Peers announce the names of their broadcasts on a shared gossip topic. When a
/// remote peer announces, the room connects over MoQ and subscribes, emitting a
/// [`RoomEvent::BroadcastSubscribed`] with the resulting
/// [`broadcast::Consumer`]. What travels on those broadcasts is entirely the
/// caller's business: the room only ever handles names, plus the chat track it
/// knows by name.
///
/// Use [`split`](Room::split) to separate the event stream from the publish
/// handle when the two need to live in different tasks.
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

    /// Creates a broadcast under `name` and announces it to everyone in the room.
    ///
    /// The returned producer is the one every peer subscribes to. Write tracks
    /// into it and end it with [`finish`](broadcast::Producer::finish); dropping
    /// it un-announces the name, and peers see the broadcast close.
    ///
    /// # Errors
    ///
    /// Fails if this peer already publishes a broadcast under `name`, or if the
    /// room actor has stopped.
    pub async fn publish(&self, name: impl Into<String>) -> Result<broadcast::Producer, Error> {
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(ApiMessage::Publish {
                name: name.into(),
                reply,
            })
            .await?;
        reply_rx.await.map_err(|_| e!(Error::ActorStopped))?
    }

    /// Registers the chat publisher the room sends messages through.
    ///
    /// Create it with [`ChatPublisher::create`] on a producer returned by
    /// [`publish`](Self::publish), so peers find the chat track on a broadcast
    /// they already subscribe to.
    pub async fn set_chat_publisher(&self, publisher: ChatPublisher) -> Result<(), Error> {
        self.tx
            .send(ApiMessage::SetChatPublisher { publisher })
            .await?;
        Ok(())
    }

    /// Sends a chat message to all peers in the room.
    ///
    /// Requires a chat publisher registered via
    /// [`set_chat_publisher`](Self::set_chat_publisher).
    pub async fn send_chat(&self, text: impl Into<String>) -> Result<(), Error> {
        self.tx
            .send(ApiMessage::SendChat { text: text.into() })
            .await?;
        Ok(())
    }

    /// Sets the display name for this peer, visible in [`RoomEvent::PeerJoined`].
    ///
    /// Triggers a gossip update so remote peers see the new name.
    pub async fn set_display_name(&self, name: impl Into<String>) -> Result<(), Error> {
        self.tx
            .send(ApiMessage::SetDisplayName { name: name.into() })
            .await?;
        Ok(())
    }
}

impl Room {
    /// Joins the room named by `ticket`.
    ///
    /// Subscribes to the room's gossip topic and spawns an actor that handles
    /// peer discovery, connection, and subscription. The actor stops when the
    /// [`Room`] and every [`RoomHandle`] cloned from it are dropped.
    ///
    /// # Errors
    ///
    /// Fails if the gossip topic cannot be subscribed to.
    pub async fn new(
        endpoint: &Endpoint,
        moq: &Moq,
        gossip: &Gossip,
        ticket: RoomTicket,
    ) -> Result<Self, Error> {
        let endpoint_id = endpoint.id();
        let (actor_tx, actor_rx) = mpsc::channel(16);
        let (event_tx, event_rx) = mpsc::channel(16);

        let actor = Actor::new(
            endpoint.secret_key(),
            moq.clone(),
            event_tx,
            gossip.clone(),
            ticket.clone(),
        )
        .await?;
        let actor_task = spawn(
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
    ///
    /// # Errors
    ///
    /// Fails once the room actor has stopped and no events remain.
    pub async fn recv(&mut self) -> Result<RoomEvent, Error> {
        self.events
            .recv()
            .await
            .ok_or_else(|| e!(Error::ActorStopped))
    }

    /// Returns the next room event without waiting, or an error if none is ready.
    pub fn try_recv(&mut self) -> Result<RoomEvent, TryRecvError> {
        self.events.try_recv()
    }

    /// Returns a ticket for this room that includes this peer as a bootstrap node.
    pub fn ticket(&self) -> RoomTicket {
        self.handle.ticket()
    }

    /// Returns a handle for publishing into this room.
    pub fn handle(&self) -> &RoomHandle {
        &self.handle
    }

    /// Splits the room into its event stream and publish handle.
    ///
    /// Useful when the event loop and the publisher live in different tasks.
    pub fn split(self) -> (RoomEvents, RoomHandle) {
        (self.events, self.handle)
    }

    /// Creates a broadcast under `name`. See [`RoomHandle::publish`].
    pub async fn publish(&self, name: impl Into<String>) -> Result<broadcast::Producer, Error> {
        self.handle.publish(name).await
    }

    /// Registers a chat publisher. See [`RoomHandle::set_chat_publisher`].
    pub async fn set_chat_publisher(&self, publisher: ChatPublisher) -> Result<(), Error> {
        self.handle.set_chat_publisher(publisher).await
    }

    /// Sends a chat message. See [`RoomHandle::send_chat`].
    pub async fn send_chat(&self, text: impl Into<String>) -> Result<(), Error> {
        self.handle.send_chat(text).await
    }

    /// Sets this peer's display name. See [`RoomHandle::set_display_name`].
    pub async fn set_display_name(&self, name: impl Into<String>) -> Result<(), Error> {
        self.handle.set_display_name(name).await
    }
}

enum ApiMessage {
    Publish {
        name: String,
        reply: oneshot::Sender<Result<broadcast::Producer, Error>>,
    },
    SendChat {
        text: String,
    },
    SetChatPublisher {
        publisher: ChatPublisher,
    },
    SetDisplayName {
        name: String,
    },
}

/// Events emitted by a [`Room`] as peers join and publish broadcasts.
#[derive(derive_more::Debug)]
#[non_exhaustive]
pub enum RoomEvent {
    /// A remote peer announced its available broadcasts via gossip.
    RemoteAnnounced {
        /// The announcing peer's endpoint ID.
        remote: EndpointId,
        /// Broadcast names the peer is publishing.
        broadcasts: Vec<String>,
    },
    /// Successfully subscribed to a remote peer's broadcast.
    BroadcastSubscribed {
        /// The peer publishing the broadcast.
        remote: EndpointId,
        /// The name the peer announced the broadcast under.
        name: String,
        /// The MoQ session with the remote peer.
        session: Box<MoqSession>,
        /// The subscribed broadcast, ready for its tracks to be read.
        #[debug(skip)]
        broadcast: broadcast::Consumer,
    },
    /// A peer appeared in the room for the first time.
    PeerJoined {
        /// The peer's endpoint ID.
        remote: EndpointId,
        /// Display name from the peer's gossip state, if set.
        display_name: Option<String>,
    },
    /// Every broadcast of a peer closed, so the peer is gone.
    PeerLeft {
        /// The peer's endpoint ID.
        remote: EndpointId,
    },
    /// A chat message arrived on a remote peer's broadcast.
    ChatReceived {
        /// The peer that sent the message.
        remote: EndpointId,
        /// The chat message.
        message: ChatMessage,
    },
}

const PEER_STATE_KEY: &[u8] = b"s";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerState {
    broadcasts: Vec<String>,
    /// Optional display name for the peer.
    ///
    /// Do NOT use `skip_serializing_if` here: postcard is a positional binary
    /// format, so skipping a field during serialization causes the deserializer
    /// to read past the buffer.
    display_name: Option<String>,
}

type SubscribeResult = Result<(MoqSession, broadcast::Consumer), AnyError>;
type ConnectingFutures = FuturesUnordered<BoxFuture<(BroadcastId, SubscribeResult)>>;
type KvEntry = (EndpointId, Bytes, SignedValue);

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, derive_more::Display)]
#[display("{}:{}", _0.fmt_short(), _1)]
struct BroadcastId(EndpointId, String);

struct Actor {
    me: EndpointId,
    /// Scopes every broadcast this room publishes, so two rooms can each carry
    /// a broadcast called "cam" on one node.
    topic_id: TopicId,
    _gossip: Gossip,
    moq: Moq,
    active_subscribe: HashSet<BroadcastId>,
    active_publish: HashSet<String>,
    known_peers: HashMap<EndpointId, Option<String>>,
    connecting: ConnectingFutures,
    subscribe_closed: FuturesUnordered<BoxFuture<BroadcastId>>,
    publish_closed: FuturesUnordered<BoxFuture<String>>,
    chat_tasks: JoinSet<()>,
    chat_publisher: Option<ChatPublisher>,
    display_name: Option<String>,
    event_tx: mpsc::Sender<RoomEvent>,
    kv: iroh_smol_kv::Client,
    kv_writer: WriteScope,
}

impl Actor {
    async fn new(
        me: &SecretKey,
        moq: Moq,
        event_tx: mpsc::Sender<RoomEvent>,
        gossip: Gossip,
        ticket: RoomTicket,
    ) -> Result<Self, Error> {
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
            topic_id: ticket.topic_id,
            moq,
            _gossip: gossip,
            active_subscribe: Default::default(),
            active_publish: Default::default(),
            known_peers: Default::default(),
            connecting: Default::default(),
            subscribe_closed: Default::default(),
            publish_closed: Default::default(),
            chat_tasks: Default::default(),
            chat_publisher: None,
            display_name: None,
            event_tx,
            kv,
            kv_writer,
        })
    }

    async fn run(mut self, mut inbox: mpsc::Receiver<ApiMessage>) {
        let updates = self
            .kv
            .subscribe_with_opts(Subscribe {
                mode: SubscribeMode::Both,
                filter: Filter::ALL,
            })
            .stream();
        tokio::pin!(updates);

        debug!("room actor started, waiting for gossip updates");

        loop {
            tokio::select! {
                update = updates.next() => {
                    match update {
                        None => {
                            warn!("gossip kv subscription stream ended unexpectedly");
                            break;
                        }
                        Some(Err(err)) => warn!("gossip kv update failed: {err:#}"),
                        Some(Ok(update)) => if !self.handle_gossip_update(update).await { break },
                    }
                }
                msg = inbox.recv() => {
                    match msg {
                        None => break,
                        Some(msg) => self.handle_api_message(msg).await
                    }
                }
                Some((id, res)) = self.connecting.next(), if !self.connecting.is_empty() => {
                    if !self.handle_subscribed(id, res).await { break }
                }
                Some(id) = self.subscribe_closed.next(), if !self.subscribe_closed.is_empty() => {
                    if !self.handle_broadcast_closed(id).await { break }
                }
                Some(name) = self.publish_closed.next(), if !self.publish_closed.is_empty() => {
                    debug!(%name, "local broadcast closed, un-announcing");
                    self.active_publish.remove(&name);
                    self.update_kv().await;
                }
                Some(res) = self.chat_tasks.join_next(), if !self.chat_tasks.is_empty() => {
                    if let Err(err) = res {
                        warn!("chat task panicked: {err}");
                    }
                }
            }
        }
    }

    /// Handles the outcome of a MoQ subscribe. Returns `false` if the actor should stop.
    async fn handle_subscribed(&mut self, id: BroadcastId, res: SubscribeResult) -> bool {
        let BroadcastId(remote, ref name) = id;
        let (session, consumer) = match res {
            Ok(parts) => parts,
            Err(err) => {
                self.active_subscribe.remove(&id);
                warn!(broadcast=%id, "subscribing to broadcast failed: {err:#}");
                return true;
            }
        };
        info!(broadcast=%id, "broadcast subscription ready, emitting event");

        self.spawn_chat_task(remote, consumer.clone());

        let event = RoomEvent::BroadcastSubscribed {
            remote,
            name: name.clone(),
            session: Box::new(session),
            broadcast: consumer.clone(),
        };
        if self.event_tx.send(event).await.is_err() {
            debug!("room event receiver dropped, stopping actor");
            return false;
        }

        // The consumer closes when the peer ends the broadcast or the session
        // fails, which is the signal `PeerLeft` is derived from.
        self.subscribe_closed.push(Box::pin(async move {
            consumer.closed().await;
            id
        }));
        true
    }

    /// Handles a remote broadcast closing. Returns `false` if the actor should stop.
    async fn handle_broadcast_closed(&mut self, id: BroadcastId) -> bool {
        debug!(broadcast=%id, "remote broadcast closed");
        let remote = id.0;
        self.active_subscribe.remove(&id);

        let still_active = self.active_subscribe.iter().any(|b| b.0 == remote);
        if still_active || self.known_peers.remove(&remote).is_none() {
            return true;
        }
        info!(remote=%remote.fmt_short(), "peer left the room");
        if self
            .event_tx
            .send(RoomEvent::PeerLeft { remote })
            .await
            .is_err()
        {
            debug!("room event receiver dropped, stopping actor");
            return false;
        }
        true
    }

    /// Spawns a task that forwards chat messages from a remote broadcast.
    ///
    /// A broadcast without a chat track is the common case, so a failed
    /// subscription ends the task quietly rather than being reported.
    fn spawn_chat_task(&mut self, remote: EndpointId, consumer: broadcast::Consumer) {
        let event_tx = self.event_tx.clone();
        self.chat_tasks.spawn(async move {
            let mut subscriber = match ChatSubscriber::subscribe(&consumer).await {
                Ok(subscriber) => subscriber,
                Err(err) => {
                    debug!(remote=%remote.fmt_short(), "no chat track on broadcast: {err}");
                    return;
                }
            };
            while let Some(message) = subscriber.recv().await {
                if event_tx
                    .send(RoomEvent::ChatReceived { remote, message })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    async fn handle_api_message(&mut self, msg: ApiMessage) {
        match msg {
            ApiMessage::Publish { name, reply } => {
                reply.send(self.publish(name).await).ok();
            }
            ApiMessage::SendChat { text } => match self.chat_publisher.as_mut() {
                Some(publisher) => {
                    if let Err(err) = publisher.send(&text) {
                        warn!("failed to send chat message: {err}");
                    }
                }
                None => warn!("chat is not enabled: register a ChatPublisher first"),
            },
            ApiMessage::SetChatPublisher { publisher } => {
                self.chat_publisher = Some(publisher);
                info!("room chat publisher set");
            }
            ApiMessage::SetDisplayName { name } => {
                info!(name, "display name set");
                self.display_name = Some(name);
                self.update_kv().await;
            }
        }
    }

    async fn publish(&mut self, name: String) -> Result<broadcast::Producer, Error> {
        let path = room_path(self.topic_id, &name);
        info!(%name, %path, "publishing broadcast to room");
        let producer = self.moq.publish(&path)?;
        let consumer = producer.consume();
        self.active_publish.insert(name.clone());
        self.publish_closed.push(Box::pin(async move {
            consumer.closed().await;
            name
        }));
        info!(broadcasts=?self.active_publish, "announcing published broadcasts over gossip");
        self.update_kv().await;
        Ok(producer)
    }

    /// Handles a gossip KV update. Returns `false` if the actor should stop.
    async fn handle_gossip_update(&mut self, entry: KvEntry) -> bool {
        let (remote, key, value) = entry;
        if remote == self.me {
            trace!(remote=%remote.fmt_short(), "ignoring own kv update");
            return true;
        }
        if key != PEER_STATE_KEY {
            trace!(remote=%remote.fmt_short(), key=?key, "ignoring kv update for unknown key");
            return true;
        }
        let Ok(value) = postcard::from_bytes::<PeerState>(&value.value) else {
            warn!(
                remote=%remote.fmt_short(),
                value_len=value.value.len(),
                "failed to deserialize peer state from kv update"
            );
            return true;
        };
        let PeerState {
            broadcasts,
            display_name,
        } = value;

        info!(
            remote=%remote.fmt_short(),
            ?broadcasts,
            ?display_name,
            "received peer announcement via gossip"
        );

        if let hash_map::Entry::Vacant(entry) = self.known_peers.entry(remote) {
            entry.insert(display_name.clone());
            info!(remote=%remote.fmt_short(), "new peer joined room");
            if self
                .event_tx
                .send(RoomEvent::PeerJoined {
                    remote,
                    display_name,
                })
                .await
                .is_err()
            {
                debug!("room event receiver dropped, stopping actor");
                return false;
            }
        }

        for name in broadcasts.clone() {
            let id = BroadcastId(remote, name.clone());
            if !self.active_subscribe.insert(id.clone()) {
                debug!(broadcast=%id, "already subscribing to broadcast, skipping");
                continue;
            }
            info!(broadcast=%id, "initiating MoQ subscription to remote broadcast");
            let moq = self.moq.clone();
            let topic = self.topic_id;
            self.connecting.push(Box::pin(async move {
                let res = subscribe(moq, remote, topic, &name).await;
                match &res {
                    Ok(_) => info!(broadcast=%id, "MoQ subscription established"),
                    Err(err) => warn!(broadcast=%id, "MoQ subscription failed: {err:#}"),
                }
                (id, res)
            }));
        }
        if self
            .event_tx
            .send(RoomEvent::RemoteAnnounced { remote, broadcasts })
            .await
            .is_err()
        {
            debug!("room event receiver dropped, stopping actor");
            return false;
        }
        true
    }

    // A plain (non-`async`) method that returns an owned, `'static` future,
    // rather than an `async fn(&self)`. The latter ties its returned future's
    // hidden type to the lifetime of `&self`, which the `run` actor loop then
    // has to hold across an await point. `Actor` cannot be `Sync` (it holds
    // boxed futures that reach a non-`Sync` MoQ session type), so any future
    // that keeps `self` borrowed would make the actor's spawned task future
    // non-`Send`. Extracting owned data up front sidesteps that entirely.
    fn update_kv(&self) -> impl Future<Output = ()> + Send + 'static {
        let kv_writer = self.kv_writer.clone();
        let state = PeerState {
            broadcasts: self.active_publish.iter().cloned().collect(),
            display_name: self.display_name.clone(),
        };
        put_peer_state(kv_writer, state)
    }
}

/// Writes `state` under [`PEER_STATE_KEY`] through `kv_writer`.
///
/// A free function so the awaited future owns `kv_writer` outright, rather
/// than borrowing it from the actor across the await point.
async fn put_peer_state(kv_writer: WriteScope, state: PeerState) {
    if let Err(err) = kv_writer
        .put(
            PEER_STATE_KEY,
            postcard::to_stdvec(&state).expect("PeerState serialization is infallible"),
        )
        .await
    {
        warn!("failed to update gossip kv: {err:#}");
    }
}

/// Connects to `remote` and subscribes to the broadcast it announced at `name`.
async fn subscribe(moq: Moq, remote: EndpointId, topic: TopicId, name: &str) -> SubscribeResult {
    let session = moq.connect(remote).await?;
    let broadcast = session.subscribe(room_path(topic, name)).await?;
    Ok((session, broadcast))
}

/// The origin path a room's broadcast lives at.
///
/// Publishing is node-wide, so a bare name would collide across rooms: a peer
/// in two rooms that publishes "cam" in each would find the second rejected as
/// a duplicate. The topic scopes it, and both sides derive the same path from
/// the ticket they already share.
fn room_path(topic: TopicId, name: &str) -> String {
    format!("rooms/{topic}/{name}")
}
