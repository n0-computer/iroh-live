//! One-to-one calls over a layout's call path.
//!
//! [`Calls`] publishes this node's side of its calls at `<root>/<its id>/call`
//! and offers it to nobody until [`ring`](Calls::ring) adds a peer to its
//! audience. A peer learns it is called when that path appears in its route
//! table. It answers by ringing back and subscribing to the caller's side,
//! and the caller, which waits for exactly that, subscribes to the answerer's.
//! Two peers that ring each other at once are the normal case of this
//! protocol, and resolve into one [`Call`] on each side.
//!
//! A peer that was hung up on rings again only once it withdraws its side and
//! offers it anew. Publishing the side again without a withdrawal in between
//! is no new offer, since the route table lists the path as present
//! throughout.
//!
//! `B`, what fills the side, stays generic: a `LocalBroadcast` for media, or
//! a bare `broadcast::Producer`.
//!
//! ```no_run
//! use iroh_live::{Node, calls::{CallEvent, Calls}, layout::LiveLayout};
//! use moq_net::broadcast;
//! use n0_future::StreamExt;
//!
//! # async fn run(node: Node, peer: iroh::EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let (calls, _side): (Calls, broadcast::Producer) = Calls::new(&node, &LiveLayout::default())?;
//!
//! // Place a call: ring, wait for the peer to answer, subscribe.
//! let call = tokio::time::timeout(std::time::Duration::from_secs(30), calls.call(peer)).await??;
//!
//! // Take calls.
//! let mut events = calls.events();
//! while let Some(event) = events.next().await {
//!     if let CallEvent::Ringing(peer) = event {
//!         let call = calls.answer(peer).await?;
//!         # drop(call);
//!     }
//! }
//! # drop(call);
//! # Ok(())
//! # }
//! ```
//!
//! # Cancellation safety
//!
//! [`Calls::call`] and [`Calls::answer`] are cancellation safe: dropping the
//! future hangs up. [`Call::ended`] and [`Calls::closed`] lose nothing when
//! dropped.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
    task::{Context, Poll},
};

use iroh::EndpointId;
use iroh_live_node::{Audience, Node, Publication, Reach, Session, Subscription};
use moq_net::{Path, announce::Event, broadcast};
use n0_error::{e, stack_error};
use n0_future::{Stream, StreamExt, stream, task::AbortOnDropHandle};
use n0_watcher::{Watchable, Watcher};
use tokio::{select, sync::broadcast as events_channel};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::layout::LiveLayout;

/// The broadcast name of a call side, as `irl call` and the Android demo use it.
///
/// Each node publishes its side at `<root>/<its id>/call`.
pub const CALL: &str = "call";

/// How many events a slow reader may fall behind before it gets [`CallEvent::Lagged`].
const EVENT_QUEUE: usize = 64;

/// Why a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HungUp {
    /// The peer withdrew its side of the call.
    Withdrawn,
    /// The session with the peer ended.
    Lost,
}

/// A change in who is calling, from [`Calls::events`].
///
/// Best-effort: the route table's announcement listing can drop a ring that
/// is withdrawn before it is read. [`Calls::ringing`] is the source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    /// The peer rings this node.
    Ringing(EndpointId),
    /// The peer ended the call that was in progress.
    HungUp(EndpointId, HungUp),
    /// The reader fell behind and missed events. Read [`Calls::ringing`] again.
    Lagged,
}

/// The stream of [`CallEvent`]s, from [`Calls::events`].
///
/// It first replays the peers that ring now.
pub struct CallEvents(Pin<Box<dyn Stream<Item = CallEvent> + Send>>);

impl fmt::Debug for CallEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CallEvents")
    }
}

impl Stream for CallEvents {
    type Item = CallEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<CallEvent>> {
        self.0.as_mut().poll_next(cx)
    }
}

/// An error from [`Calls`].
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// A grant does not let a peer publish its call side.
    ///
    /// Either the node's grant refuses this node, or the session's grant
    /// refuses the peer.
    #[error("the grant does not let {peer} publish {path}")]
    NotGranted { path: Path, peer: EndpointId },
    /// The peer is not calling this node.
    #[error("{peer} is not ringing")]
    NotRinging { peer: EndpointId },
    /// A call with the peer is in progress or being placed.
    #[error("already in a call with {peer}")]
    AlreadyInCall { peer: EndpointId },
    /// The peer's session ended before it offered its side of the call.
    #[error("{peer} did not answer")]
    NotAnswered { peer: EndpointId },
    /// The node failed.
    #[error(transparent)]
    Node(iroh_live_node::Error),
}

/// What the call service keeps under one lock.
#[derive(Debug, Default)]
struct CallState {
    /// Peers whose side is in the route table.
    visible: BTreeSet<EndpointId>,
    /// Calls being placed.
    pending: BTreeSet<EndpointId>,
    /// Calls in progress.
    active: BTreeSet<EndpointId>,
    /// Peers that were hung up on, until they withdraw their side.
    ended: BTreeSet<EndpointId>,
    /// Dials started by [`Calls::ring`].
    dialing: BTreeMap<EndpointId, AbortOnDropHandle<()>>,
}

impl CallState {
    /// Returns the peers that ring: visible, in no call, and not hung up on.
    fn ringing(&self) -> BTreeSet<EndpointId> {
        self.visible
            .iter()
            .filter(|&peer| {
                !self.pending.contains(peer)
                    && !self.active.contains(peer)
                    && !self.ended.contains(peer)
            })
            .copied()
            .collect()
    }
}

/// The shared state behind every clone of [`Calls`] and every [`Call`].
struct Inner {
    node: Node,
    layout: LiveLayout,
    /// Who this node's side is offered to.
    audience: Watchable<BTreeSet<EndpointId>>,
    /// Carries [`CallEvent`]s to every [`CallEvents`] stream.
    events: events_channel::Sender<CallEvent>,
    /// The peers that ring, as [`CallState::ringing`] derives them.
    ringing: Watchable<BTreeSet<EndpointId>>,
    /// Cancelled once the service ends.
    closed: CancellationToken,
    state: Mutex<CallState>,
    /// Keeps the announce watcher alive for as long as the service is.
    task: OnceLock<AbortOnDropHandle<()>>,
}

impl Inner {
    /// Updates the ringing set from `state`, and reports the peers that began to ring.
    fn recompute(&self, state: &CallState) {
        let now = state.ringing();
        let before = self.ringing.get();
        if now == before {
            return;
        }
        for peer in now.difference(&before) {
            debug!(remote = %peer.fmt_short(), "ringing");
            self.events.send(CallEvent::Ringing(*peer)).ok();
        }
        self.ringing.set(now).ok();
    }

    /// Locks the call state.
    fn lock(&self) -> MutexGuard<'_, CallState> {
        self.state.lock().expect("poisoned")
    }

    /// Takes `peer` out of the audience and the call bookkeeping, as a hang-up does.
    ///
    /// Takes `state` so the audience changes under the state lock.
    fn withdraw(&self, peer: EndpointId, state: &mut CallState) {
        let mut audience = self.audience.get();
        if audience.remove(&peer) {
            self.audience.set(audience).ok();
        }
        state.pending.remove(&peer);
        state.dialing.remove(&peer);
    }
}

/// The call service of a node.
///
/// Cheap to clone. See the [module](self) for the protocol.
#[derive(Clone, derive_more::Debug)]
#[debug("Calls({})", inner.node.id().fmt_short())]
pub struct Calls {
    inner: Arc<Inner>,
}

impl Calls {
    /// Publishes this node's call side at `<root>/<its id>/call` and starts the service.
    ///
    /// Returns the service with the side, which is visible to nobody until a
    /// peer is rung. One side serves every call in turn, so a camera and a
    /// preview keep running between calls. `B` is what fills it, built from
    /// the route table's `broadcast::Producer`, before the path is announced.
    ///
    /// For a `LocalBroadcast`, dropping the side ends every call and
    /// [`closed`](Self::closed) resolves. A bare producer has to be closed by
    /// the application.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotGranted`] if the node's grant does not let this
    /// node publish its call path (the check assumes the grant treats all
    /// ids alike), and with [`Error::Node`] if the side is already published
    /// or `B` cannot be built.
    pub fn new<B>(node: &Node, layout: &LiveLayout) -> Result<(Self, B), Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        let local_id = node.id();
        let path = layout.path(local_id, CALL)?;
        if !node.grant_for(local_id).allows_publish(path.as_str()) {
            return Err(e!(Error::NotGranted {
                path,
                peer: local_id
            }));
        }
        let audience = Watchable::new(BTreeSet::new());
        let (publication, side) =
            node.publish_with(path, Audience::Peers(audience.clone()), |producer| {
                B::try_from(producer.clone())
            })?;
        let (events, _) = events_channel::channel(EVENT_QUEUE);
        let inner = Arc::new(Inner {
            node: node.clone(),
            layout: layout.clone(),
            audience,
            events,
            ringing: Watchable::new(BTreeSet::new()),
            closed: CancellationToken::new(),
            state: Mutex::new(CallState::default()),
            task: OnceLock::new(),
        });
        let task = tokio::spawn(watch_rings(Arc::downgrade(&inner), publication));
        inner.task.set(AbortOnDropHandle::new(task)).ok();
        info!(id = %local_id.fmt_short(), "calls started");
        Ok((Self { inner }, side))
    }

    /// Rings `peer`: offers it this node's side, and dials it if no session exists.
    ///
    /// Non-blocking and idempotent. The peer sees the side appear in its route
    /// table. [`call`](Self::call) and [`answer`](Self::answer) ring first.
    pub fn ring(&self, peer: EndpointId) {
        // The audience changes under the state lock, as in `withdraw`, so a
        // ring and a hang-up at once cannot lose either update.
        let mut state = self.inner.lock();
        let mut audience = self.inner.audience.get();
        if audience.insert(peer) {
            self.inner.audience.set(audience).ok();
        }
        let connected = self
            .inner
            .node
            .sessions()
            .get()
            .iter()
            .any(|session| session.remote_id() == peer);
        if connected || state.dialing.contains_key(&peer) {
            return;
        }
        let node = self.inner.node.clone();
        let inner = Arc::downgrade(&self.inner);
        let dial = tokio::spawn(async move {
            if let Err(err) = node.connect(peer).await {
                warn!(remote = %peer.fmt_short(), %err, "could not dial the peer to ring it");
            }
            if let Some(inner) = inner.upgrade() {
                inner.lock().dialing.remove(&peer);
            }
        });
        state.dialing.insert(peer, AbortOnDropHandle::new(dial));
    }

    /// Calls `peer`: rings it, waits until it answers, and subscribes to its side.
    ///
    /// The peer answers by ringing back, so this waits as long as the peer
    /// takes. Bound it with a timeout. Dropping the future hangs up.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::AlreadyInCall`] if a call with `peer` exists,
    /// [`Error::NotGranted`] if the session's grant does not let the peer
    /// publish its side, [`Error::NotAnswered`] if the session ends first,
    /// and [`Error::Node`] otherwise.
    pub async fn call(&self, peer: EndpointId) -> Result<Call, Error> {
        let pending = self.begin(peer, false)?;
        self.ring(peer);
        self.subscribe(pending, peer).await
    }

    /// Answers the call of `peer`: rings it back, and subscribes to its side.
    ///
    /// Dropping the future hangs up.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotRinging`] if `peer` is not calling, and as
    /// [`call`](Self::call) otherwise.
    pub async fn answer(&self, peer: EndpointId) -> Result<Call, Error> {
        let pending = self.begin(peer, true)?;
        self.ring(peer);
        self.subscribe(pending, peer).await
    }

    /// Returns the peers that ring this node now, as they change.
    ///
    /// The source of truth: peers whose side is visible, minus those with a
    /// call in progress or being placed, and those that were hung up on and
    /// have not withdrawn their side since.
    #[must_use]
    pub fn ringing(&self) -> n0_watcher::Direct<BTreeSet<EndpointId>> {
        self.inner.ringing.watch()
    }

    /// Returns a stream of calls and hang-ups.
    ///
    /// It first replays the peers that ring now. See [`CallEvent`] for what
    /// it does not guarantee.
    #[must_use]
    pub fn events(&self) -> CallEvents {
        let live = BroadcastStream::new(self.inner.events.subscribe()).map(|event| match event {
            Ok(event) => event,
            Err(BroadcastStreamRecvError::Lagged(_)) => CallEvent::Lagged,
        });
        let replay = self
            .inner
            .ringing
            .get()
            .into_iter()
            .map(CallEvent::Ringing)
            .collect::<Vec<_>>();
        CallEvents(Box::pin(stream::iter(replay).chain(live)))
    }

    /// Waits until the service ends: the node shut down, or the call side was withdrawn.
    ///
    /// Cancellation safe.
    pub async fn closed(&self) {
        self.inner.closed.cancelled().await;
    }

    /// Marks a call with `peer` as being placed.
    fn begin(&self, peer: EndpointId, must_ring: bool) -> Result<Pending, Error> {
        let mut state = self.inner.lock();
        if state.active.contains(&peer) || state.pending.contains(&peer) {
            return Err(e!(Error::AlreadyInCall { peer }));
        }
        if must_ring && !state.ringing().contains(&peer) {
            return Err(e!(Error::NotRinging { peer }));
        }
        state.pending.insert(peer);
        self.inner.recompute(&state);
        Ok(Pending {
            inner: self.inner.clone(),
            peer,
            connected: false,
        })
    }

    /// Subscribes to the side of `peer`, which it offers once it answers.
    async fn subscribe(&self, mut pending: Pending, peer: EndpointId) -> Result<Call, Error> {
        let path = self.inner.layout.path(peer, CALL)?;
        let subscription = self
            .inner
            .node
            .subscribe(path, Reach::Direct(peer))
            .await
            .map_err(|err| match err {
                iroh_live_node::Error::NotGranted { path, peer, .. } => {
                    e!(Error::NotGranted { path, peer })
                }
                iroh_live_node::Error::NotAnnounced { .. } => e!(Error::NotAnswered { peer }),
                err => Error::from(err),
            })?;
        let mut state = self.inner.lock();
        state.pending.remove(&peer);
        state.active.insert(peer);
        self.inner.recompute(&state);
        pending.connected = true;
        info!(remote = %peer.fmt_short(), "call connected");
        Ok(Call {
            inner: self.inner.clone(),
            peer,
            session: subscription.session(),
            subscription,
        })
    }
}

/// A call being placed.
///
/// Dropping it before the call connects hangs up.
struct Pending {
    inner: Arc<Inner>,
    peer: EndpointId,
    /// Set once the call connects, so the drop does not hang up.
    connected: bool,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.connected {
            return;
        }
        let mut state = self.inner.lock();
        self.inner.withdraw(self.peer, &mut state);
        self.inner.recompute(&state);
        debug!(remote = %self.peer.fmt_short(), "call attempt dropped");
    }
}

/// A call in progress.
///
/// Dropping it, or [`hang_up`](Self::hang_up), ends the call: the peer loses
/// this node's side, and rings again only once it withdraws its own side and
/// offers it anew.
#[derive(derive_more::Debug)]
#[must_use = "dropping a `Call` hangs up"]
#[debug("Call({})", peer.fmt_short())]
pub struct Call {
    inner: Arc<Inner>,
    peer: EndpointId,
    subscription: Subscription,
    session: Option<Session>,
}

impl Call {
    /// Returns the peer on the other side.
    #[must_use]
    pub fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Returns the subscription to the peer's side.
    #[must_use]
    pub fn subscription(&self) -> &Subscription {
        &self.subscription
    }

    /// Ends the call.
    pub fn hang_up(self) {}

    /// Waits until the call ends, and returns why.
    ///
    /// Cancellation safe.
    pub async fn ended(&self) -> HungUp {
        let broadcast = self.subscription.as_moq();
        let Some(session) = &self.session else {
            broadcast.closed().await;
            return HungUp::Withdrawn;
        };
        select! {
            _ = session.closed() => HungUp::Lost,
            _ = broadcast.closed() => {
                if session.connection().close_reason().is_some() {
                    HungUp::Lost
                } else {
                    HungUp::Withdrawn
                }
            }
        }
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        let mut state = self.inner.lock();
        self.inner.withdraw(self.peer, &mut state);
        state.active.remove(&self.peer);
        if state.visible.contains(&self.peer) {
            state.ended.insert(self.peer);
        }
        self.inner.recompute(&state);
        info!(remote = %self.peer.fmt_short(), "call ended");
    }
}

/// Follows the route table for rings, until the node or the call side ends.
///
/// Holds the service weakly, so dropping every handle stops it.
async fn watch_rings(inner: Weak<Inner>, publication: Publication) {
    let Some(strong) = inner.upgrade() else {
        return;
    };
    let node = strong.node.clone();
    let layout = strong.layout.clone();
    drop(strong);
    let local_id = node.id();
    let mut announced = node.origin().announced();
    let node_closed = node.closed();
    tokio::pin!(node_closed);
    let withdrawn = publication.withdrawn();
    tokio::pin!(withdrawn);
    loop {
        let event = select! {
            event = announced.next() => match event {
                Some(event) => event,
                None => break,
            },
            () = &mut node_closed => break,
            () = &mut withdrawn => break,
        };
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let (announce, present) = match &event {
            Event::Absent(announce) => (announce, false),
            Event::Present(announce) => (announce, true),
        };
        let Some((peer, CALL)) = layout.parse(&announce.prefix) else {
            continue;
        };
        if peer == local_id {
            continue;
        }
        let mut state = inner.lock();
        if present {
            state.visible.insert(peer);
            state.ended.remove(&peer);
        } else {
            state.visible.remove(&peer);
            state.ended.remove(&peer);
            if state.active.contains(&peer) {
                inner
                    .events
                    .send(CallEvent::HungUp(peer, HungUp::Withdrawn))
                    .ok();
            }
        }
        inner.recompute(&state);
    }
    if let Some(inner) = inner.upgrade() {
        debug!(id = %local_id.fmt_short(), "calls closed");
        inner.closed.cancel();
    }
}
