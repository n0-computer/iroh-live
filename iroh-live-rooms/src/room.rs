//! Rooms, the room handle, and the actor behind it.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use iroh::{EndpointId, protocol::ProtocolHandler};
use iroh_gossip::{Gossip, TopicId};
use iroh_moq::{Audience, Moq, Publication, Subscription};
use iroh_smol_kv::{
    ExpiryConfig, Filter, SignedValue, Subscribe, SubscribeItem, SubscribeMode, WriteScope,
};
use moq_net::{Consume, Pattern, broadcast};
use n0_error::{e, stack_error};
use n0_future::{StreamExt, task::AbortOnDropHandle};
use n0_watcher::{Watchable, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{Instrument, debug, info, info_span, trace, warn};

use crate::ticket::RoomTicket;

/// The ALPN rooms speak, iroh-gossip's.
///
/// Mount [`Rooms::protocol_handler`] under it.
pub const ALPN: &[u8] = iroh_gossip::ALPN;

/// The key a member's announcement is stored under in the room's gossip map.
const PEER_STATE_KEY: &[u8] = b"s";

/// How long a member's announcement lives in the gossip map without renewal.
///
/// smol-kv 0.4 sweeps every 30 seconds, so a vanished member drops off two to
/// two and a half minutes after its last renewal. The age is measured by the
/// member's own clock: a member whose clock runs more than this far behind is
/// never seen.
const STATE_HORIZON: Duration = Duration::from_secs(2 * 60);

/// How often a member rewrites its announcement to renew its lease.
const STATE_REFRESH: Duration = Duration::from_secs(30);

/// How often the gossip map should look for announcements past the horizon.
///
/// smol-kv 0.4 ignores it and sweeps every 30 seconds.
const EXPIRY_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// How long leaving waits for each step that needs the network.
///
/// Telling the others and stopping the gossip map wait on peers, who may be
/// gone.
const LEAVE_STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times in a row the room resubscribes to its gossip map.
///
/// smol-kv ends a subscription that falls behind, and a fresh one replays the
/// map. One that keeps ending means the map is gone.
const MAP_RESUBSCRIBES: u32 = 3;

/// Errors of a room.
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// The room's gossip topic could not be joined.
    #[error(transparent)]
    Gossip(iroh_gossip::api::ApiError),
    /// The transport failed a publication, a subscription or a dial.
    #[error(transparent)]
    Transport(iroh_moq::Error),
    /// A broadcast name that is empty or holds a slash.
    #[error("invalid broadcast name {name:?}")]
    InvalidName { name: String },
    /// The room was left.
    #[error("the room was left")]
    Left,
}

/// Rooms on one endpoint.
///
/// Owns the gossip instance all rooms on the endpoint share. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Rooms {
    moq: Moq,
    gossip: Gossip,
}

impl Rooms {
    /// Creates the room service on `moq`'s endpoint, with its own gossip.
    pub fn new(moq: &Moq) -> Self {
        let gossip = Gossip::builder().spawn(moq.endpoint().clone());
        Self::with_gossip(moq, gossip)
    }

    /// Creates the room service over an existing gossip instance.
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

    /// Joins the room `ticket` names, as `display_name` if given.
    ///
    /// Subscribes to the room's gossip topic and starts announcing this
    /// member.
    ///
    /// # Errors
    ///
    /// Fails if the gossip topic cannot be joined.
    pub async fn join(
        &self,
        ticket: &RoomTicket,
        display_name: Option<String>,
    ) -> Result<Room, Error> {
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

        let (leave, inbox) = mpsc::channel(1);
        let inner = Arc::new(Inner {
            me,
            ticket: ticket.clone(),
            moq: self.moq.clone(),
            state: Watchable::new(RoomState::default()),
            members: Watchable::new(BTreeSet::new()),
            leave,
            local: Mutex::new(BTreeMap::new()),
            local_changed: Watchable::new(0),
            display_name: Watchable::new(display_name),
            done: Watchable::new(false),
        });
        let actor = Actor {
            inner: inner.clone(),
            kv,
            writer,
            peers: BTreeMap::new(),
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

/// Who is in a room, and what each member publishes into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomState {
    /// Every other member, by endpoint id. This node is not among them.
    pub peers: BTreeMap<EndpointId, RoomPeer>,
}

/// One member of a room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPeer {
    /// The name the member announced, if any.
    pub display_name: Option<String>,
    /// The broadcasts the member publishes into the room, by name.
    pub broadcasts: BTreeSet<String>,
}

/// A joined room.
///
/// Cheap to clone. The actor behind it runs while any clone exists, and
/// [`leave`](Self::leave) ends it for all of them.
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
    /// Asks the actor to leave, and hears back once it did.
    #[debug(skip)]
    leave: mpsc::Sender<oneshot::Sender<()>>,
    /// This member's publications, by name.
    #[debug(skip)]
    local: Mutex<BTreeMap<String, Local>>,
    /// Bumped whenever [`Inner::local`] changes, so the actor re-announces.
    local_changed: Watchable<u64>,
    display_name: Watchable<Option<String>>,
    done: Watchable<bool>,
}

/// One of this member's publications.
struct Local {
    publication: Publication,
    /// Forgets the entry once the publication is withdrawn, however it goes.
    _withdrawn: AbortOnDropHandle<()>,
}

impl Room {
    /// Returns a ticket for this room with this member as its bootstrap peer.
    pub fn ticket(&self) -> RoomTicket {
        self.inner.ticket.with_bootstrap(self.inner.me)
    }

    /// Returns the members and their broadcasts, as they change.
    pub fn state(&self) -> n0_watcher::Direct<RoomState> {
        self.inner.state.watch()
    }

    /// Publishes `broadcast` into the room as `name`, to its members only.
    ///
    /// The path is `rooms/<topic>/<this member>/<name>`. The broadcast stays in
    /// this member's announcement until it ends, is unpublished, or the room is
    /// left.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::InvalidName`] for an empty name or one holding a
    /// slash, [`Error::Transport`] if the name is already published, and
    /// [`Error::Left`] once the room was left.
    pub fn publish(
        &self,
        name: &str,
        broadcast: impl Consume<broadcast::Consumer>,
    ) -> Result<Publication, Error> {
        if name.is_empty() || name.contains('/') {
            return Err(e!(Error::InvalidName {
                name: name.to_owned()
            }));
        }
        if self.inner.done.get() {
            return Err(e!(Error::Left));
        }
        let topic = self.inner.ticket.topic_id();
        let publication = self.inner.moq.publish(
            room_path(topic, self.inner.me, name),
            broadcast,
            Audience::Peers(self.inner.members.clone()),
        )?;
        info!(%name, path = %publication.path(), "published into the room");
        {
            let mut local = self.inner.local.lock().expect("poisoned");
            // Spawned under the lock, so the task cannot run before the entry
            // exists.
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
    /// Dials the member if needed, and reads what the member itself announces
    /// on that session, so no other peer can stand in for it. Waits until the
    /// member announces `name` to this node, so bound the wait if the member
    /// may never publish it.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Left`] once the room was left, and with
    /// [`Error::Transport`] if the member cannot be dialed, if the node's
    /// grant does not let the member publish under [`publish_scope`], or if
    /// the session ends before the member announces `name`.
    pub async fn subscribe(&self, peer: EndpointId, name: &str) -> Result<Subscription, Error> {
        if self.inner.done.get() {
            return Err(e!(Error::Left));
        }
        let path = room_path(self.inner.ticket.topic_id(), peer, name);
        let session = self.inner.moq.connect(peer).await?;
        Ok(session.subscribe(path).await?)
    }

    /// Sets the name other members see, or clears it.
    pub fn set_display_name(&self, name: Option<String>) {
        self.inner.display_name.set(name).ok();
    }

    /// Leaves the room for every clone of this handle.
    ///
    /// Tells the other members and withdraws this member's publications.
    /// Subscriptions made with [`subscribe`](Self::subscribe) stay open, so
    /// drop them too. Returns within seconds even if every other member is
    /// gone. Idempotent. Not cancellation safe: call it again to finish.
    pub async fn leave(&self) {
        let (reply, reply_rx) = oneshot::channel();
        if self.inner.leave.send(reply).await.is_ok() {
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
    /// Unpublishes this member's broadcasts when the last handle goes
    /// without [`Room::leave`], so rejoining can publish the same names.
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
    fn local_names(&self) -> BTreeSet<String> {
        self.local
            .lock()
            .expect("poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

/// A member's announcement in the room's gossip map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Announcement {
    peer: RoomPeer,
    /// The member left; drop it without waiting for its lease to run out.
    left: bool,
}

struct Actor {
    inner: Arc<Inner>,
    kv: iroh_smol_kv::Client,
    writer: WriteScope,
    peers: BTreeMap<EndpointId, RoomPeer>,
    /// The members the replay after a resubscription has shown so far.
    resync: Option<BTreeSet<EndpointId>>,
}

impl Drop for Actor {
    /// Lets [`Room::leave`] return, even if the actor panicked.
    fn drop(&mut self) {
        self.inner.done.set(true).ok();
    }
}

impl Actor {
    async fn run(mut self, mut inbox: mpsc::Receiver<oneshot::Sender<()>>) {
        let mut updates = Self::subscribe_map(self.kv.clone());

        // One writer, so announcements go out in the order they were decided.
        let desired = Watchable::new(self.announcement(false));
        let announcer = AbortOnDropHandle::new(tokio::spawn(
            announce(self.writer.clone(), desired.watch()).in_current_span(),
        ));
        let mut local_changed = self.inner.local_changed.watch();
        let mut display_name = self.inner.display_name.watch();
        let mut resubscribes = 0;

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
                reply = inbox.recv() => break reply,
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

    /// Withdraws this member's publications, and tells the others if `leaving`.
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
        let local: Vec<Local> = std::mem::take(&mut *self.inner.local.lock().expect("poisoned"))
            .into_values()
            .collect();
        for entry in local {
            entry.publication.unpublish();
        }
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
    fn announcement(&self, left: bool) -> Announcement {
        let broadcasts = if left {
            BTreeSet::new()
        } else {
            self.inner.local_names()
        };
        Announcement {
            peer: RoomPeer {
                display_name: self.inner.display_name.get(),
                broadcasts,
            },
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
            // The replay is complete. After a resubscription, a member it did
            // not show expired in the meantime.
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

    fn handle_entry(&mut self, (remote, key, value): (EndpointId, Bytes, SignedValue)) {
        if remote == self.inner.me || key != PEER_STATE_KEY {
            return;
        }
        let Ok(announcement) = postcard::from_bytes::<Announcement>(&value.value) else {
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
        // these say nothing new.
        let peer = announcement.peer;
        match self.peers.get(&remote) {
            Some(known) if *known == peer => {
                trace!(remote = %remote.fmt_short(), "announcement renewed");
                return;
            }
            Some(_) => debug!(
                remote = %remote.fmt_short(),
                broadcasts = ?peer.broadcasts,
                "member announcement changed",
            ),
            None => info!(
                remote = %remote.fmt_short(),
                display_name = ?peer.display_name,
                "member joined the room",
            ),
        }
        self.peers.insert(remote, peer);
        self.publish_state();
    }

    /// Publishes the membership to the state watcher and the audience set.
    fn publish_state(&self) {
        let members: BTreeSet<EndpointId> = self.peers.keys().copied().collect();
        self.inner.members.set(members).ok();
        self.inner
            .state
            .set(RoomState {
                peers: self.peers.clone(),
            })
            .ok();
    }
}

/// Writes `desired` whenever it changes, and every [`STATE_REFRESH`].
async fn announce(writer: WriteScope, mut desired: n0_watcher::Direct<Announcement>) {
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
async fn put(writer: &WriteScope, announcement: &Announcement) {
    let bytes = postcard::to_stdvec(announcement).expect("an announcement serializes");
    if let Err(err) = writer.put(PEER_STATE_KEY, bytes).await {
        warn!(%err, "failed to write the announcement");
    }
}

/// Returns the paths `member` publishes room broadcasts under, `rooms/*/<member>/**`.
///
/// A node that runs rooms adds this to every peer's grant.
pub fn publish_scope(member: EndpointId) -> Pattern {
    format!("rooms/*/{member}/**")
        .parse()
        .expect("an endpoint id is a valid path segment")
}

/// Returns the path a room broadcast lives at: `rooms/<topic>/<publisher>/<name>`.
pub(crate) fn room_path(topic: TopicId, publisher: EndpointId, name: &str) -> String {
    format!("rooms/{topic}/{publisher}/{name}")
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    /// A member survives two missed refreshes.
    #[test]
    fn several_refreshes_fit_inside_the_expiry_horizon() {
        assert!(STATE_REFRESH * 3 <= STATE_HORIZON);
        assert!(EXPIRY_CHECK_INTERVAL < STATE_REFRESH);
    }

    /// A member leaving says so, and the flag survives the wire.
    #[test]
    fn a_leaving_announcement_decodes_as_left() {
        let announcement = Announcement {
            left: true,
            ..Announcement::default()
        };
        let bytes = postcard::to_stdvec(&announcement).expect("encode");
        let decoded: Announcement = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, announcement);
    }

    #[test]
    fn a_room_path_names_topic_and_publisher() {
        let topic = TopicId::from_bytes([7; 32]);
        let publisher = SecretKey::from_bytes(&[3; 32]).public();
        let path = room_path(topic, publisher, "cam");
        assert_eq!(path, format!("rooms/{topic}/{publisher}/cam"));
        assert!(publish_scope(publisher).matches(&path));
        assert!(!publish_scope(SecretKey::from_bytes(&[4; 32]).public()).matches(&path));
    }
}
