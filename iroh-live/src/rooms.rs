use std::{collections::HashMap, pin::Pin, sync::Arc, time::Duration};

use bytes::Bytes;
use iroh::{EndpointId, SecretKey};
use iroh_gossip::Gossip;
use iroh_moq::MoqSession;
use iroh_smol_kv::{ExpiryConfig, Filter, SignedValue, Subscribe, SubscribeMode, WriteScope};
use moq_lite::BroadcastProducer;
use moq_media::{chat::ChatMessage, subscribe::RemoteBroadcast};
use n0_error::{Result, StackResultExt, StdResultExt, anyerr};
use n0_future::{FuturesUnordered, StreamExt, task::AbortOnDropHandle};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc::{self, error::TryRecvError},
    task,
};
use tracing::{Instrument, debug, error_span, info, trace, warn};

pub use self::{publisher::RoomPublisherSync, ticket::RoomTicket};
use crate::{
    Live, Subscription,
    sources::{RelayOffer, SourceSet, TransportSource},
};

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

    /// Publishes a [`LocalBroadcast`] into the room, announcing it to all peers.
    ///
    /// Extracts the underlying producer via [`LocalBroadcast::producer()`].
    /// For raw producer access, use [`publish_producer`](Self::publish_producer).
    pub async fn publish(
        &self,
        name: impl ToString,
        broadcast: &moq_media::publish::LocalBroadcast,
    ) -> Result<()> {
        self.publish_producer(name, broadcast.producer()).await
    }

    /// Publishes a raw [`BroadcastProducer`] into the room.
    pub async fn publish_producer(
        &self,
        name: impl ToString,
        producer: BroadcastProducer,
    ) -> Result<()> {
        self.tx
            .send(ApiMessage::Publish {
                name: name.to_string(),
                producer,
            })
            .await
            .map_err(|_| anyerr!("room actor died"))
    }

    /// Registers a [`ChatPublisher`](moq_media::chat::ChatPublisher) with the room actor.
    ///
    /// Call this after [`LocalBroadcast::enable_chat`](moq_media::publish::LocalBroadcast::enable_chat)
    /// to allow [`send_chat`](Self::send_chat) to work.
    pub async fn set_chat_publisher(
        &self,
        publisher: moq_media::chat::ChatPublisher,
    ) -> Result<()> {
        self.tx
            .send(ApiMessage::SetChatPublisher { publisher })
            .await
            .map_err(|_| anyerr!("room actor died"))
    }

    /// Sends a chat message to all peers in the room.
    ///
    /// Requires that a chat publisher has been registered via
    /// [`set_chat_publisher`](Self::set_chat_publisher).
    pub async fn send_chat(&self, text: impl Into<String>) -> Result<()> {
        self.tx
            .send(ApiMessage::SendChat { text: text.into() })
            .await
            .map_err(|_| anyerr!("room actor died"))
    }

    /// Sets the display name for this peer, visible in [`RoomEvent::PeerJoined`].
    ///
    /// Triggers a gossip KV update so remote peers see the new name.
    pub async fn set_display_name(&self, name: impl Into<String>) -> Result<()> {
        self.tx
            .send(ApiMessage::SetDisplayName { name: name.into() })
            .await
            .map_err(|_| anyerr!("room actor died"))
    }

    /// Enables relay-backed publish and announce for this peer.
    ///
    /// The peer's next gossip announcement will carry `offer` as its
    /// relay hint, and every currently-published broadcast gets
    /// announced on a fresh H3 session to the relay. Other peers in
    /// the room pick up the hint and can subscribe either directly
    /// or via the relay.
    pub async fn enable_relay(&self, offer: RelayOffer) -> Result<()> {
        self.tx
            .send(ApiMessage::EnableRelay { offer })
            .await
            .map_err(|_| anyerr!("room actor died"))
    }

    /// Disables relay-backed publish and announce for this peer.
    ///
    /// Removes the relay hint from the next gossip announcement and
    /// drops every relay-side broadcast announce. Peers currently
    /// subscribed via the relay will fall over to direct if their
    /// source set still includes this peer's direct endpoint.
    pub async fn disable_relay(&self) -> Result<()> {
        self.tx
            .send(ApiMessage::DisableRelay)
            .await
            .map_err(|_| anyerr!("room actor died"))
    }

    /// Sets the default selection policy for how this peer's
    /// subscribers pick between candidate transports for a given
    /// remote peer's broadcasts. Currently informational: the
    /// preferred-ordered policy is hardcoded internally.
    #[doc(hidden)]
    pub async fn set_subscribe_mode(&self, mode: RoomSubscribeMode) -> Result<()> {
        self.tx
            .send(ApiMessage::SetSubscribeMode { mode })
            .await
            .map_err(|_| anyerr!("room actor died"))
    }
}

/// Controls how this peer picks between direct and relay sources when
/// subscribing to another peer's broadcasts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RoomSubscribeMode {
    /// Prefer direct sources, fall back to relay when direct is
    /// unavailable. Default.
    #[default]
    PreferDirect,
    /// Prefer the relay source when the peer advertises one, fall
    /// back to direct otherwise.
    PreferRelay,
    /// Use only direct sources; ignore relay hints entirely.
    DirectOnly,
    /// Use only relay sources; ignore direct peers. Useful when
    /// local connectivity to peers is poor.
    RelayOnly,
}

/// Builder for a [`Room`].
///
/// Use [`Live::join_room`] for the default (direct-P2P only) path.
/// Pass a pre-configured `RoomBuilder` to opt into relay-backed
/// publishing and subscribing at join time.
#[derive(Debug)]
pub struct RoomBuilder {
    ticket: RoomTicket,
    relay: Option<RelayOffer>,
    subscribe_mode: RoomSubscribeMode,
}

impl RoomBuilder {
    /// Creates a builder for the given ticket with no relay attached.
    pub fn new(ticket: RoomTicket) -> Self {
        Self {
            ticket,
            relay: None,
            subscribe_mode: RoomSubscribeMode::PreferDirect,
        }
    }

    /// Attaches a relay offer to the room. Every broadcast this peer
    /// publishes will also be announced on the relay, and the offer
    /// is advertised via gossip so other peers can subscribe via the
    /// relay as a fallback.
    pub fn with_relay(mut self, offer: RelayOffer) -> Self {
        self.relay = Some(offer);
        self
    }

    /// Sets the preferred subscribe mode. See [`RoomSubscribeMode`].
    pub fn subscribe_mode(mut self, mode: RoomSubscribeMode) -> Self {
        self.subscribe_mode = mode;
        self
    }

    /// Spawns the room actor and returns the [`Room`].
    pub async fn spawn(self, live: &Live) -> Result<Room> {
        Room::from_builder(live, self).await
    }
}

impl Room {
    /// Joins a room using the given ticket with default settings.
    ///
    /// Requires gossip to be enabled on [`Live`]. Spawns an internal actor
    /// that handles peer discovery, connection, and subscription.
    pub async fn new(live: &Live, ticket: RoomTicket) -> Result<Self> {
        RoomBuilder::new(ticket).spawn(live).await
    }

    async fn from_builder(live: &Live, builder: RoomBuilder) -> Result<Self> {
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
            builder.ticket.clone(),
            builder.relay,
            builder.subscribe_mode,
        )
        .await?;
        let actor_task = task::spawn(async move { actor.run(actor_rx).await }.instrument(
            error_span!("RoomActor", id = builder.ticket.topic_id.fmt_short()),
        ));

        Ok(Self {
            handle: RoomHandle {
                ticket: builder.ticket,
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

    /// Publishes a [`LocalBroadcast`](moq_media::publish::LocalBroadcast) into the room,
    /// announcing it to all peers.
    pub async fn publish(
        &self,
        name: impl ToString,
        broadcast: &moq_media::publish::LocalBroadcast,
    ) -> Result<()> {
        self.handle.publish(name, broadcast).await
    }

    /// Publishes a raw [`BroadcastProducer`] into the room.
    pub async fn publish_producer(
        &self,
        name: impl ToString,
        producer: BroadcastProducer,
    ) -> Result<()> {
        self.handle.publish_producer(name, producer).await
    }

    /// Registers a chat publisher with the room. See [`RoomHandle::set_chat_publisher`].
    pub async fn set_chat_publisher(
        &self,
        publisher: moq_media::chat::ChatPublisher,
    ) -> Result<()> {
        self.handle.set_chat_publisher(publisher).await
    }

    /// Sends a chat message. See [`RoomHandle::send_chat`].
    pub async fn send_chat(&self, text: impl Into<String>) -> Result<()> {
        self.handle.send_chat(text).await
    }

    /// Enables relay-backed publish and announce for this peer.
    pub async fn enable_relay(&self, offer: RelayOffer) -> Result<()> {
        self.handle.enable_relay(offer).await
    }

    /// Disables relay-backed publish and announce for this peer.
    pub async fn disable_relay(&self) -> Result<()> {
        self.handle.disable_relay().await
    }

    /// Returns the handle for this room, useful when cloning is enough.
    pub fn handle(&self) -> &RoomHandle {
        &self.handle
    }
}

enum ApiMessage {
    Publish {
        name: String,
        producer: BroadcastProducer,
    },
    SendChat {
        text: String,
    },
    SetChatPublisher {
        publisher: moq_media::chat::ChatPublisher,
    },
    SetDisplayName {
        name: String,
    },
    EnableRelay {
        offer: RelayOffer,
    },
    DisableRelay,
    SetSubscribeMode {
        mode: RoomSubscribeMode,
    },
}

/// Events emitted by a [`Room`] as peers join and publish broadcasts.
#[derive(Debug)]
#[non_exhaustive]
pub enum RoomEvent {
    /// A remote peer announced its available broadcasts via gossip.
    RemoteAnnounced {
        /// The announcing peer's endpoint ID.
        remote: EndpointId,
        /// Broadcast names the peer is publishing.
        broadcasts: Vec<String>,
        /// The relay this peer offers, if any. Other peers can use
        /// this as a fallback source for the peer's broadcasts.
        relay: Option<RelayOffer>,
    },
    /// Successfully subscribed to a remote peer's broadcast.
    BroadcastSubscribed {
        /// The MoQ session with the remote peer (the currently active source).
        session: Box<MoqSession>,
        /// The subscribed broadcast, ready for video/audio decoding.
        broadcast: Box<RemoteBroadcast>,
    },
    /// The active source for a peer's broadcast changed (for
    /// instance, direct session died and the subscriber fell over to
    /// the relay).
    BroadcastSwitched {
        /// The peer whose broadcast source changed.
        remote: EndpointId,
        /// Name of the broadcast.
        broadcast_name: String,
        /// `true` when the new source is a relay. `false` when
        /// direct.
        via_relay: bool,
    },
    /// A peer appeared in the room for the first time.
    PeerJoined {
        /// The peer's endpoint ID.
        remote: EndpointId,
        /// Display name from the peer's gossip state, if set.
        display_name: Option<String>,
    },
    /// A peer's gossip state expired or was removed.
    PeerLeft {
        /// The peer's endpoint ID.
        remote: EndpointId,
    },
    /// A chat message was received from a remote peer's broadcast.
    ChatReceived {
        /// The peer that sent the message.
        remote: EndpointId,
        /// The chat message.
        message: ChatMessage,
    },
}

const PEER_STATE_KEY: &[u8] = b"s";

/// Gossip KV message advertising a peer's presence and publish state.
///
/// Carries the list of broadcasts the peer is publishing and, when
/// the peer opted into relay-backed publishing, the [`RelayOffer`]
/// other peers should use as a fallback source. Older versions of
/// this struct wire-compatibly omit the `relay` field by carrying
/// `None`; note that postcard is positional, so new fields must be
/// appended and older clients will simply stop reading at the
/// struct's previous tail.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerState {
    broadcasts: Vec<String>,
    /// Optional display name for the peer.
    ///
    /// Do NOT use `skip_serializing_if` here: postcard is a positional binary
    /// format, so skipping a field during serialization causes the deserializer
    /// to read past the buffer.
    display_name: Option<String>,
    /// Relay this peer advertises as an alternate source for its
    /// broadcasts. `None` means peers must subscribe directly.
    relay: Option<RelayOffer>,
}

type ConnectingFutures = FuturesUnordered<BoxFuture<(BroadcastId, SubscribeResult)>>;
type KvEntry = (EndpointId, Bytes, SignedValue);

struct SubscribeResult {
    subscription: Option<Subscription>,
    err: Option<n0_error::AnyError>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, derive_more::Display)]
#[display("{}:{}", _0.fmt_short(), _1)]
struct BroadcastId(EndpointId, String);

type ChatFuture = BoxFuture<(
    EndpointId,
    Option<ChatMessage>,
    moq_media::chat::ChatSubscriber,
)>;

struct Actor {
    me: EndpointId,
    _gossip: Gossip,
    live: Live,
    active_subscribe: HashMap<BroadcastId, ActiveSubscribe>,
    active_publish: HashMap<String, ActivePublish>,
    known_peers: HashMap<EndpointId, Option<String>>,
    peer_relays: HashMap<EndpointId, Option<RelayOffer>>,
    connecting: ConnectingFutures,
    subscribe_closed: FuturesUnordered<BoxFuture<BroadcastId>>,
    publish_closed: FuturesUnordered<BoxFuture<String>>,
    chat_messages: FuturesUnordered<ChatFuture>,
    chat_publisher: Option<moq_media::chat::ChatPublisher>,
    display_name: Option<String>,
    relay: Option<RelayOffer>,
    subscribe_mode: RoomSubscribeMode,
    event_tx: mpsc::Sender<RoomEvent>,
    kv: iroh_smol_kv::Client,
    kv_writer: WriteScope,
}

/// Tracks an active subscription to one remote peer's broadcast.
///
/// The [`Subscription`] owns the candidate sessions and
/// swaps between them when the preferred source disappears. The
/// handle is stored so that relay-hint changes can mutate the source
/// set in place without tearing down the subscription.
struct ActiveSubscribe {
    multi: Subscription,
}

/// Tracks an active publish on the room.
///
/// Direct peers receive the broadcast through the global
/// [`Live::publish_broadcast_producer`](crate::Live::publish_broadcast_producer)
/// registration on the underlying `Live`. When the room is also
/// attached to a relay (via [`RoomBuilder::with_relay`] or
/// [`RoomHandle::enable_relay`]) the broadcast is additionally
/// announced on the relay session held by `relay_publish`.
struct ActivePublish {
    /// Consumer for the producer registered globally with the
    /// `Moq` actor. Held so the broadcaster's lifetime ties to
    /// this entry; dropped when the publish ends.
    _global_consumer: moq_lite::BroadcastConsumer,
    /// Relay-side announce, present only when the room has a
    /// relay attached. Replaced when the relay configuration
    /// changes at runtime.
    relay: Option<RelayAnnounce>,
}

/// Announce of a single broadcast on a relay session. Dropping
/// this ends the announce on the relay only; the global publish
/// continues to serve direct peers.
struct RelayAnnounce {
    /// The relay session this broadcast is announced on.
    _session: MoqSession,
    /// Consumer attached to the session via `session.publish`.
    /// Dropping ends the announce.
    _consumer: moq_lite::BroadcastConsumer,
}

impl Actor {
    #[allow(
        clippy::too_many_arguments,
        reason = "actor constructor takes one arg per piece of injected state"
    )]
    async fn new(
        me: &SecretKey,
        live: Live,
        event_tx: mpsc::Sender<RoomEvent>,
        gossip: Gossip,
        ticket: RoomTicket,
        relay: Option<RelayOffer>,
        subscribe_mode: RoomSubscribeMode,
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
            known_peers: Default::default(),
            peer_relays: Default::default(),
            connecting: Default::default(),
            subscribe_closed: Default::default(),
            publish_closed: Default::default(),
            chat_messages: Default::default(),
            chat_publisher: None,
            display_name: None,
            relay,
            subscribe_mode,
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

        debug!("room actor started, waiting for gossip updates");

        // Seed the KV so peers that are already in the topic see us.
        self.update_kv().await;

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
                    match res.subscription {
                        Some(multi) => {
                            let remote = id.0;
                            info!(broadcast=%id, remote=%remote.fmt_short(), "multi-source subscription live");
                            let Some(active) = multi.active().await else {
                                warn!(broadcast=%id, "multi-source subscription closed before picking a source");
                                self.active_subscribe.remove(&id);
                                continue;
                            };
                            let broadcast = active.broadcast().clone();
                            let session = active.session().clone();

                            self.active_subscribe.insert(id.clone(), ActiveSubscribe { multi: multi.clone() });

                            // Spawn a watcher task that forwards active-source
                            // changes into a BroadcastSwitched event.
                            self.spawn_active_watcher(id.clone(), multi.clone());

                            // Spawn a chat subscriber task if the broadcast has a chat track.
                            if let Some(mut chat_sub) = broadcast.chat() {
                                self.chat_messages.push(Box::pin(async move {
                                    let msg = chat_sub.recv().await;
                                    (remote, msg, chat_sub)
                                }));
                            }

                            let closed_fut = broadcast.closed();
                            let id_for_closed = id.clone();
                            self.subscribe_closed.push(Box::pin(async move {
                                closed_fut.await;
                                id_for_closed
                            }));

                            if self.event_tx.send(RoomEvent::BroadcastSubscribed { session: Box::new(session), broadcast: Box::new(broadcast) }).await.is_err() {
                                debug!("room event receiver dropped, stopping actor");
                                return;
                            }
                        }
                        None => {
                            self.active_subscribe.remove(&id);
                            if let Some(err) = res.err {
                                warn!(broadcast=%id, "subscribing to broadcast failed: {err:#}");
                            }
                        }
                    }
                }
                Some(id) = self.subscribe_closed.next(), if !self.subscribe_closed.is_empty() => {
                    debug!("broadcast closed: {id}");
                    let remote = id.0;
                    self.active_subscribe.remove(&id);

                    let still_active = self.active_subscribe.keys().any(|b| b.0 == remote);
                    if !still_active
                        && self.known_peers.remove(&remote).is_some()
                        && self.event_tx.send(RoomEvent::PeerLeft { remote }).await.is_err()
                    {
                        debug!("room event receiver dropped, stopping actor");
                        return;
                    }
                }
                Some(name) = self.publish_closed.next(), if !self.publish_closed.is_empty() => {
                    self.active_publish.remove(&name);
                    self.update_kv().await;
                }
                Some((remote, maybe_msg, mut chat_sub)) = self.chat_messages.next(), if !self.chat_messages.is_empty() => {
                    if let Some(message) = maybe_msg {
                        if self.event_tx.send(RoomEvent::ChatReceived { remote, message }).await.is_err() {
                            debug!("room event receiver dropped, stopping actor");
                            return;
                        }
                        self.chat_messages.push(Box::pin(async move {
                            let msg = chat_sub.recv().await;
                            (remote, msg, chat_sub)
                        }));
                    }
                }
            }
        }
    }

    fn spawn_active_watcher(&self, id: BroadcastId, multi: Subscription) {
        let tx = self.event_tx.clone();
        let remote = id.0;
        let name = id.1.clone();
        tokio::spawn(async move {
            let mut last_active = multi.active_id();
            while let Some(event) = multi.next_event().await {
                if let crate::SubscriptionEvent::ActiveChanged { current, .. } = event
                    && current != last_active
                {
                    let via_relay = current.as_ref().is_some_and(|id| id.is_relay());
                    if tx
                        .send(RoomEvent::BroadcastSwitched {
                            remote,
                            broadcast_name: name.clone(),
                            via_relay,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                    last_active = current;
                }
            }
        });
    }

    async fn handle_api_message(&mut self, msg: ApiMessage) {
        match msg {
            ApiMessage::Publish { name, producer } => {
                info!(%name, "publishing broadcast to room");
                let consume_for_close = producer.consume();
                let global_consumer = producer.consume();
                if let Err(err) = self
                    .live
                    .publish_broadcast_producer(name.clone(), producer)
                    .await
                {
                    warn!(%name, "failed to register broadcast: {err:#}");
                    return;
                }
                let relay_announce = self.attach_relay_announce(&name, &global_consumer).await;
                self.active_publish.insert(
                    name.clone(),
                    ActivePublish {
                        _global_consumer: global_consumer,
                        relay: relay_announce,
                    },
                );
                self.publish_closed.push(Box::pin(async move {
                    consume_for_close.closed().await;
                    name
                }));
                info!(
                    broadcasts=?self.active_publish.keys().collect::<Vec<_>>(),
                    "updating gossip kv with published broadcasts",
                );
                self.update_kv().await;
            }
            ApiMessage::SendChat { text } => {
                if let Some(publisher) = self.chat_publisher.as_mut() {
                    if let Err(err) = publisher.send(&text) {
                        warn!("failed to send chat message: {err:#}");
                    }
                } else {
                    warn!("chat not enabled; call enable_chat() on the broadcast first");
                }
            }
            ApiMessage::SetChatPublisher { publisher } => {
                self.chat_publisher = Some(publisher);
                info!("room chat publisher set");
            }
            ApiMessage::SetDisplayName { name } => {
                info!(name, "display name set");
                self.display_name = Some(name);
                self.update_kv().await;
            }
            ApiMessage::EnableRelay { offer } => {
                info!(relay=%offer.endpoint.fmt_short(), path=%offer.path, "enabling relay for room");
                self.relay = Some(offer.clone());
                self.refresh_relay_announces().await;
                self.apply_peer_sources();
                self.update_kv().await;
            }
            ApiMessage::DisableRelay => {
                info!("disabling relay for room");
                self.relay = None;
                self.refresh_relay_announces().await;
                self.apply_peer_sources();
                self.update_kv().await;
            }
            ApiMessage::SetSubscribeMode { mode } => {
                info!(?mode, "setting subscribe mode");
                self.subscribe_mode = mode;
                self.apply_peer_sources();
            }
        }
    }

    /// Opens the relay session and announces `name` on it when a
    /// relay is configured for this room. Returns `None` when the
    /// room has no relay attached or when the connection or
    /// announce setup fails.
    async fn attach_relay_announce(
        &self,
        name: &str,
        consumer: &moq_lite::BroadcastConsumer,
    ) -> Option<RelayAnnounce> {
        let offer = self.relay.as_ref()?;
        let target = offer.to_target();
        let session = match self.live.connect_relay(&target).await {
            Ok(session) => session,
            Err(err) => {
                warn!(name, "failed to connect to relay for announce: {err:#}");
                return None;
            }
        };
        let consumer = consumer.clone();
        session.publish(name.to_string(), consumer.clone());
        Some(RelayAnnounce {
            _session: session,
            _consumer: consumer,
        })
    }

    /// Re-runs [`Self::attach_relay_announce`] for every active
    /// publish so that toggling the room's relay at runtime adds
    /// or drops the relay-side announce for each broadcast.
    async fn refresh_relay_announces(&mut self) {
        let names: Vec<String> = self.active_publish.keys().cloned().collect();
        for name in names {
            let consumer = match self.active_publish.get(&name) {
                Some(pub_state) => pub_state._global_consumer.clone(),
                None => continue,
            };
            let new_announce = self.attach_relay_announce(&name, &consumer).await;
            if let Some(pub_state) = self.active_publish.get_mut(&name) {
                pub_state.relay = new_announce;
            }
        }
    }

    /// Rebuilds the source set for every remote peer's subscription,
    /// reflecting the room's current subscribe mode and the peer's
    /// most recent relay hint.
    fn apply_peer_sources(&self) {
        for (id, sub) in &self.active_subscribe {
            let remote = id.0;
            let set = self.sources_for_peer(
                remote,
                self.peer_relays.get(&remote).and_then(|r| r.as_ref()),
            );
            sub.multi.sources().set(set);
        }
    }

    /// Builds the source set a subscriber should use for `remote`'s
    /// broadcasts given its advertised relay (if any) and this
    /// peer's subscribe mode.
    fn sources_for_peer(&self, remote: EndpointId, peer_relay: Option<&RelayOffer>) -> SourceSet {
        let mut set = SourceSet::new();
        let include_direct = matches!(
            self.subscribe_mode,
            RoomSubscribeMode::PreferDirect
                | RoomSubscribeMode::PreferRelay
                | RoomSubscribeMode::DirectOnly
        );
        let include_relay = matches!(
            self.subscribe_mode,
            RoomSubscribeMode::PreferDirect
                | RoomSubscribeMode::PreferRelay
                | RoomSubscribeMode::RelayOnly
        );
        let direct = TransportSource::direct(iroh::EndpointAddr::new(remote));
        let relay = peer_relay.map(|offer| TransportSource::relay(offer.to_target()));

        match self.subscribe_mode {
            RoomSubscribeMode::PreferDirect | RoomSubscribeMode::DirectOnly => {
                if include_direct {
                    set.push(direct);
                }
                if include_relay && let Some(relay) = relay {
                    set.push(relay);
                }
            }
            RoomSubscribeMode::PreferRelay | RoomSubscribeMode::RelayOnly => {
                if include_relay && let Some(relay) = relay {
                    set.push(relay);
                }
                if include_direct {
                    set.push(TransportSource::direct(iroh::EndpointAddr::new(remote)));
                }
            }
        }
        set
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
            relay,
        } = value;

        info!(
            remote=%remote.fmt_short(),
            ?broadcasts,
            ?display_name,
            relay=%relay.as_ref().map(|r| r.endpoint.fmt_short().to_string()).unwrap_or_else(|| "none".into()),
            "received peer announcement via gossip"
        );

        let relay_changed = self
            .peer_relays
            .get(&remote)
            .map(|existing| existing != &relay)
            .unwrap_or(true);
        self.peer_relays.insert(remote, relay.clone());

        if let std::collections::hash_map::Entry::Vacant(e) = self.known_peers.entry(remote) {
            e.insert(display_name.clone());
            info!(remote=%remote.fmt_short(), "new peer joined room");
            if self
                .event_tx
                .send(RoomEvent::PeerJoined {
                    remote,
                    display_name: display_name.clone(),
                })
                .await
                .is_err()
            {
                debug!("room event receiver dropped, stopping actor");
                return false;
            }
        }

        // If the relay changed for an already-subscribed peer, update
        // the source sets of every subscription to that peer in place.
        if relay_changed {
            let keys_for_peer: Vec<BroadcastId> = self
                .active_subscribe
                .keys()
                .filter(|id| id.0 == remote)
                .cloned()
                .collect();
            for id in keys_for_peer {
                if let Some(sub) = self.active_subscribe.get(&id) {
                    let set = self.sources_for_peer(remote, relay.as_ref());
                    sub.multi.sources().set(set);
                }
            }
        }

        for name in broadcasts.clone() {
            let id = BroadcastId(remote, name.clone());
            if self.active_subscribe.contains_key(&id) {
                debug!(broadcast=%id, "already subscribing to broadcast, skipping");
                continue;
            }
            info!(broadcast=%id, "initiating multi-source subscription to remote broadcast");
            let set = self.sources_for_peer(remote, relay.as_ref());
            if set.is_empty() {
                warn!(broadcast=%id, "no sources for peer under current subscribe mode");
                continue;
            }
            let multi = self.live.subscribe(set, name.clone());
            // Reserve the slot so we don't issue a duplicate subscribe
            // on the next gossip tick. The ActiveSubscribe gets its
            // final shape when `connecting` resolves.
            self.active_subscribe.insert(
                id.clone(),
                ActiveSubscribe {
                    multi: multi.clone(),
                },
            );
            let id_for_task = id.clone();
            self.connecting.push(Box::pin(async move {
                let res = match multi.wait_active().await {
                    Some(_) => SubscribeResult {
                        subscription: Some(multi),
                        err: None,
                    },
                    None => SubscribeResult {
                        subscription: None,
                        err: Some(anyerr!(
                            "multi-source subscription closed with no active source"
                        )),
                    },
                };
                (id_for_task, res)
            }));
        }
        if self
            .event_tx
            .send(RoomEvent::RemoteAnnounced {
                remote,
                broadcasts,
                relay,
            })
            .await
            .is_err()
        {
            debug!("room event receiver dropped, stopping actor");
            return false;
        }
        true
    }

    async fn update_kv(&self) {
        let state = PeerState {
            broadcasts: self.active_publish.keys().cloned().collect(),
            display_name: self.display_name.clone(),
            relay: self.relay.clone(),
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
        /// Optional relay offer baked into the ticket. Peers that join
        /// via this ticket MAY use the relay as an additional source
        /// for any broadcast in the room that the relay carries.
        #[serde(default)]
        pub relay: Option<crate::sources::RelayOffer>,
    }

    impl RoomTicket {
        /// Creates a ticket with the given topic and bootstrap peers.
        pub fn new(topic_id: TopicId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
            Self {
                bootstrap: bootstrap.into_iter().collect(),
                topic_id,
                relay: None,
            }
        }

        /// Attaches a relay offer to the ticket.
        #[must_use]
        pub fn with_relay(mut self, offer: crate::sources::RelayOffer) -> Self {
            self.relay = Some(offer);
            self
        }

        /// Generates a new room with a random topic ID and no bootstrap peers.
        pub fn generate() -> Self {
            Self {
                bootstrap: vec![],
                topic_id: TopicId::from_bytes(rand::random()),
                relay: None,
            }
        }

        /// Creates a ticket from environment variables.
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
                            "Created new topic. Reuse with IROH_LIVE_TOPIC={}",
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
