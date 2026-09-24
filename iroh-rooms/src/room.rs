//! Rooms, the room handle, and the actor behind it.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use iroh::{EndpointId, protocol::ProtocolHandler};
use iroh_gossip::{Gossip, TopicId};
use iroh_moq::{Audience, Moq, Publication, Subscription};
use iroh_smol_kv::{
    ExpiryConfig, Filter, SignedValue, Subscribe, SubscribeItem, SubscribeMode, WriteScope,
};
use moq_net::{Consume, broadcast};
use n0_error::{e, stack_error};
use n0_future::{StreamExt, task::AbortOnDropHandle};
use n0_watcher::{Watchable, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast as channel, mpsc, oneshot};
use tracing::{Instrument, debug, info, info_span, trace, warn};

use crate::{
    chat::{self, CHAT_BROADCAST, CHAT_BUFFER, ChatCursor, ChatMessage, ChatReceiver, ChatWriter},
    ticket::RoomTicket,
};

/// The ALPN rooms speak: iroh-gossip's. Mount [`Rooms::protocol_handler`]
/// under it.
pub const ALPN: &[u8] = iroh_gossip::ALPN;

/// The key a member's announcement is stored under in the room's gossip map.
const PEER_STATE_KEY: &[u8] = b"s";

/// The version of the announcement this release writes.
const PEER_STATE_VERSION: u32 = 2;

/// How long a member's announcement survives in the gossip map unrewritten.
///
/// The map is the membership roll, so this is how long a member that vanished
/// without saying so stays on it: long enough to ride out a brief outage, short
/// enough that a room does not accumulate members who left minutes ago.
/// smol-kv 0.4 sweeps expired entries on a fixed 30 second timer, so such a
/// member actually drops off between two and two and a half minutes after its
/// last renewal. The age is measured against the timestamp the member wrote, by
/// its own clock, so a member whose clock runs more than this far behind is
/// never seen at all.
const STATE_HORIZON: Duration = Duration::from_secs(2 * 60);

/// How often a member rewrites its announcement, which renews its lease.
///
/// Several refreshes fit inside [`STATE_HORIZON`], so a member that misses one
/// or two is still a member.
const STATE_REFRESH: Duration = Duration::from_secs(30);

/// How often the gossip map should look for announcements past the horizon.
///
/// smol-kv 0.4 ignores it and sweeps every 30 seconds; it is passed on for the
/// release that honours it.
const EXPIRY_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// How often the room restarts chat readers that stopped.
const CHAT_RETRY: Duration = Duration::from_secs(5);

/// How many commands may wait for the room actor.
const COMMAND_QUEUE: usize = 16;

/// How long leaving waits for each step that needs the network.
///
/// Telling the others and stopping the gossip map both wait on peers; a room
/// whose peers are gone must still be left promptly.
const LEAVE_STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// How often in a row the room resubscribes to its gossip map before it gives
/// up.
///
/// smol-kv ends a subscription that falls behind its internal buffer; a fresh
/// one replays the map, so the room carries on. One that ends again at once,
/// several times over, means the map itself is gone.
const MAP_RESUBSCRIBES: u32 = 3;

/// How far before joining a chat message may have been sent and still count as
/// sent after it, for clocks that disagree a little.
const CHAT_CLOCK_TOLERANCE: Duration = Duration::from_secs(2);

/// Everything that can go wrong in a room.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum Error {
    /// The room's gossip topic could not be joined.
    #[error(transparent)]
    Gossip(iroh_gossip::api::ApiError),
    /// The transport refused: a publication, a subscription, or a dial.
    #[error(transparent)]
    Moq(iroh_moq::Error),
    /// The room's chat broadcast failed: it could not be created, or a
    /// message could not be written to it.
    #[error("the chat broadcast failed")]
    Chat {
        /// What moq-net reported.
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// A broadcast name that a room does not accept: empty, starting with a
    /// dot, which the room keeps for itself, or holding a slash, which would
    /// reach into another member's part of the room's namespace.
    #[error("invalid broadcast name {name:?}")]
    InvalidName {
        /// The name as given.
        name: String,
    },
    /// The room was left.
    #[error("the room was left")]
    Left,
}

/// Rooms on one endpoint.
///
/// Owns the one gossip instance rooms need, so two rooms share it rather than
/// fighting over its ALPN. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Rooms {
    moq: Moq,
    gossip: Gossip,
}

impl Rooms {
    /// Creates the room service for the endpoint `moq` runs on, spawning its
    /// gossip instance.
    pub fn new(moq: &Moq) -> Self {
        let gossip = Gossip::builder().spawn(moq.endpoint().clone());
        Self::with_gossip(moq, gossip)
    }

    /// Creates the room service over a gossip instance the application already
    /// runs.
    pub fn with_gossip(moq: &Moq, gossip: Gossip) -> Self {
        Self {
            moq: moq.clone(),
            gossip,
        }
    }

    /// Returns the handler to mount under [`ALPN`] on the endpoint's router.
    pub fn protocol_handler(&self) -> impl ProtocolHandler + use<> {
        self.gossip.clone()
    }

    /// Joins the room `ticket` names.
    ///
    /// Subscribes to the room's gossip topic, publishes this member's chat
    /// broadcast, and starts announcing this member. Cancellation safe:
    /// dropping the future leaves the topic and publishes nothing.
    ///
    /// # Errors
    ///
    /// Fails if the gossip topic cannot be joined or the chat broadcast cannot
    /// be published.
    pub async fn join(&self, ticket: &RoomTicket, config: RoomConfig) -> Result<Room, Error> {
        let topic = ticket.topic_id();
        let me = self.moq.endpoint().id();
        let gossip_topic = self
            .gossip
            .subscribe(topic, ticket.bootstrap().to_vec())
            .await?;
        let kv = iroh_smol_kv::Client::local(
            gossip_topic,
            iroh_smol_kv::Config {
                anti_entropy_interval: Duration::from_secs(60),
                fast_anti_entropy_interval: Duration::from_secs(1),
                expiry: Some(ExpiryConfig {
                    check_interval: EXPIRY_CHECK_INTERVAL,
                    horizon: STATE_HORIZON,
                }),
            },
        );
        let writer = kv.write(self.moq.endpoint().secret_key().clone());

        let members = Watchable::new(BTreeSet::new());
        let chat = ChatWriter::new().map_err(|source| e!(Error::Chat { source }))?;
        let chat_publication = self.moq.publish_at_with_legacy(
            room_path(topic, me, CHAT_BROADCAST),
            legacy_room_path(topic, CHAT_BROADCAST),
            chat.consume(),
            Audience::Peers(members.watch()),
        )?;
        let (chat_tx, _) = channel::channel(CHAT_BUFFER);
        let (commands, inbox) = mpsc::channel(COMMAND_QUEUE);
        let inner = Arc::new(Inner {
            me,
            ticket: ticket.clone(),
            moq: self.moq.clone(),
            state: Watchable::new(RoomState::default()),
            members,
            chat: Mutex::new(Some(chat_tx)),
            commands,
            local: Mutex::new(BTreeMap::new()),
            local_changed: Watchable::new(0),
            display_name: Watchable::new(config.display_name),
            legacy_peers: Mutex::new(BTreeSet::new()),
            chat_since: SystemTime::now() - CHAT_CLOCK_TOLERANCE,
            done: Watchable::new(false),
        });
        let actor = Actor {
            inner: inner.clone(),
            kv,
            writer,
            peers: BTreeMap::new(),
            chat,
            chat_publication,
            resync: None,
        };
        let span = info_span!("room", topic = %topic.fmt_short(), me = %me.fmt_short());
        let task = tokio::spawn(actor.run(inbox).instrument(span));
        info!(topic = %topic.fmt_short(), "joined room");
        Ok(Room {
            inner,
            _actor: Arc::new(AbortOnDropHandle::new(task)),
        })
    }
}

/// How to join a room.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RoomConfig {
    /// The name other members see, if any.
    pub display_name: Option<String>,
}

impl RoomConfig {
    /// Sets the name other members see.
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }
}

/// Who is in a room, and what each member publishes into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RoomState {
    /// Every other member, by endpoint id. This node is not among them.
    pub peers: BTreeMap<EndpointId, RoomPeer>,
}

/// One member of a room.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RoomPeer {
    /// The name the member announced, if any.
    pub display_name: Option<String>,
    /// The broadcasts the member publishes into the room, by name.
    pub broadcasts: BTreeSet<String>,
}

/// A joined room.
///
/// Cheap to clone. The actor behind it runs while any clone exists, and
/// [`leave`](Self::leave) ends it for all of them. Nothing a caller does with
/// the state or chat receivers can stall it: the state is a watcher, and every
/// chat receiver has its own buffer.
#[derive(Debug, Clone)]
pub struct Room {
    inner: Arc<Inner>,
    _actor: Arc<AbortOnDropHandle<()>>,
}

/// What every handle of a room shares with its actor.
#[derive(derive_more::Debug)]
struct Inner {
    me: EndpointId,
    ticket: RoomTicket,
    #[debug(skip)]
    moq: Moq,
    state: Watchable<RoomState>,
    /// The members' ids, which every publication into the room is offered to.
    members: Watchable<BTreeSet<EndpointId>>,
    /// Taken when the room is left, which ends every receiver.
    #[debug(skip)]
    chat: Mutex<Option<channel::Sender<ChatMessage>>>,
    #[debug(skip)]
    commands: mpsc::Sender<Command>,
    /// This member's publications, by name.
    #[debug(skip)]
    local: Mutex<BTreeMap<String, Local>>,
    /// Bumped whenever [`Inner::local`] changes, so the actor re-announces.
    local_changed: Watchable<u64>,
    display_name: Watchable<Option<String>>,
    /// Members that announce the layout before paths named their publisher.
    legacy_peers: Mutex<BTreeSet<EndpointId>>,
    /// Chat sent before this is history from before joining.
    chat_since: SystemTime,
    done: Watchable<bool>,
}

/// One of this member's publications.
struct Local {
    publication: Publication,
    /// Forgets the entry once the publication is withdrawn, however it goes.
    _withdrawn: AbortOnDropHandle<()>,
}

enum Command {
    Chat {
        text: String,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Leave {
        reply: oneshot::Sender<()>,
    },
}

impl Room {
    /// Returns a ticket for this room with this member as its bootstrap peer.
    pub fn ticket(&self) -> RoomTicket {
        self.inner.ticket.with_bootstrap(self.inner.me)
    }

    /// Returns the room's membership and what each member publishes, as it
    /// changes.
    pub fn state(&self) -> n0_watcher::Direct<RoomState> {
        self.inner.state.watch()
    }

    /// Publishes `broadcast` into the room as `name`, to its members only.
    ///
    /// The path is `rooms/<topic>/<this member>/<name>`, and the audience is the
    /// room's membership as it changes, so the broadcast is offered to exactly
    /// the members and to nobody else who connects. It stays in this member's
    /// announcement until it is withdrawn: when the broadcast ends, with
    /// [`Publication::unpublish`], or when the room is left.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidName`] for an empty name, one starting with a
    /// dot, or one holding a slash, [`Error::Moq`] if the name is already
    /// published, and [`Error::Left`] once the room was left.
    pub fn publish(
        &self,
        name: &str,
        broadcast: impl Consume<broadcast::Consumer>,
    ) -> Result<Publication, Error> {
        if name.is_empty() || name.starts_with('.') || name.contains('/') {
            return Err(e!(Error::InvalidName {
                name: name.to_owned()
            }));
        }
        if self.inner.done.get() {
            return Err(e!(Error::Left));
        }
        let topic = self.inner.ticket.topic_id();
        let publication = self.inner.moq.publish_at_with_legacy(
            room_path(topic, self.inner.me, name),
            legacy_room_path(topic, name),
            broadcast,
            Audience::Peers(self.inner.members.watch()),
        )?;
        info!(%name, path = %publication.path(), "published into the room");
        {
            let mut local = self.inner.local.lock().expect("poisoned");
            // Spawned under the lock the task takes to forget the entry, so it
            // cannot run before the entry is there, whenever the publication
            // is withdrawn.
            let withdrawn = {
                let inner = Arc::downgrade(&self.inner);
                let name = name.to_owned();
                let publication = publication.clone();
                AbortOnDropHandle::new(tokio::spawn(async move {
                    publication.withdrawn().await;
                    if let Some(inner) = inner.upgrade() {
                        debug!(%name, "room broadcast withdrawn");
                        inner.remove_local(&name, &publication);
                    }
                }))
            };
            local.insert(
                name.to_owned(),
                Local {
                    publication: publication.clone(),
                    _withdrawn: withdrawn,
                },
            );
        }
        self.inner.bump_local();
        Ok(publication)
    }

    /// Resolves `peer`'s broadcast `name` over the session with that member.
    ///
    /// Dials the member if there is no session yet, and reads the broadcast
    /// from what the member itself announces on that session, so no other peer
    /// can stand in for it. Room broadcasts go to members directly and never
    /// through a relay. Waits until the member announces `name` to this node,
    /// which it does once it counts this node as a member and for as long as
    /// it publishes `name`, so a caller that may ask for a name the member
    /// never published should bound the wait. Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Left`] once the room was left, and with
    /// [`Error::Moq`] if the member cannot be dialed or its session ends before
    /// it announces `name`.
    pub async fn subscribe(&self, peer: EndpointId, name: &str) -> Result<Subscription, Error> {
        if self.inner.done.get() {
            return Err(e!(Error::Left));
        }
        let topic = self.inner.ticket.topic_id();
        let legacy = self
            .inner
            .legacy_peers
            .lock()
            .expect("poisoned")
            .contains(&peer);
        // A member on the older layout publishes at a path that names no
        // publisher, which only means something on the session with it.
        let path = if legacy {
            legacy_room_path(topic, name)
        } else {
            room_path(topic, peer, name)
        };
        let session = self.inner.moq.connect(peer).await?;
        Ok(session.subscribe(path).await?)
    }

    /// Sets the name other members see, or clears it.
    pub fn set_display_name(&self, name: Option<String>) {
        self.inner.display_name.set(name).ok();
    }

    /// Sends a chat message to the room.
    ///
    /// Returns once the message is written to this member's chat broadcast.
    /// Not cancellation safe: a dropped call may or may not have sent it.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Left`] once the room was left.
    pub async fn send_chat(&self, text: impl Into<String>) -> Result<(), Error> {
        let (reply, reply_rx) = oneshot::channel();
        self.inner
            .commands
            .send(Command::Chat {
                text: text.into(),
                reply,
            })
            .await
            .map_err(|_| e!(Error::Left))?;
        reply_rx.await.map_err(|_| e!(Error::Left))?
    }

    /// Returns a receiver of the chat messages other members send from now on.
    ///
    /// Returns a receiver that ends at once if the room was left.
    pub fn chat(&self) -> ChatReceiver {
        let chat = self.inner.chat.lock().expect("poisoned");
        let rx = match chat.as_ref() {
            Some(chat) => chat.subscribe(),
            // A sender dropped at once closes the receiver it made.
            None => channel::channel(1).0.subscribe(),
        };
        ChatReceiver::new(rx)
    }

    /// Leaves the room for every clone of this handle.
    ///
    /// Tells the other members, withdraws this member's publications and chat,
    /// and ends every chat receiver. The subscriptions this member made with
    /// [`subscribe`](Self::subscribe) are the caller's and stay open; drop or
    /// close them as well. Steps that need the network are bounded, so this
    /// returns within seconds even with every other member gone. Idempotent;
    /// not cancellation safe, call it again to finish.
    pub async fn leave(&self) {
        let (reply, reply_rx) = oneshot::channel();
        if self
            .inner
            .commands
            .send(Command::Leave { reply })
            .await
            .is_ok()
        {
            reply_rx.await.ok();
        }
        let mut done = self.inner.done.watch();
        while !done.get() {
            if done.updated().await.is_err() {
                break;
            }
        }
    }
}

impl Drop for Inner {
    /// Withdraws this member's publications when the last handle of a room
    /// that was never left goes, rather than leaving them offered to a
    /// membership nobody updates any more.
    fn drop(&mut self) {
        for local in self.local.get_mut().expect("poisoned").values() {
            local.publication.unpublish();
        }
    }
}

impl Inner {
    /// Forgets the local publication `name`, if it is still `publication`.
    ///
    /// Compared by identity: `name` may have been published anew since, and
    /// the new publication must stay.
    fn remove_local(&self, name: &str, publication: &Publication) {
        let mut local = self.local.lock().expect("poisoned");
        if local
            .get(name)
            .is_some_and(|entry| entry.publication == *publication)
        {
            local.remove(name);
            drop(local);
            self.bump_local();
        }
    }

    fn bump_local(&self) {
        self.local_changed.set(self.local_changed.get() + 1).ok();
    }

    /// Returns the names this member publishes into the room.
    fn local_names(&self) -> Vec<String> {
        self.local
            .lock()
            .expect("poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

/// A member's announcement, as this release writes it.
///
/// Postcard is positional: the first two fields are exactly the announcement
/// the release before this one wrote and reads, and it ignores the bytes after
/// them. Do not reorder them, and add new fields at the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PeerState {
    /// Every broadcast name, the chat broadcast's included, so a member on the
    /// older release subscribes to it and reads the chat track it expects.
    broadcasts: Vec<String>,
    /// Do not use `skip_serializing_if` here: postcard is positional, and a
    /// skipped field makes the reader take the next field's bytes for this one.
    display_name: Option<String>,
    version: u32,
    /// The name of the member's chat broadcast.
    chat: Option<String>,
    /// The member left; drop it without waiting for its lease to run out.
    left: bool,
}

/// A member's announcement as the release before this one wrote it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyPeerState {
    broadcasts: Vec<String>,
    display_name: Option<String>,
}

/// What this member knows about another from its announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Announcement {
    display_name: Option<String>,
    broadcasts: BTreeSet<String>,
    /// The member writes the older announcement and path layout.
    legacy: bool,
    left: bool,
}

impl Announcement {
    /// Reads an announcement in either layout.
    fn parse(bytes: &[u8]) -> Option<Self> {
        // The older layout is a prefix of the current one, so it always decodes
        // as that; the current one is tried first and tells them apart by its
        // version field.
        if let Ok(state) = postcard::from_bytes::<PeerState>(bytes)
            && state.version >= PEER_STATE_VERSION
        {
            let chat = state.chat.as_deref();
            return Some(Self {
                display_name: state.display_name,
                broadcasts: state
                    .broadcasts
                    .into_iter()
                    .filter(|name| Some(name.as_str()) != chat)
                    .collect(),
                legacy: false,
                left: state.left,
            });
        }
        let state = postcard::from_bytes::<LegacyPeerState>(bytes).ok()?;
        Some(Self {
            display_name: state.display_name,
            broadcasts: state.broadcasts.into_iter().collect(),
            legacy: true,
            left: false,
        })
    }
}

/// Another member, as the actor tracks it.
///
/// Removing the entry drops its tasks, which aborts them, so nothing a removed
/// member started can come back and change the room.
struct Peer {
    announcement: Announcement,
    /// The member's chat readers, by the broadcast each reads: its chat
    /// broadcast for a current member, each of its broadcasts for one on the
    /// older release.
    chat: BTreeMap<String, ChatReader>,
}

/// One chat source of a member, and how far it was read.
struct ChatReader {
    /// `None` until started, and while the room cannot deliver chat.
    task: Option<AbortOnDropHandle<()>>,
    /// Kept when the task restarts, so a restart does not deliver again.
    cursor: Arc<ChatCursor>,
}

impl Peer {
    fn new(announcement: Announcement) -> Self {
        Self {
            announcement,
            chat: BTreeMap::new(),
        }
    }

    /// Returns the broadcasts this member's chat is read from.
    fn chat_sources(&self) -> BTreeSet<String> {
        if self.announcement.legacy {
            self.announcement.broadcasts.clone()
        } else {
            BTreeSet::from([CHAT_BROADCAST.to_owned()])
        }
    }

    /// Reports whether a chat source of this member has no running reader.
    fn chat_stalled(&self) -> bool {
        self.chat_sources().iter().any(|name| {
            self.chat
                .get(name)
                .and_then(|reader| reader.task.as_ref())
                .is_none_or(|task| task.is_finished())
        })
    }
}

type KvEntry = (EndpointId, Bytes, SignedValue);

struct Actor {
    inner: Arc<Inner>,
    kv: iroh_smol_kv::Client,
    writer: WriteScope,
    peers: BTreeMap<EndpointId, Peer>,
    chat: ChatWriter,
    chat_publication: Publication,
    /// The members a replay of the gossip map has shown so far, while one runs
    /// after a resubscription.
    resync: Option<BTreeSet<EndpointId>>,
}

impl Drop for Actor {
    /// Ends every chat receiver and lets [`Room::leave`] return, also when
    /// the actor panicked or its last handle went without leaving.
    fn drop(&mut self) {
        if let Ok(mut chat) = self.inner.chat.lock() {
            chat.take();
        }
        self.inner.done.set(true).ok();
    }
}

impl Actor {
    async fn run(mut self, mut inbox: mpsc::Receiver<Command>) {
        let mut updates = Self::subscribe_map(self.kv.clone());

        // One writer, so announcements go out in the order they were decided.
        let desired = Watchable::new(self.announcement(false));
        let announcer = AbortOnDropHandle::new(tokio::spawn(
            announce(self.writer.clone(), desired.watch()).in_current_span(),
        ));
        let mut local_changed = self.inner.local_changed.watch();
        let mut display_name = self.inner.display_name.watch();
        let mut resubscribes = 0;
        let mut chat_retry = tokio::time::interval(CHAT_RETRY);
        chat_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let leave = loop {
            let mut ended = false;
            tokio::select! {
                update = updates.next() => match update {
                    None => ended = true,
                    Some(Err(err)) => warn!(%err, "gossip map update failed"),
                    Some(Ok(item)) => {
                        resubscribes = 0;
                        self.handle_item(item);
                    }
                },
                changed = local_changed.updated() => {
                    if changed.is_err() {
                        break None;
                    }
                    desired.set(self.announcement(false)).ok();
                }
                changed = display_name.updated() => {
                    if changed.is_err() {
                        break None;
                    }
                    desired.set(self.announcement(false)).ok();
                }
                _ = chat_retry.tick() => self.restart_stalled_chat(),
                command = inbox.recv() => match command {
                    None => break None,
                    Some(Command::Chat { text, reply }) => {
                        let sent = self
                            .chat
                            .send(&text)
                            .map_err(|source| e!(Error::Chat { source }));
                        reply.send(sent).ok();
                    }
                    Some(Command::Leave { reply }) => break Some(reply),
                },
            }
            if ended {
                if resubscribes == MAP_RESUBSCRIBES {
                    warn!("the room's gossip map ended");
                    break None;
                }
                resubscribes += 1;
                warn!(
                    attempt = resubscribes,
                    "gossip map subscription ended, resubscribing"
                );
                updates = Self::subscribe_map(self.kv.clone());
                self.resync = Some(BTreeSet::new());
            }
        };
        // Stopped first, so a refresh cannot write over the announcement that
        // says this member left.
        drop(announcer);
        self.shut_down(leave.is_some()).await;
        if let Some(reply) = leave {
            reply.send(()).ok();
        }
    }

    /// Subscribes to the gossip map: every entry there is now, then changes.
    fn subscribe_map(
        kv: iroh_smol_kv::Client,
    ) -> std::pin::Pin<
        Box<impl n0_future::Stream<Item = Result<SubscribeItem, impl std::fmt::Display>> + Send>,
    > {
        // The raw stream rather than `stream()`, which drops the expiry items
        // membership is read out of.
        Box::pin(
            kv.subscribe_with_opts(Subscribe {
                mode: SubscribeMode::Both,
                filter: Filter::ALL,
            })
            .stream_raw(),
        )
    }

    /// Withdraws everything this member put into the room, telling the others
    /// when it is leaving on purpose.
    async fn shut_down(&mut self, leaving: bool) {
        if leaving {
            info!("leaving the room");
            let told = tokio::time::timeout(
                LEAVE_STEP_TIMEOUT,
                put(&self.writer, &self.announcement(true)),
            )
            .await;
            if told.is_err() {
                warn!(
                    "could not tell the others in time; they drop this member when its lease ends"
                );
            }
        }
        self.inner.chat.lock().expect("poisoned").take();
        let local: Vec<Local> = std::mem::take(&mut *self.inner.local.lock().expect("poisoned"))
            .into_values()
            .collect();
        for entry in local {
            entry.publication.unpublish();
        }
        self.chat_publication.unpublish();
        self.chat.finish();
        self.peers.clear();
        self.publish_state();
        match tokio::time::timeout(LEAVE_STEP_TIMEOUT, self.kv.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => debug!(%err, "gossip map already stopped"),
            Err(_) => warn!("the gossip map did not stop in time"),
        }
        self.inner.done.set(true).ok();
    }

    /// Returns this member's announcement as it stands.
    fn announcement(&self, left: bool) -> PeerState {
        let mut broadcasts = if left {
            Vec::new()
        } else {
            self.inner.local_names()
        };
        if !left {
            broadcasts.push(CHAT_BROADCAST.to_owned());
        }
        PeerState {
            broadcasts,
            display_name: self.inner.display_name.get(),
            version: PEER_STATE_VERSION,
            chat: Some(CHAT_BROADCAST.to_owned()),
            left,
        }
    }

    fn handle_item(&mut self, item: SubscribeItem) {
        match item {
            SubscribeItem::Entry(entry) => self.handle_entry(entry),
            SubscribeItem::Expired((remote, key, _timestamp)) => {
                if remote != self.inner.me
                    && key == PEER_STATE_KEY
                    && self.peers.remove(&remote).is_some()
                {
                    info!(
                        remote = %remote.fmt_short(),
                        horizon = ?STATE_HORIZON,
                        "member stopped announcing, treating it as gone",
                    );
                    self.publish_state();
                }
            }
            // The boundary between entries that were there when the
            // subscription opened and the ones that arrive from here on. After
            // a resubscription, a member the replay did not show expired while
            // the room was not listening.
            SubscribeItem::CurrentDone => {
                if let Some(seen) = self.resync.take() {
                    let before = self.peers.len();
                    self.peers.retain(|remote, _| seen.contains(remote));
                    if self.peers.len() != before {
                        info!(
                            gone = before - self.peers.len(),
                            "members expired while resubscribing"
                        );
                        self.publish_state();
                    }
                }
            }
        }
    }

    fn handle_entry(&mut self, (remote, key, value): KvEntry) {
        if remote == self.inner.me || key != PEER_STATE_KEY {
            return;
        }
        let Some(announcement) = Announcement::parse(&value.value) else {
            warn!(
                remote = %remote.fmt_short(),
                len = value.value.len(),
                "member announcement does not decode",
            );
            return;
        };
        if announcement.left {
            if self.peers.remove(&remote).is_some() {
                info!(remote = %remote.fmt_short(), "member left the room");
                self.publish_state();
            }
            return;
        }
        if let Some(seen) = self.resync.as_mut() {
            seen.insert(remote);
        }
        // Every member rewrites its announcement to renew its lease, so most of
        // these say nothing new. A repeat restarts chat readers that stopped,
        // which is how a member whose session dropped is read again.
        let known = self.peers.get(&remote);
        let changed = known.is_none_or(|peer| peer.announcement != announcement);
        if !changed && known.is_some_and(|peer| !peer.chat_stalled()) {
            trace!(remote = %remote.fmt_short(), "announcement renewed");
            return;
        }
        match known {
            None => info!(
                remote = %remote.fmt_short(),
                display_name = ?announcement.display_name,
                legacy = announcement.legacy,
                "member joined the room",
            ),
            Some(_) if changed => debug!(
                remote = %remote.fmt_short(),
                broadcasts = ?announcement.broadcasts,
                "member announcement changed",
            ),
            Some(_) => debug!(remote = %remote.fmt_short(), "restarting a stopped chat reader"),
        }
        let mut peer = self
            .peers
            .remove(&remote)
            .unwrap_or_else(|| Peer::new(announcement.clone()));
        peer.announcement = announcement;
        self.sync_chat(remote, &mut peer);
        self.peers.insert(remote, peer);
        if changed {
            self.publish_state();
        }
    }

    /// Restarts the chat readers that stopped, of every member still here.
    ///
    /// A reader stops when its member's session drops, and the member is
    /// usually back within seconds, well before its next announcement would
    /// bring the reader back.
    fn restart_stalled_chat(&mut self) {
        let stalled: Vec<EndpointId> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.chat_stalled())
            .map(|(remote, _)| *remote)
            .collect();
        for remote in stalled {
            if let Some(mut peer) = self.peers.remove(&remote) {
                trace!(remote = %remote.fmt_short(), "restarting a stopped chat reader");
                self.sync_chat(remote, &mut peer);
                self.peers.insert(remote, peer);
            }
        }
    }

    /// Runs one chat reader per chat source of member `remote`.
    ///
    /// Keeps the readers that still run, so a change to what the member
    /// publishes does not restart them; starts the missing and stopped ones
    /// over their cursors, and drops the ones for sources that went.
    fn sync_chat(&self, remote: EndpointId, peer: &mut Peer) {
        let sources = peer.chat_sources();
        peer.chat.retain(|name, _| sources.contains(name));
        let Some(tx) = self.inner.chat.lock().expect("poisoned").clone() else {
            return;
        };
        let topic = self.inner.ticket.topic_id();
        let legacy = peer.announcement.legacy;
        for name in sources {
            let reader = peer.chat.entry(name.clone()).or_insert_with(|| ChatReader {
                task: None,
                cursor: Arc::new(ChatCursor::new(self.inner.chat_since)),
            });
            if reader.task.as_ref().is_some_and(|task| !task.is_finished()) {
                continue;
            }
            let path = if legacy {
                legacy_room_path(topic, &name)
            } else {
                room_path(topic, remote, &name)
            };
            reader.task = Some(AbortOnDropHandle::new(tokio::spawn(
                read_chat(
                    self.inner.moq.clone(),
                    remote,
                    path,
                    legacy,
                    reader.cursor.clone(),
                    tx.clone(),
                )
                .in_current_span(),
            )));
        }
    }

    /// Publishes the membership to the state watcher and the audience set.
    fn publish_state(&self) {
        let state = RoomState {
            peers: self
                .peers
                .iter()
                .map(|(id, peer)| {
                    (
                        *id,
                        RoomPeer {
                            display_name: peer.announcement.display_name.clone(),
                            broadcasts: peer.announcement.broadcasts.clone(),
                        },
                    )
                })
                .collect(),
        };
        let members: BTreeSet<EndpointId> = self.peers.keys().copied().collect();
        let legacy: BTreeSet<EndpointId> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.announcement.legacy)
            .map(|(id, _)| *id)
            .collect();
        *self.inner.legacy_peers.lock().expect("poisoned") = legacy;
        self.inner.members.set(members).ok();
        self.inner.state.set(state).ok();
    }
}

/// Reads one chat source of member `remote` into `tx`.
///
/// Over the session with the member, never the route table: what the member
/// announces on its own session is its chat, and no other peer can put a
/// broadcast there.
async fn read_chat(
    moq: Moq,
    remote: EndpointId,
    path: String,
    legacy: bool,
    cursor: Arc<ChatCursor>,
    tx: channel::Sender<ChatMessage>,
) {
    let session = match moq.connect(remote).await {
        Ok(session) => session,
        Err(err) => {
            debug!(remote = %remote.fmt_short(), %err, "member unreachable");
            return;
        }
    };
    match session.subscribe(path).await {
        Ok(subscription) => chat::forward(remote, subscription.as_moq(), legacy, cursor, tx).await,
        Err(err) => debug!(remote = %remote.fmt_short(), %err, "member chat unreachable"),
    }
}

/// Writes the announcement `desired` holds whenever it changes, and every
/// [`STATE_REFRESH`] to renew this member's lease.
async fn announce(writer: WriteScope, mut desired: n0_watcher::Direct<PeerState>) {
    let mut refresh = tokio::time::interval(STATE_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        put(&writer, &desired.get()).await;
        tokio::select! {
            changed = desired.updated() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = refresh.tick() => trace!("renewing the announcement"),
        }
    }
}

/// Writes `state` under [`PEER_STATE_KEY`].
async fn put(writer: &WriteScope, state: &PeerState) {
    let bytes = postcard::to_stdvec(state).expect("an announcement serializes");
    if let Err(err) = writer.put(PEER_STATE_KEY, bytes).await {
        warn!(%err, "failed to write the announcement");
    }
}

/// Returns the path a room broadcast lives at: `rooms/<topic>/<publisher>/<name>`.
///
/// The topic scopes it, so a member of two rooms can publish "cam" in each, and
/// the publisher's id makes one broadcast one path over every link.
pub(crate) fn room_path(topic: TopicId, publisher: EndpointId, name: &str) -> String {
    format!("rooms/{topic}/{publisher}/{name}")
}

/// Returns the path the release before publisher-named paths used for a room
/// broadcast: `rooms/<topic>/<name>`.
fn legacy_room_path(topic: TopicId, name: &str) -> String {
    format!("rooms/{topic}/{name}")
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    /// The refresh interval and the expiry horizon are one mechanism split
    /// across two constants: a member has to survive two missed refreshes.
    #[test]
    fn several_refreshes_fit_inside_the_expiry_horizon() {
        assert!(STATE_REFRESH * 3 <= STATE_HORIZON);
        assert!(EXPIRY_CHECK_INTERVAL < STATE_REFRESH);
    }

    fn current(broadcasts: &[&str], display_name: Option<&str>) -> PeerState {
        let mut names: Vec<String> = broadcasts.iter().map(|name| (*name).to_owned()).collect();
        names.push(CHAT_BROADCAST.to_owned());
        PeerState {
            broadcasts: names,
            display_name: display_name.map(str::to_owned),
            version: PEER_STATE_VERSION,
            chat: Some(CHAT_BROADCAST.to_owned()),
            left: false,
        }
    }

    /// The release before this one decodes what this one writes: the older
    /// layout is a positional prefix, and postcard ignores the rest.
    #[test]
    fn an_older_member_reads_the_current_announcement() {
        let bytes = postcard::to_stdvec(&current(&["cam"], Some("Alice"))).expect("encode");
        let old: LegacyPeerState = postcard::from_bytes(&bytes).expect("an older reader");
        assert_eq!(old.broadcasts, ["cam", CHAT_BROADCAST]);
        assert_eq!(old.display_name.as_deref(), Some("Alice"));
    }

    /// This release reads both layouts and tells them apart.
    #[test]
    fn a_current_member_reads_both_announcements() {
        let bytes = postcard::to_stdvec(&current(&["cam"], None)).expect("encode");
        let parsed = Announcement::parse(&bytes).expect("current");
        assert!(!parsed.legacy);
        assert_eq!(
            parsed.broadcasts,
            BTreeSet::from(["cam".to_owned()]),
            "the chat broadcast is not a media broadcast"
        );

        for display_name in [Some("Bob".to_owned()), None] {
            let legacy = LegacyPeerState {
                broadcasts: vec!["cam".into(), "screen".into()],
                display_name: display_name.clone(),
            };
            let bytes = postcard::to_stdvec(&legacy).expect("encode");
            let parsed = Announcement::parse(&bytes).expect("legacy");
            assert!(parsed.legacy);
            assert_eq!(parsed.display_name, display_name);
            assert_eq!(parsed.broadcasts.len(), 2);
        }
    }

    /// A member leaving says so, and the flag survives the wire.
    #[test]
    fn a_leaving_announcement_decodes_as_left() {
        let mut state = current(&[], None);
        state.left = true;
        let bytes = postcard::to_stdvec(&state).expect("encode");
        assert!(Announcement::parse(&bytes).expect("parse").left);
    }

    #[test]
    fn a_room_path_names_topic_and_publisher() {
        let topic = TopicId::from_bytes([7; 32]);
        let publisher = SecretKey::from_bytes(&[3; 32]).public();
        let path = room_path(topic, publisher, "cam");
        assert_eq!(path, format!("rooms/{topic}/{publisher}/cam"));
        assert_eq!(
            iroh_moq::publisher_of(&moq_net::Path::new(&path)),
            Some(publisher),
            "the transport dials the publisher a room path names",
        );
        assert_eq!(legacy_room_path(topic, "cam"), format!("rooms/{topic}/cam"));
    }
}
