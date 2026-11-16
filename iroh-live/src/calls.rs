//! One-to-one calls over a layout's call path.
//!
//! [`Calls`] publishes this node's side of its calls at `<root>/<its id>/call`
//! and offers it to nobody until [`ring`](Calls::ring) adds a peer to its
//! audience. A peer learns it is called when that path appears in its route
//! table, and answers by ringing back and subscribing to the caller's side.
//! Two peers that ring each other at once resolve into one [`Call`] on each
//! side.
//!
//! A peer that was hung up on rings again only once it withdraws its side and
//! offers it anew. Publishing the side again without a withdrawal in between
//! is no new offer, since the route table lists the path throughout.
//!
//! ```no_run
//! use iroh_live::{Node, calls::Calls, layout::LiveLayout};
//! use moq_net::broadcast;
//! use n0_watcher::Watcher;
//!
//! # async fn run(node: Node, peer: iroh::EndpointId) -> Result<(), Box<dyn std::error::Error>> {
//! let (calls, _side): (Calls, broadcast::Producer) = Calls::new(&node, &LiveLayout::default())?;
//!
//! // Place a call: ring, wait for the peer to answer, subscribe.
//! let call = tokio::time::timeout(std::time::Duration::from_secs(30), calls.call(peer)).await??;
//!
//! // Take the first call that rings.
//! let mut ringing = calls.ringing();
//! let peer = loop {
//!     if let Some(peer) = ringing.get().first().copied() {
//!         break peer;
//!     }
//!     ringing.updated().await?;
//! };
//! let answered = calls.answer(peer).await?;
//! // Ends with the reason once the peer hangs up.
//! let reason = answered.ended().await;
//! # drop(reason);
//! # drop(call);
//! # Ok(())
//! # }
//! ```
//!
//! [`Calls::call`] and [`Calls::answer`] are cancellation safe: dropping the
//! future hangs up. [`Call::ended`] and [`Calls::closed`] lose nothing when
//! dropped.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
};

use iroh::EndpointId;
use iroh_live_node::{Audience, Node, Publication, Reach, Session, Subscription};
use moq_net::{announce::Event, broadcast};
use n0_error::{e, stack_error};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher};
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::layout::LiveLayout;

/// The broadcast name of a call side, as `irl call` and the Android demo use it.
pub const CALL: &str = "call";

/// Why a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HangUpReason {
    /// The peer withdrew its side of the call.
    Withdrawn,
    /// The session with the peer ended.
    Lost,
}

/// An error from [`Calls`].
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
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
    /// Returns the peers that ring, as [`Calls::ringing`] describes.
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
    /// The peers that ring, as [`CallState::ringing`] derives them.
    ringing: Watchable<BTreeSet<EndpointId>>,
    /// Cancelled once the service ends.
    closed: CancellationToken,
    state: Mutex<CallState>,
    /// Keeps the announce watcher alive for as long as the service is.
    task: OnceLock<AbortOnDropHandle<()>>,
}

impl Inner {
    /// Updates the ringing set from `state`.
    fn update_ringing(&self, state: &CallState) {
        let now = state.ringing();
        let before = self.ringing.get();
        if now == before {
            return;
        }
        for peer in now.difference(&before) {
            debug!(peer = %peer.fmt_short(), "ringing");
        }
        self.ringing.set(now).ok();
    }

    /// Returns the open session with `peer`, if there is one.
    fn session_with(&self, peer: EndpointId) -> Option<Session> {
        self.node
            .sessions()
            .get()
            .into_iter()
            .find(|session| session.peer() == peer)
            .filter(|session| session.connection().close_reason().is_none())
    }

    /// Returns why the call with `peer` ended, once its side is gone.
    fn hang_up_reason(&self, peer: EndpointId) -> HangUpReason {
        match self.session_with(peer) {
            Some(_) => HangUpReason::Withdrawn,
            None => HangUpReason::Lost,
        }
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
///
/// # Examples
///
/// ```no_run
/// # async fn run(node: iroh_live::Node, peer: iroh::EndpointId)
/// # -> Result<(), Box<dyn std::error::Error>> {
/// use iroh_live::{calls::Calls, layout::LiveLayout};
/// use moq_net::broadcast;
///
/// let (calls, side): (Calls, broadcast::Producer) = Calls::new(&node, &LiveLayout::default())?;
/// let call = calls.call(peer).await?;
/// // Fill `side` with tracks and read the peer's side from the call.
/// # drop((call, side));
/// # Ok(())
/// # }
/// ```
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
    /// preview keep running between calls. `B` is built from the route
    /// table's `broadcast::Producer` before the path is announced: a
    /// `LocalBroadcast`, or the producer itself.
    ///
    /// For a `LocalBroadcast`, dropping the side ends every call and
    /// [`closed`](Self::closed) resolves. A bare producer has to be closed by
    /// the application.
    ///
    /// Each node's policy has to let the other publish at
    /// `<root>/<peer id>/call`, as [`LiveLayout::policy`] does. Otherwise
    /// [`call`](Self::call) and [`answer`](Self::answer) fail with
    /// [`NotGranted`](iroh_live_node::Error::NotGranted).
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Node`] if the side is already published or `B`
    /// cannot be built.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn new<B>(node: &Node, layout: &LiveLayout) -> Result<(Self, B), Error>
    where
        B: TryFrom<broadcast::Producer, Error: std::error::Error + Send + Sync + 'static>,
    {
        let local_id = node.id();
        let path = layout.path(local_id, CALL)?;
        let audience = Watchable::new(BTreeSet::new());
        let (publication, side) =
            node.publish_with(path, Audience::Peers(audience.clone()), |producer| {
                B::try_from(producer.clone())
            })?;
        let inner = Arc::new(Inner {
            node: node.clone(),
            layout: layout.clone(),
            audience,
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
    /// table.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn ring(&self, peer: EndpointId) {
        // The audience changes under the state lock, as in `withdraw`, so a
        // ring and a hang-up at once cannot lose either update.
        let mut state = self.inner.lock();
        let mut audience = self.inner.audience.get();
        if audience.insert(peer) {
            self.inner.audience.set(audience).ok();
        }
        // A session that is closing does not count, or a ring right after a
        // hang-up would skip the redial.
        let connected = self.inner.session_with(peer).is_some();
        if connected || state.dialing.contains_key(&peer) {
            return;
        }
        let node = self.inner.node.clone();
        let inner = Arc::downgrade(&self.inner);
        let dial = tokio::spawn(async move {
            if let Err(err) = node.connect(peer).await {
                warn!(peer = %peer.fmt_short(), error = %err, "could not dial the peer to ring it");
            }
            if let Some(inner) = inner.upgrade() {
                inner.lock().dialing.remove(&peer);
            }
        });
        state.dialing.insert(peer, AbortOnDropHandle::new(dial));
    }

    /// Calls `peer`: rings it, waits until it answers, and subscribes to its side.
    ///
    /// Waits as long as the peer takes to answer, so bound it with a timeout.
    /// Dropping the future hangs up.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::AlreadyInCall`] if a call with `peer` exists,
    /// [`Error::NotAnswered`] if the session ends first, and [`Error::Node`]
    /// otherwise, holding [`NotGranted`](iroh_live_node::Error::NotGranted)
    /// if the session's grant does not let the peer publish its side.
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
    /// These are the peers whose side is visible, in no call, and not hung up
    /// on since they last offered their side. A peer leaves the set when it
    /// withdraws its side before an answer, and when this node answers or
    /// calls it. [`Call::ended`] reports how a call in progress ends.
    #[must_use]
    pub fn ringing(&self) -> n0_watcher::Direct<BTreeSet<EndpointId>> {
        self.inner.ringing.watch()
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
        self.inner.update_ringing(&state);
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
                iroh_live_node::Error::NotAnnounced { .. } => e!(Error::NotAnswered { peer }),
                err => Error::from(err),
            })?;
        let mut state = self.inner.lock();
        state.pending.remove(&peer);
        state.active.insert(peer);
        self.inner.update_ringing(&state);
        pending.connected = true;
        info!(peer = %peer.fmt_short(), "call connected");
        Ok(Call {
            inner: self.inner.clone(),
            peer,
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
        self.inner.update_ringing(&state);
        debug!(peer = %self.peer.fmt_short(), "call attempt dropped");
    }
}

/// A call in progress.
///
/// Dropping it, or [`hang_up`](Self::hang_up), ends the call. See the
/// [module](self) for when the peer can ring again.
#[derive(derive_more::Debug)]
#[must_use = "dropping a `Call` hangs up"]
#[debug("Call({})", peer.fmt_short())]
pub struct Call {
    inner: Arc<Inner>,
    peer: EndpointId,
    subscription: Subscription,
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
    pub async fn ended(&self) -> HangUpReason {
        let broadcast = self.subscription.broadcast();
        let mut sessions = self.inner.node.sessions();
        // The session is looked up on every pass rather than kept from the
        // connect, since the node may list it only after the subscribe returns.
        loop {
            match self.inner.session_with(self.peer) {
                Some(session) => {
                    return select! {
                        _ = session.closed() => HangUpReason::Lost,
                        _ = broadcast.closed() => self.inner.hang_up_reason(self.peer),
                    };
                }
                None => select! {
                    _ = broadcast.closed() => return HangUpReason::Lost,
                    updated = sessions.updated() => if updated.is_err() {
                        return HangUpReason::Lost;
                    },
                },
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
        self.inner.update_ringing(&state);
        info!(peer = %self.peer.fmt_short(), "call ended");
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
            Event::Absent(announce) | Event::Offline(announce) => (announce, false),
            Event::Present(announce)
            | Event::Unavailable(announce)
            | Event::Available(announce) => (announce, true),
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
        }
        inner.update_ringing(&state);
    }
    if let Some(inner) = inner.upgrade() {
        debug!(id = %local_id.fmt_short(), "calls closed");
        inner.closed.cancel();
    }
}
