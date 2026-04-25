use std::{collections::HashMap, pin::Pin, sync::Arc, time::Duration};

use bytes::Bytes;
use iroh::{EndpointId, SecretKey};
use iroh_gossip::Gossip;
pub use iroh_gossip::TopicId;
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

pub use self::{
    publisher::RoomPublisherSync,
    ticket::{RoomMode, RoomTicket},
};
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

    /// Sets the policy this peer uses to pick between candidate
    /// transports for a remote peer's broadcasts.
    ///
    /// The new mode applies to existing subscriptions; the room
    /// rebuilds each peer's source set under the new mode and the
    /// subscription's selection policy re-evaluates immediately.
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
        // The builder's `relay` overrides whatever is in the
        // ticket. Surface it in the ticket so downstream code has
        // one source of truth for "the room's relay".
        let mut ticket = builder.ticket.clone();
        if let Some(relay) = builder.relay.clone() {
            ticket.relay = Some(relay);
            ticket.mode = match ticket.mode {
                RoomMode::Gossip => RoomMode::Hybrid,
                other => other,
            };
        }

        // Mode-specific preconditions.
        match ticket.mode {
            RoomMode::Gossip | RoomMode::Hybrid => {
                if live.gossip().is_none() {
                    return Err(anyerr!(
                        "Cannot join room: gossip is required for {:?} mode but not enabled on Live",
                        ticket.mode
                    ));
                }
            }
            RoomMode::Relay => {
                if ticket.relay.is_none() {
                    return Err(anyerr!(
                        "Cannot join room: Relay mode requires a relay attachment on the ticket"
                    ));
                }
            }
        }

        let endpoint = live.endpoint();
        let endpoint_id = endpoint.id();
        let (actor_tx, actor_rx) = mpsc::channel(16);
        let (event_tx, event_rx) = mpsc::channel(16);

        let actor = Actor::new(
            endpoint.secret_key(),
            live.clone(),
            event_tx,
            ticket.clone(),
            builder.subscribe_mode,
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

// Gossip KV message advertising a peer's presence and publish
// state. New fields must be appended (postcard is positional).
// `display_name` cannot use `skip_serializing_if`: a skipped
// `Option` field causes the deserializer to read past the buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerState {
    broadcasts: Vec<String>,
    display_name: Option<String>,
    /// Relay this peer advertises as an alternate source for its
    /// broadcasts. `None` means peers must subscribe directly.
    relay: Option<RelayOffer>,
}

/// Maximum number of broadcasts a peer can advertise in a single
/// gossip [`PeerState`] payload. Hostile peers can otherwise force
/// every subscriber to spawn a [`Subscription`] per fictitious
/// broadcast.
const MAX_PEER_BROADCASTS: usize = 32;

/// Maximum byte length of a peer's advertised broadcast name. The
/// name flows through `Subscription`'s span fields, the gossip
/// announce log, and `BroadcastId`'s `Display`; long names inflate
/// every trace. The cap is generous for typical names and bounds
/// the worst case.
const MAX_BROADCAST_NAME_LEN: usize = 128;

/// Maximum byte length of a peer's `display_name`. Tied to the
/// same log-spam concern as `MAX_BROADCAST_NAME_LEN`.
const MAX_DISPLAY_NAME_LEN: usize = 64;

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
    mode: RoomMode,
    ticket: RoomTicket,
    live: Live,
    /// Gossip-based discovery state. Present in
    /// [`RoomMode::Gossip`] and [`RoomMode::Hybrid`]; absent in
    /// [`RoomMode::Relay`].
    gossip: Option<GossipState>,
    /// Relay-based discovery state. Present in
    /// [`RoomMode::Relay`] and [`RoomMode::Hybrid`]; absent in
    /// [`RoomMode::Gossip`].
    relay_discovery: Option<RelayDiscovery>,
    /// Outbound publisher session at `room/<topic>/<my_id>` on the
    /// relay. Present in [`RoomMode::Relay`] and
    /// [`RoomMode::Hybrid`].
    relay_publisher: Option<MoqSession>,
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
}

/// Gossip discovery state attached to a room actor.
struct GossipState {
    _gossip: Gossip,
    kv: iroh_smol_kv::Client,
    kv_writer: WriteScope,
}

/// Relay discovery state attached to a room actor.
///
/// The discovery session is rooted at `room/<topic>` and the
/// background watcher task forwards parsed `(peer, name, present)`
/// events from the announce stream onto `rx`.
struct RelayDiscovery {
    _session: MoqSession,
    rx: mpsc::Receiver<RelayDiscoveryEvent>,
    _watcher: AbortOnDropHandle<()>,
}

/// Event surfaced by the relay-discovery watcher task.
#[derive(Debug)]
enum RelayDiscoveryEvent {
    /// Relay's primary origin announced a broadcast at the given
    /// path. The path is relative to the discovery session's
    /// root and parses as `<peer>/<broadcast>`.
    Announce { peer: EndpointId, name: String },
    /// Relay's announce stream reports the broadcast went away.
    Unannounce { peer: EndpointId, name: String },
}

/// Tracks an active subscription to one remote peer's broadcast.
///
/// The [`Subscription`] owns the candidate sessions and swaps
/// between them when the preferred source disappears. The
/// `_event_watcher` task forwards
/// [`SubscriptionEvent::ActiveChanged`](crate::SubscriptionEvent::ActiveChanged)
/// from the subscription to the room's [`RoomEvent::BroadcastSwitched`]
/// stream and is aborted when this entry is dropped.
struct ActiveSubscribe {
    multi: Subscription,
    _event_watcher: AbortOnDropHandle<()>,
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
    global_consumer: moq_lite::BroadcastConsumer,
    /// Relay-side announce, present in [`RoomMode::Relay`] and
    /// [`RoomMode::Hybrid`]. Replaced when the relay
    /// configuration changes at runtime.
    relay: Option<RelayAnnounce>,
    /// Wire name the broadcast was registered globally under, in
    /// [`RoomMode::Gossip`] and [`RoomMode::Hybrid`] modes. Held
    /// only for diagnostic purposes; the actual lifetime is
    /// governed by the global Moq actor.
    _direct_wire: Option<String>,
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
    async fn new(
        me: &SecretKey,
        live: Live,
        event_tx: mpsc::Sender<RoomEvent>,
        ticket: RoomTicket,
        subscribe_mode: RoomSubscribeMode,
    ) -> Result<Self> {
        let me_pub = me.public();
        let mode = ticket.mode;
        let relay = ticket.relay.clone();

        let gossip = match mode {
            RoomMode::Gossip | RoomMode::Hybrid => {
                let gossip = live
                    .gossip()
                    .context("gossip is enabled at Live level")?
                    .clone();
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
                Some(GossipState {
                    _gossip: gossip,
                    kv,
                    kv_writer,
                })
            }
            RoomMode::Relay => None,
        };

        let (relay_discovery, relay_publisher) = match mode {
            RoomMode::Relay | RoomMode::Hybrid => {
                let offer = relay
                    .clone()
                    .context("relay attached for Relay/Hybrid mode")?;
                let discovery = open_relay_discovery(&live, &ticket, &offer).await?;
                let publisher = open_relay_publisher(&live, &ticket, me_pub, &offer).await?;
                (Some(discovery), Some(publisher))
            }
            RoomMode::Gossip => (None, None),
        };

        Ok(Self {
            me: me_pub,
            mode,
            ticket,
            live,
            gossip,
            relay_discovery,
            relay_publisher,
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
        })
    }

    pub(crate) async fn run(mut self, mut inbox: mpsc::Receiver<ApiMessage>) {
        // Build the gossip KV update stream when gossip is
        // attached, otherwise the corresponding select branch is
        // a no-op.
        let gossip_updates = self.gossip.as_ref().map(|g| {
            g.kv.subscribe_with_opts(Subscribe {
                mode: SubscribeMode::Both,
                filter: Filter::ALL,
            })
            .stream()
        });
        let mut gossip_updates = gossip_updates.map(Box::pin);

        debug!(mode = ?self.mode, "room actor started");

        // Seed the gossip KV so peers in the topic see us.
        self.update_kv().await;

        loop {
            // Take ownership of the relay discovery rx for the
            // duration of this iteration so the borrow checker is
            // happy with `self` mutations inside select branches.
            let relay_rx_present = self.relay_discovery.is_some();

            tokio::select! {
                update = async {
                    match gossip_updates.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match update {
                        None => {
                            warn!("gossip kv subscription stream ended unexpectedly");
                            break;
                        }
                        Some(Err(err)) => warn!("gossip kv update failed: {err:#}"),
                        Some(Ok(update)) => if !self.handle_gossip_update(update).await { break },
                    }
                }
                event = async {
                    match self.relay_discovery.as_mut() {
                        Some(d) => d.rx.recv().await,
                        None => std::future::pending().await,
                    }
                }, if relay_rx_present => {
                    match event {
                        Some(event) => if !self.handle_relay_discovery_event(event).await { break },
                        None => {
                            warn!("relay discovery channel closed unexpectedly");
                            break;
                        }
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

                            // The active-source watcher was installed
                            // when the subscription was originally
                            // started in `handle_gossip_update`; we
                            // only refresh the `multi` reference
                            // here in case it differs.
                            if let Some(entry) = self.active_subscribe.get_mut(&id) {
                                entry.multi = multi.clone();
                            }

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

    fn spawn_active_watcher(&self, id: BroadcastId, multi: Subscription) -> AbortOnDropHandle<()> {
        let tx = self.event_tx.clone();
        let remote = id.0;
        let name = id.1.clone();
        let mut events = multi.events();
        let last_active = multi.active_id();
        let handle = tokio::spawn(async move {
            let mut last_active = last_active;
            loop {
                match events.recv().await {
                    Ok(crate::SubscriptionEvent::ActiveChanged { current, .. }) => {
                        if current == last_active {
                            continue;
                        }
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
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Re-sync from the watchable on lag.
                        last_active = multi.active_id();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        AbortOnDropHandle::new(handle)
    }

    async fn handle_api_message(&mut self, msg: ApiMessage) {
        match msg {
            ApiMessage::Publish { name, producer } => {
                info!(%name, mode = ?self.mode, "publishing broadcast to room");
                let consume_for_close = producer.consume();
                let global_consumer = producer.consume();

                // Direct-mode wire registration. In Gossip and
                // Hybrid modes, register the wire-uniform name
                // `<my_id>/<name>` globally with the Moq actor so
                // every direct session announces it. In pure Relay
                // mode we skip this; Direct sessions don't carry
                // room broadcasts.
                let direct_wire = match self.mode {
                    RoomMode::Gossip | RoomMode::Hybrid => {
                        let wire = wire_name(self.me, &name);
                        if let Err(err) = self
                            .live
                            .publish_broadcast_producer(wire.clone(), producer)
                            .await
                        {
                            warn!(%name, "failed to register direct broadcast: {err:#}");
                            return;
                        }
                        Some(wire)
                    }
                    RoomMode::Relay => None,
                };

                // Relay-mode announce. The publisher session has
                // root `room/<topic>/<my_id>`, so a bare-name
                // publish lands at `room/<topic>/<my_id>/<name>`
                // on the relay's primary origin, which is the
                // wire-uniform `<my_id>/<name>` form once
                // subscribers strip the room root.
                let relay_announce = match (self.relay_publisher.as_ref(), self.mode) {
                    (Some(session), RoomMode::Relay | RoomMode::Hybrid) => {
                        let consumer = global_consumer.clone();
                        session.publish(name.clone(), consumer.clone());
                        Some(RelayAnnounce {
                            _session: session.clone(),
                            _consumer: consumer,
                        })
                    }
                    _ => None,
                };

                self.active_publish.insert(
                    name.clone(),
                    ActivePublish {
                        global_consumer,
                        relay: relay_announce,
                        _direct_wire: direct_wire,
                    },
                );
                self.publish_closed.push(Box::pin(async move {
                    consume_for_close.closed().await;
                    name
                }));
                info!(
                    broadcasts=?self.active_publish.keys().collect::<Vec<_>>(),
                    "updated active publishes",
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

    /// Re-runs the relay-side publish for every active broadcast.
    ///
    /// In [`RoomMode::Gossip`] this is invoked on
    /// `enable_relay`/`disable_relay`: the actor opens or closes
    /// the publisher session and re-announces on the new state.
    /// In [`RoomMode::Relay`]/[`RoomMode::Hybrid`] the publisher
    /// session is set up at construction; this routine is a no-op
    /// when the relay configuration matches.
    async fn refresh_relay_announces(&mut self) {
        // Open or close the publisher session to match
        // `self.relay`.
        match (&self.relay, self.relay_publisher.as_ref()) {
            (Some(offer), None) => {
                match open_relay_publisher(&self.live, &self.ticket, self.me, offer).await {
                    Ok(session) => self.relay_publisher = Some(session),
                    Err(err) => {
                        warn!("failed to open relay publisher session: {err:#}");
                        return;
                    }
                }
            }
            (None, Some(_)) => {
                self.relay_publisher = None;
            }
            _ => {}
        }

        let names: Vec<String> = self.active_publish.keys().cloned().collect();
        for name in names {
            let new_announce = match (self.relay_publisher.as_ref(), &self.relay) {
                (Some(session), Some(_)) => {
                    let consumer = self
                        .active_publish
                        .get(&name)
                        .map(|s| s.global_consumer.clone());
                    if let Some(consumer) = consumer {
                        session.publish(name.clone(), consumer.clone());
                        Some(RelayAnnounce {
                            _session: session.clone(),
                            _consumer: consumer,
                        })
                    } else {
                        None
                    }
                }
                _ => None,
            };
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
    /// broadcasts.
    ///
    /// The set is mode-aware:
    /// - [`RoomMode::Gossip`]: a `Direct` source for `remote`. If
    ///   the peer advertised a relay hint and the subscribe mode
    ///   permits, the relay is added as a fallback.
    /// - [`RoomMode::Relay`]: a single `Relay` source pointing at
    ///   the room's relay rooted at `room/<topic>`. Subscribing to
    ///   `<remote>/<name>` on that session pulls the peer's
    ///   broadcast.
    /// - [`RoomMode::Hybrid`]: both sources.
    fn sources_for_peer(&self, remote: EndpointId, peer_relay: Option<&RelayOffer>) -> SourceSet {
        let mut set = SourceSet::new();

        let want_direct = matches!(self.mode, RoomMode::Gossip | RoomMode::Hybrid)
            && matches!(
                self.subscribe_mode,
                RoomSubscribeMode::PreferDirect
                    | RoomSubscribeMode::PreferRelay
                    | RoomSubscribeMode::DirectOnly
            );
        let want_relay = matches!(self.mode, RoomMode::Relay | RoomMode::Hybrid)
            && matches!(
                self.subscribe_mode,
                RoomSubscribeMode::PreferDirect
                    | RoomSubscribeMode::PreferRelay
                    | RoomSubscribeMode::RelayOnly
            );
        let want_peer_relay_hint = matches!(self.mode, RoomMode::Gossip)
            && matches!(
                self.subscribe_mode,
                RoomSubscribeMode::PreferDirect
                    | RoomSubscribeMode::PreferRelay
                    | RoomSubscribeMode::RelayOnly
            );

        let direct = if want_direct {
            Some(TransportSource::direct(iroh::EndpointAddr::new(remote)))
        } else {
            None
        };
        let room_relay = if want_relay && let Some(offer) = self.relay.as_ref() {
            use crate::relay::RelayTarget;
            let target = RelayTarget::new(offer.endpoint)
                .with_path(&self.ticket.relay_room_path())
                .with_api_key(offer.api_key.clone());
            Some(TransportSource::relay(target))
        } else {
            None
        };
        let peer_relay = if want_peer_relay_hint {
            peer_relay.map(|offer| TransportSource::relay(offer.to_target()))
        } else {
            None
        };

        match self.subscribe_mode {
            RoomSubscribeMode::PreferDirect | RoomSubscribeMode::DirectOnly => {
                if let Some(s) = direct {
                    set.push(s);
                }
                if let Some(s) = room_relay {
                    set.push(s);
                }
                if let Some(s) = peer_relay {
                    set.push(s);
                }
            }
            RoomSubscribeMode::PreferRelay | RoomSubscribeMode::RelayOnly => {
                if let Some(s) = room_relay {
                    set.push(s);
                }
                if let Some(s) = peer_relay {
                    set.push(s);
                }
                if let Some(s) = direct {
                    set.push(s);
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
            mut broadcasts,
            display_name,
            relay,
        } = value;

        // Enforce caps on attacker-controlled fields. Truncation
        // is preferred over rejection so a single bad field does
        // not silently drop the whole announcement.
        if broadcasts.len() > MAX_PEER_BROADCASTS {
            warn!(
                remote=%remote.fmt_short(),
                received = broadcasts.len(),
                cap = MAX_PEER_BROADCASTS,
                "peer announced too many broadcasts; truncating"
            );
            broadcasts.truncate(MAX_PEER_BROADCASTS);
        }
        broadcasts.retain(|b| {
            let ok = b.len() <= MAX_BROADCAST_NAME_LEN;
            if !ok {
                warn!(remote=%remote.fmt_short(), len=b.len(), "dropping over-long broadcast name");
            }
            ok
        });
        let display_name = display_name.filter(|n| {
            let ok = n.len() <= MAX_DISPLAY_NAME_LEN;
            if !ok {
                warn!(
                    remote=%remote.fmt_short(),
                    len = n.len(),
                    cap = MAX_DISPLAY_NAME_LEN,
                    "dropping over-long display name"
                );
            }
            ok
        });
        let relay = relay.filter(|offer| {
            let ok = offer.validate().is_ok();
            if !ok {
                warn!(remote=%remote.fmt_short(), "dropping invalid relay offer");
            }
            ok
        });

        info!(
            remote=%remote.fmt_short(),
            broadcast_count = broadcasts.len(),
            has_display_name = display_name.is_some(),
            has_relay = relay.is_some(),
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
            self.ensure_subscription(remote, name);
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

    /// Reacts to one event from the relay-discovery watcher.
    ///
    /// Returns `false` if the event-emit loop should stop.
    async fn handle_relay_discovery_event(&mut self, event: RelayDiscoveryEvent) -> bool {
        match event {
            RelayDiscoveryEvent::Announce { peer, name } => {
                if name.len() > MAX_BROADCAST_NAME_LEN {
                    warn!(
                        peer=%peer.fmt_short(),
                        len=name.len(),
                        cap=MAX_BROADCAST_NAME_LEN,
                        "dropping over-long relay announce name"
                    );
                    return true;
                }
                if let std::collections::hash_map::Entry::Vacant(e) = self.known_peers.entry(peer) {
                    e.insert(None);
                    if self
                        .event_tx
                        .send(RoomEvent::PeerJoined {
                            remote: peer,
                            display_name: None,
                        })
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
                self.ensure_subscription(peer, name.clone());
                if self
                    .event_tx
                    .send(RoomEvent::RemoteAnnounced {
                        remote: peer,
                        broadcasts: vec![name],
                        relay: None,
                    })
                    .await
                    .is_err()
                {
                    return false;
                }
                true
            }
            RelayDiscoveryEvent::Unannounce { peer, name } => {
                let id = BroadcastId(peer, name);
                self.active_subscribe.remove(&id);
                let still_active = self.active_subscribe.keys().any(|b| b.0 == peer);
                if !still_active && self.known_peers.remove(&peer).is_some() {
                    let _ = self
                        .event_tx
                        .send(RoomEvent::PeerLeft { remote: peer })
                        .await;
                }
                true
            }
        }
    }

    /// Idempotent: builds (or refreshes) the subscription for a
    /// `(peer, broadcast_name)` pair. Called from both the gossip
    /// and relay-discovery paths.
    fn ensure_subscription(&mut self, remote: EndpointId, name: String) {
        let id = BroadcastId(remote, name.clone());
        if self.active_subscribe.contains_key(&id) {
            debug!(broadcast=%id, "already subscribing to broadcast, skipping");
            return;
        }
        let peer_relay = self
            .peer_relays
            .get(&remote)
            .and_then(|r| r.as_ref())
            .cloned();
        let set = self.sources_for_peer(remote, peer_relay.as_ref());
        if set.is_empty() {
            warn!(broadcast=%id, mode=?self.mode, "no sources for peer under current configuration");
            return;
        }
        let wire = wire_name(remote, &name);
        info!(broadcast=%id, wire=%wire, "initiating multi-source subscription");
        let multi = self.live.subscribe(set, wire);
        let event_watcher = self.spawn_active_watcher(id.clone(), multi.clone());
        self.active_subscribe.insert(
            id.clone(),
            ActiveSubscribe {
                multi: multi.clone(),
                _event_watcher: event_watcher,
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

    async fn update_kv(&self) {
        let Some(gossip) = self.gossip.as_ref() else {
            return;
        };
        let state = PeerState {
            broadcasts: self.active_publish.keys().cloned().collect(),
            display_name: self.display_name.clone(),
            relay: self.relay.clone(),
        };
        if let Err(err) = gossip
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

/// Wire-uniform broadcast name for a peer's broadcast within this
/// room. Direct sessions, gossip-published `PeerState` entries,
/// and relay announces all converge on this string shape.
fn wire_name(peer: EndpointId, broadcast_name: &str) -> String {
    format!("{}/{}", peer, broadcast_name)
}

/// Parses a wire path of the form `<peer>/<broadcast_name>` into
/// its components. Returns `None` for paths with the wrong shape
/// or an unparseable peer id.
fn parse_wire_name(path: &str) -> Option<(EndpointId, String)> {
    let (peer_part, name) = path.split_once('/')?;
    let peer: EndpointId = peer_part.parse().ok()?;
    Some((peer, name.to_string()))
}

/// Opens the relay discovery session for a room.
///
/// The session is rooted at `room/<topic_hex>`. The spawned watcher
/// task forwards parsed announce events through the returned
/// channel.
async fn open_relay_discovery(
    live: &Live,
    ticket: &RoomTicket,
    offer: &RelayOffer,
) -> Result<RelayDiscovery> {
    use crate::relay::RelayTarget;
    let target = RelayTarget::new(offer.endpoint)
        .with_path(&ticket.relay_room_path())
        .with_api_key(offer.api_key.clone());
    let session = live
        .connect_relay(&target)
        .await
        .std_context("connect to relay for room discovery")?;
    let (tx, rx) = mpsc::channel(MAX_PEER_BROADCASTS);
    let me = live.endpoint().id();
    let watcher = spawn_relay_announce_watcher(session.clone(), me, tx);
    Ok(RelayDiscovery {
        _session: session,
        rx,
        _watcher: AbortOnDropHandle::new(watcher),
    })
}

/// Opens the relay publisher session for this peer.
///
/// The session is rooted at `room/<topic_hex>/<my_id>`. Calls to
/// `session.publish(name, ...)` register at
/// `room/<topic_hex>/<my_id>/<name>` on the relay's primary origin.
async fn open_relay_publisher(
    live: &Live,
    ticket: &RoomTicket,
    me: EndpointId,
    offer: &RelayOffer,
) -> Result<MoqSession> {
    use crate::relay::RelayTarget;
    let target = RelayTarget::new(offer.endpoint)
        .with_path(&ticket.relay_publisher_path(me))
        .with_api_key(offer.api_key.clone());
    live.connect_relay(&target)
        .await
        .std_context("connect to relay for publisher session")
}

/// Spawns a task that walks the relay discovery session's announce
/// stream and emits parsed `(peer, name, present)` events.
fn spawn_relay_announce_watcher(
    session: MoqSession,
    me: EndpointId,
    tx: mpsc::Sender<RelayDiscoveryEvent>,
) -> task::JoinHandle<()> {
    let span = tracing::info_span!("RelayDiscovery");
    task::spawn(
        async move {
            let mut consumer = session.origin_consumer().clone();
            loop {
                let Some((path, present)) = consumer.announced().await else {
                    debug!("relay discovery announce stream closed");
                    break;
                };
                let path_str = path.as_str();
                let Some((peer, name)) = parse_wire_name(path_str) else {
                    debug!(path = %path_str, "ignoring unparseable announce path");
                    continue;
                };
                if peer == me {
                    // Echo of our own publish; suppress.
                    continue;
                }
                let event = if present.is_some() {
                    RelayDiscoveryEvent::Announce { peer, name }
                } else {
                    RelayDiscoveryEvent::Unannounce { peer, name }
                };
                if tx.send(event).await.is_err() {
                    debug!("room actor dropped relay discovery channel");
                    break;
                }
            }
        }
        .instrument(span),
    )
}

mod ticket {
    use std::{env, str::FromStr};

    use iroh::EndpointId;
    use iroh_gossip::TopicId;
    use n0_error::{Result, StdResultExt};
    use serde::{Deserialize, Serialize};

    use crate::sources::RelayOffer;

    /// Mode a [`RoomTicket`] joins under.
    ///
    /// The mode selects how peers discover each other and how
    /// broadcasts move between them. The user-facing API
    /// ([`Live::join_room`](crate::Live::join_room),
    /// [`Room::publish`](super::Room::publish)) is identical
    /// across modes; only the wire-level mechanics differ.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[non_exhaustive]
    pub enum RoomMode {
        /// Discovery via gossip; transport via direct iroh
        /// sessions. The gossip topic is `topic_id`. The optional
        /// relay (if attached) acts purely as a hint other peers
        /// can use as a fallback transport.
        Gossip,
        /// Discovery and transport via the relay. No gossip is
        /// used. Peers connect to the relay rooted at
        /// `room/<topic_hex>` and watch its announce stream.
        Relay,
        /// Discovery via both gossip and the relay; transport via
        /// direct sessions when reachable, relay otherwise. Same
        /// gossip topic and relay path as in the single-mode
        /// variants.
        Hybrid,
    }

    /// Ticket for joining a room.
    ///
    /// Carries the room id (`topic_id`), the gossip bootstrap
    /// peers (used in [`RoomMode::Gossip`] and [`RoomMode::Hybrid`]),
    /// the optional relay attachment (required in
    /// [`RoomMode::Relay`] and [`RoomMode::Hybrid`]), and the mode
    /// itself. Serializes to a compact string via the
    /// `iroh_tickets` crate.
    #[derive(Debug, Serialize, Deserialize, Clone, derive_more::Display)]
    #[display("{}", iroh_tickets::Ticket::serialize(self))]
    pub struct RoomTicket {
        /// Peers to contact initially for gossip bootstrap. Empty
        /// in [`RoomMode::Relay`].
        pub bootstrap: Vec<EndpointId>,
        /// The room id. Doubles as the gossip topic in modes that
        /// use gossip and as the relay path component
        /// (`room/<topic_hex>`) in modes that use the relay.
        pub topic_id: TopicId,
        /// Optional relay attachment. Required in
        /// [`RoomMode::Relay`] and [`RoomMode::Hybrid`]; carried
        /// as a hint in [`RoomMode::Gossip`].
        #[serde(default)]
        pub relay: Option<RelayOffer>,
        /// Mode the ticket was minted for.
        #[serde(default = "default_mode")]
        pub mode: RoomMode,
    }

    fn default_mode() -> RoomMode {
        RoomMode::Gossip
    }

    impl RoomTicket {
        /// Creates a ticket with the given topic and bootstrap
        /// peers in [`RoomMode::Gossip`].
        pub fn new(topic_id: TopicId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
            Self {
                bootstrap: bootstrap.into_iter().collect(),
                topic_id,
                relay: None,
                mode: RoomMode::Gossip,
            }
        }

        /// Generates a [`RoomMode::Gossip`] ticket with a random
        /// topic and no bootstrap peers.
        pub fn generate() -> Self {
            Self::new(TopicId::from_bytes(rand::random()), [])
        }

        /// Creates a [`RoomMode::Relay`] ticket with a random topic
        /// id and the given relay attachment. Peers using the
        /// returned ticket discover each other through the relay's
        /// announce stream and route broadcasts through the relay.
        pub fn for_relay(offer: RelayOffer) -> Self {
            Self::for_relay_at(TopicId::from_bytes(rand::random()), offer)
        }

        /// Creates a [`RoomMode::Relay`] ticket with an explicit
        /// topic id.
        pub fn for_relay_at(topic_id: TopicId, offer: RelayOffer) -> Self {
            Self {
                bootstrap: Vec::new(),
                topic_id,
                relay: Some(offer),
                mode: RoomMode::Relay,
            }
        }

        /// Attaches a relay to the ticket.
        ///
        /// Promotes the mode from [`Gossip`](RoomMode::Gossip) to
        /// [`Hybrid`](RoomMode::Hybrid). [`Relay`](RoomMode::Relay)
        /// stays [`Relay`](RoomMode::Relay) but the offer is
        /// replaced.
        #[must_use]
        pub fn with_relay(mut self, offer: RelayOffer) -> Self {
            self.relay = Some(offer);
            self.mode = match self.mode {
                RoomMode::Gossip => RoomMode::Hybrid,
                other => other,
            };
            self
        }

        /// Adds a bootstrap peer.
        #[must_use]
        pub fn with_bootstrap(mut self, peer: EndpointId) -> Self {
            if !self.bootstrap.contains(&peer) {
                self.bootstrap.push(peer);
            }
            self
        }

        /// Returns the mode this ticket was minted for.
        pub fn mode(&self) -> RoomMode {
            self.mode
        }

        /// Returns the relay path component for this room.
        ///
        /// Equal to `format!("room/{}", topic_hex)` where
        /// `topic_hex` is the lowercase-hex of the topic id.
        /// Used by [`RoomMode::Relay`] and [`RoomMode::Hybrid`]
        /// peers when connecting their discovery session.
        pub fn relay_room_path(&self) -> String {
            format!(
                "/room/{}",
                data_encoding::HEXLOWER.encode(self.topic_id.as_bytes())
            )
        }

        /// Returns the per-peer relay path component for this
        /// room. Used by publishers when opening their outbound
        /// publisher session. The peer id is encoded in its full
        /// canonical form so the discovery path components round-
        /// trip through `EndpointId::from_str`.
        pub fn relay_publisher_path(&self, me: EndpointId) -> String {
            format!("{}/{}", self.relay_room_path(), me)
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
