//! Multi-origin subscription that selects the active source from a
//! candidate set.
//!
//! A [`Subscription`] holds one [`SourceSetHandle`] and opens a MoQ
//! session and a [`RemoteBroadcast`] for every source in the set.
//! The [`SelectionPolicy`] picks one of them as the active source;
//! the active subscription is exposed through
//! [`Subscription::active`] and via the [`SubscriptionEvent`] stream.
//!
//! Mutating the source set (via the handle) at runtime adds or
//! removes candidates and triggers re-selection. When the active
//! source disappears or another source becomes preferred, the
//! subscription swaps the active source.
//!
//! The default mode of operation is session-level swap: callers
//! observe [`SubscriptionEvent::ActiveChanged`] and re-attach their
//! decoder pipelines. For frame-level continuity, use
//! [`Subscription::media`] which returns
//! [`SeamlessMediaTracks`](crate::seamless::SeamlessMediaTracks)
//! that internally re-attach decode pipelines on swap without the
//! consumer rebuilding anything.

use std::{collections::HashMap, sync::Arc};

use iroh_moq::MoqSession;
use moq_media::subscribe::RemoteBroadcast;
use n0_error::{AnyError, Result, StdResultExt};
use n0_future::{FuturesUnordered, StreamExt, task::AbortOnDropHandle};
use n0_watcher::{Watchable, Watcher};
use tokio::{
    sync::{Mutex, broadcast, mpsc},
    task,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{
    Live,
    sources::{
        Candidate, PreferOrdered, SelectionPolicy, SourceId, SourceSet, SourceSetHandle,
        TransportSource,
    },
};

/// Capacity of the broadcast event channel. Subscribers that lag
/// further than this miss intermediate events and re-read the
/// current state via [`Subscription::active`] /
/// [`Subscription::active_id`]; missed events are reported as
/// [`broadcast::error::RecvError::Lagged`] on the receiver and a
/// `warn!` is logged from the actor.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Capacity of the internal session-closed signal channel.
/// Watchdog tasks send through this when their session ends; the
/// actor's main loop drains it inside `tokio::select!`.
const SESSION_CLOSED_CHANNEL_CAPACITY: usize = 32;

/// A subscription to a broadcast across a set of candidate transport
/// sources.
///
/// The subscription opens a session and a [`RemoteBroadcast`] for
/// each source in its [`SourceSetHandle`]. A [`SelectionPolicy`]
/// picks the active source; consumers read the active session and
/// broadcast through [`Subscription::active`] or wait for the first
/// active source via [`Subscription::wait_active`].
///
/// When the active source changes, a
/// [`SubscriptionEvent::ActiveChanged`] is emitted on the event
/// stream returned by [`Subscription::events`].
#[derive(Debug, Clone)]
pub struct Subscription {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    broadcast_name: String,
    sources: SourceSetHandle,
    state: Arc<Mutex<State>>,
    active_id: Watchable<Option<SourceId>>,
    events_tx: broadcast::Sender<SubscriptionEvent>,
    _task: AbortOnDropHandle<()>,
    shutdown: CancellationToken,
}

/// Snapshot of the active source's session and broadcast.
///
/// Held by reference so callers can read `session()` and
/// `broadcast()` without cloning. Returned by
/// [`Subscription::active`]. `Clone`d snapshots share the same
/// underlying [`AttachedSubscription`] state.
#[derive(Debug, Clone)]
pub struct ActiveSource {
    /// Identifier of the active source.
    pub id: SourceId,
    inner: Arc<AttachedSubscription>,
}

impl ActiveSource {
    /// Returns the underlying MoQ session for the active source.
    pub fn session(&self) -> &MoqSession {
        &self.inner.session
    }

    /// Returns the active [`RemoteBroadcast`].
    pub fn broadcast(&self) -> &RemoteBroadcast {
        &self.inner.broadcast
    }

    /// Returns the network signals receiver attached to the active
    /// session. Callers that drive adaptive rendition selection
    /// observe this watcher.
    pub fn signals(&self) -> &tokio::sync::watch::Receiver<moq_media::net::NetworkSignals> {
        &self.inner.signals
    }
}

/// Receiver returned by [`Subscription::events`].
///
/// Wraps a [`broadcast::Receiver`] so cloning the subscription
/// does not load-balance events across observers; each call to
/// [`Subscription::events`] returns an independent receiver that
/// observes the full stream from the moment it was created.
#[derive(Debug)]
pub struct SubscriptionEvents {
    rx: broadcast::Receiver<SubscriptionEvent>,
}

impl SubscriptionEvents {
    /// Awaits the next event.
    ///
    /// Returns `Ok(event)` on a fresh event,
    /// `Err(RecvError::Lagged(n))` when this consumer fell more
    /// than [`EVENT_CHANNEL_CAPACITY`] events behind, or
    /// `Err(RecvError::Closed)` once the subscription is shut
    /// down.
    pub async fn recv(&mut self) -> Result<SubscriptionEvent, broadcast::error::RecvError> {
        self.rx.recv().await
    }
}

/// Event emitted by [`Subscription::events`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SubscriptionEvent {
    /// A candidate source went live (its session connected and the
    /// broadcast was confirmed announced). Concrete state is
    /// available via [`Subscription::source_for`].
    Attached {
        /// Identifier of the activated source.
        id: SourceId,
    },
    /// A candidate source went away because its session closed or
    /// it was removed from the [`SourceSet`].
    Detached {
        /// Identifier of the source that went away.
        id: SourceId,
    },
    /// A candidate source failed to attach.
    AttachFailed {
        /// Identifier of the source that failed.
        id: SourceId,
        /// Human-readable error description.
        error: String,
    },
    /// The active source changed. `previous` is the source that was
    /// active before; `current` is the new active source. Either
    /// may be `None` for the start of the subscription or when no
    /// healthy source remains.
    ActiveChanged {
        /// Previously active source, if any.
        previous: Option<SourceId>,
        /// New active source, if any.
        current: Option<SourceId>,
    },
}

impl Subscription {
    /// Spawns a subscription with the default [`PreferOrdered`]
    /// selection policy.
    pub fn spawn(
        live: Live,
        broadcast_name: impl ToString,
        sources: impl Into<SourceSetHandle>,
    ) -> Self {
        Self::with_policy(live, broadcast_name, sources, PreferOrdered)
    }

    /// Spawns a subscription with a custom selection policy.
    pub fn with_policy<P: SelectionPolicy>(
        live: Live,
        broadcast_name: impl ToString,
        sources: impl Into<SourceSetHandle>,
        policy: P,
    ) -> Self {
        let broadcast_name = broadcast_name.to_string();
        let sources = sources.into();
        let active_id = Watchable::new(None);
        let shutdown = CancellationToken::new();
        let state: Arc<Mutex<State>> = Arc::new(Mutex::new(State::default()));
        let (events_tx, _events_rx_anchor) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let task = spawn_loop(
            live,
            broadcast_name.clone(),
            sources.clone(),
            Box::new(policy),
            state.clone(),
            active_id.clone(),
            events_tx.clone(),
            shutdown.clone(),
        );
        Self {
            inner: Arc::new(Inner {
                broadcast_name,
                sources,
                state,
                active_id,
                events_tx,
                _task: AbortOnDropHandle::new(task),
                shutdown,
            }),
        }
    }

    /// Returns the broadcast name.
    pub fn broadcast_name(&self) -> &str {
        &self.inner.broadcast_name
    }

    /// Returns a reference to the [`SourceSetHandle`] for runtime
    /// mutation of the candidate set.
    pub fn sources(&self) -> &SourceSetHandle {
        &self.inner.sources
    }

    /// Returns the identifier of the currently active source, if
    /// any.
    pub fn active_id(&self) -> Option<SourceId> {
        self.inner.active_id.get()
    }

    /// Returns the active source snapshot, if any.
    pub async fn active(&self) -> Option<ActiveSource> {
        let id = self.inner.active_id.get()?;
        let state = self.inner.state.lock().await;
        let attached = state.attached.get(&id)?.clone();
        Some(ActiveSource {
            id,
            inner: attached,
        })
    }

    /// Returns the snapshot for the source identified by `id`, or
    /// `None` when that source is not currently attached.
    pub async fn source_for(&self, id: &SourceId) -> Option<ActiveSource> {
        let state = self.inner.state.lock().await;
        let attached = state.attached.get(id)?.clone();
        Some(ActiveSource {
            id: id.clone(),
            inner: attached,
        })
    }

    /// Waits for the subscription to pick an active source and
    /// returns it.
    ///
    /// Returns `None` when the subscription is shut down or the
    /// source set never produces a healthy source before the
    /// caller drops the subscription.
    pub async fn wait_active(&self) -> Option<ActiveSource> {
        let mut watcher = self.inner.active_id.watch();
        loop {
            if let Some(active) = self.active().await {
                return Some(active);
            }
            tokio::select! {
                biased;
                _ = self.inner.shutdown.cancelled() => return None,
                res = watcher.updated() => {
                    if res.is_err() {
                        return None;
                    }
                }
            }
        }
    }

    /// Waits for the subscription to pick an active source and
    /// returns it, or an error when the subscription is shut down
    /// before any source becomes healthy.
    ///
    /// Convenience wrapper around [`Subscription::wait_active`].
    /// Most callers want to abort if the subscription cannot pick
    /// any source; this method bakes the error in.
    ///
    /// # Errors
    ///
    /// Returns an error when the subscription closed before any
    /// source became healthy. The error type is [`AnyError`]
    /// (n0_error's catch-all). Use [`Subscription::wait_active`]
    /// for the underlying `Option` shape.
    pub async fn ready(&self) -> Result<ActiveSource, AnyError> {
        self.wait_active()
            .await
            .std_context("subscription closed before any source became active")
    }

    /// Returns a watcher over the active source's id.
    pub fn watch_active(&self) -> n0_watcher::Direct<Option<SourceId>> {
        self.inner.active_id.watch()
    }

    /// Subscribes to the [`SubscriptionEvent`] stream.
    ///
    /// Each call returns an independent receiver; cloning the
    /// [`Subscription`] does not share a single receiver. The
    /// channel is bounded by [`EVENT_CHANNEL_CAPACITY`]; a slow
    /// consumer that lags further is delivered
    /// [`broadcast::error::RecvError::Lagged`] and should re-read
    /// the current state via [`Subscription::active`].
    pub fn events(&self) -> SubscriptionEvents {
        SubscriptionEvents {
            rx: self.inner.events_tx.subscribe(),
        }
    }

    /// Awaits the next [`SubscriptionEvent`].
    ///
    /// Convenience around [`Subscription::events`] for callers that
    /// only need one receiver. Returns `None` once the subscription
    /// is shut down. Lag events surface as a stream restart from
    /// the latest available event; callers tracking distinct event
    /// counts should use `events()` directly to observe lag.
    pub async fn next_event(&self) -> Option<SubscriptionEvent> {
        let mut rx = self.inner.events_tx.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "subscription event consumer lagged");
                    continue;
                }
            }
        }
    }

    /// Stops the subscription and closes every owned session.
    ///
    /// Equivalent to dropping all clones of the subscription.
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
    }
}

type AnyPolicy = Box<dyn SelectionPolicy>;

#[derive(Default, Debug)]
struct State {
    attached: HashMap<SourceId, Arc<AttachedSubscription>>,
}

/// Per-source state held by the subscription.
///
/// Wrapped in an `Arc` so [`ActiveSource`] can hand out a stable
/// reference even if the source is later removed.
#[derive(Debug)]
pub(crate) struct AttachedSubscription {
    pub(crate) session: MoqSession,
    pub(crate) broadcast: RemoteBroadcast,
    pub(crate) signals: tokio::sync::watch::Receiver<moq_media::net::NetworkSignals>,
    /// Aborted when the source is removed from the set.
    _watchdog: AbortOnDropHandle<()>,
}

type AttachFuture =
    std::pin::Pin<Box<dyn Future<Output = (SourceId, Result<AttachedRaw, AnyError>)> + Send>>;

#[allow(
    clippy::too_many_arguments,
    reason = "private spawn helper, grouping the args would add complexity for one call site"
)]
fn spawn_loop(
    live: Live,
    broadcast_name: String,
    sources: SourceSetHandle,
    policy: AnyPolicy,
    state: Arc<Mutex<State>>,
    active_id: Watchable<Option<SourceId>>,
    events_tx: broadcast::Sender<SubscriptionEvent>,
    shutdown: CancellationToken,
) -> task::JoinHandle<()> {
    let span = info_span!("Subscription", name = %broadcast_name);
    task::spawn(
        async move {
            let (closed_tx, mut closed_rx) =
                mpsc::channel::<SourceId>(SESSION_CLOSED_CHANNEL_CAPACITY);
            let mut watcher = sources.watch();
            // In-flight attach futures driven in parallel so an
            // unreachable peer cannot block attach of every other
            // source. The actor's main `select!` polls them
            // alongside the source-set watcher and the close-watchdog
            // channel.
            let mut attaching = FuturesUnordered::<AttachFuture>::new();
            let mut attaching_ids: std::collections::HashSet<SourceId> =
                std::collections::HashSet::new();

            debug!(
                source_count = sources.get().len(),
                "subscription actor started"
            );

            // Seed: one attach per source already in the set.
            let initial = sources.get();
            schedule_attaches(
                &live,
                &broadcast_name,
                &initial,
                &state,
                &mut attaching,
                &mut attaching_ids,
            )
            .await;
            pick_active(&initial, &policy, &state, &active_id, &events_tx).await;

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        debug!("subscription shutting down");
                        break;
                    }
                    res = watcher.updated() => {
                        if res.is_err() {
                            debug!("source set watcher closed");
                            break;
                        }
                        let desired = sources.get();
                        prune_removed_sources(&desired, &state, &active_id, &events_tx).await;
                        schedule_attaches(
                            &live,
                            &broadcast_name,
                            &desired,
                            &state,
                            &mut attaching,
                            &mut attaching_ids,
                        )
                        .await;
                        pick_active(&desired, &policy, &state, &active_id, &events_tx).await;
                    }
                    Some((id, result)) = attaching.next(), if !attaching.is_empty() => {
                        attaching_ids.remove(&id);
                        complete_attach(id, result, &state, &events_tx, &closed_tx).await;
                        let desired = sources.get();
                        pick_active(&desired, &policy, &state, &active_id, &events_tx).await;
                    }
                    Some(closed_id) = closed_rx.recv() => {
                        handle_session_closed(closed_id, &state, &active_id, &events_tx).await;
                        let desired = sources.get();
                        pick_active(&desired, &policy, &state, &active_id, &events_tx).await;
                    }
                }
            }
            active_id.set(None).ok();
            state.lock().await.attached.clear();
        }
        .instrument(span),
    )
}

/// Drops every entry in `state.attached` whose id is not in the
/// desired set. Emits `Detached` for each removed entry. If the
/// active source was removed, clears `active_id` immediately so
/// readers see a consistent state without waiting for the next
/// `pick_active` pass.
async fn prune_removed_sources(
    desired: &SourceSet,
    state: &Mutex<State>,
    active_id: &Watchable<Option<SourceId>>,
    events_tx: &broadcast::Sender<SubscriptionEvent>,
) {
    let desired_ids: std::collections::HashSet<SourceId> = desired.iter().map(|s| s.id()).collect();
    let removed: Vec<SourceId> = {
        let mut st = state.lock().await;
        let removed: Vec<SourceId> = st
            .attached
            .keys()
            .filter(|id| !desired_ids.contains(id))
            .cloned()
            .collect();
        for id in &removed {
            st.attached.remove(id);
        }
        removed
    };
    if let Some(current) = active_id.get()
        && removed.contains(&current)
    {
        active_id.set(None).ok();
        emit_event(
            events_tx,
            SubscriptionEvent::ActiveChanged {
                previous: Some(current),
                current: None,
            },
        );
    }
    for id in removed {
        emit_event(events_tx, SubscriptionEvent::Detached { id });
    }
}

/// Schedules an attach future for every source that is neither
/// already attached nor in flight. The futures complete onto the
/// actor's `tokio::select!` and the actor installs them into the
/// state map there.
async fn schedule_attaches(
    live: &Live,
    broadcast_name: &str,
    desired: &SourceSet,
    state: &Mutex<State>,
    attaching: &mut FuturesUnordered<AttachFuture>,
    attaching_ids: &mut std::collections::HashSet<SourceId>,
) {
    let attached_ids: std::collections::HashSet<SourceId> = {
        let st = state.lock().await;
        st.attached.keys().cloned().collect()
    };
    for source in desired.iter() {
        let id = source.id();
        if attached_ids.contains(&id) || attaching_ids.contains(&id) {
            continue;
        }
        attaching_ids.insert(id.clone());
        let live = live.clone();
        let broadcast_name = broadcast_name.to_string();
        let source = source.clone();
        let id_for_future = id.clone();
        attaching.push(Box::pin(async move {
            let res = attach_source(&live, &broadcast_name, source).await;
            (id_for_future, res)
        }));
    }
}

/// Installs (or reports the failure of) one in-flight attach. On
/// success, spawns the close watchdog before inserting into state
/// so a session that closed between connect and insert still
/// surfaces as `Detached` immediately.
async fn complete_attach(
    id: SourceId,
    result: Result<AttachedRaw, AnyError>,
    state: &Mutex<State>,
    events_tx: &broadcast::Sender<SubscriptionEvent>,
    closed_tx: &mpsc::Sender<SourceId>,
) {
    match result {
        Ok(attached) => {
            let watchdog = spawn_watchdog(attached.session.clone(), id.clone(), closed_tx.clone());
            let attached = Arc::new(AttachedSubscription {
                session: attached.session,
                broadcast: attached.broadcast,
                signals: attached.signals,
                _watchdog: AbortOnDropHandle::new(watchdog),
            });
            info!(source = %id, "attached source");
            state.lock().await.attached.insert(id.clone(), attached);
            emit_event(events_tx, SubscriptionEvent::Attached { id });
        }
        Err(err) => {
            warn!(source = %id, "attach failed: {err:#}");
            emit_event(
                events_tx,
                SubscriptionEvent::AttachFailed {
                    id,
                    error: format!("{err:#}"),
                },
            );
        }
    }
}

/// Removes the closed source from state and clears `active_id` if
/// the active source was the one that closed. Emits `Detached`.
async fn handle_session_closed(
    id: SourceId,
    state: &Mutex<State>,
    active_id: &Watchable<Option<SourceId>>,
    events_tx: &broadcast::Sender<SubscriptionEvent>,
) {
    debug!(source = %id, "session closed");
    state.lock().await.attached.remove(&id);
    let was_active = active_id.get();
    if was_active.as_ref() == Some(&id) {
        active_id.set(None).ok();
        emit_event(
            events_tx,
            SubscriptionEvent::ActiveChanged {
                previous: was_active,
                current: None,
            },
        );
    }
    emit_event(events_tx, SubscriptionEvent::Detached { id });
}

/// Non-blocking event emit on the broadcast channel. A `Send`
/// failure means there are no live receivers, which is fine; it
/// just means no one is listening yet. Lag handling lives on the
/// receiver side.
fn emit_event(events_tx: &broadcast::Sender<SubscriptionEvent>, event: SubscriptionEvent) {
    let _ = events_tx.send(event);
}

struct AttachedRaw {
    session: MoqSession,
    broadcast: RemoteBroadcast,
    signals: tokio::sync::watch::Receiver<moq_media::net::NetworkSignals>,
}

async fn attach_source(
    live: &Live,
    broadcast_name: &str,
    source: TransportSource,
) -> Result<AttachedRaw, AnyError> {
    match source {
        TransportSource::Direct(direct) => {
            let mut session = live
                .transport()
                .connect(direct.peer)
                .await
                .std_context("connect direct source")?;
            let consumer = session.subscribe(broadcast_name).await?;
            let broadcast = RemoteBroadcast::new(broadcast_name, consumer).await?;
            crate::util::spawn_stats_recorder(
                session.conn(),
                broadcast.stats().net.clone(),
                broadcast.shutdown_token(),
            );
            let signals =
                crate::util::spawn_signal_producer(session.conn(), broadcast.shutdown_token());
            Ok(AttachedRaw {
                session,
                broadcast,
                signals,
            })
        }
        TransportSource::Relay(target) => {
            let mut session = live
                .connect_relay(&target)
                .await
                .std_context("connect relay source")?;
            let consumer = session.subscribe(broadcast_name).await?;
            let broadcast = RemoteBroadcast::new(broadcast_name, consumer).await?;
            crate::util::spawn_stats_recorder(
                session.conn(),
                broadcast.stats().net.clone(),
                broadcast.shutdown_token(),
            );
            let signals =
                crate::util::spawn_signal_producer(session.conn(), broadcast.shutdown_token());
            Ok(AttachedRaw {
                session,
                broadcast,
                signals,
            })
        }
    }
}

async fn pick_active(
    desired: &SourceSet,
    policy: &AnyPolicy,
    state: &Mutex<State>,
    active_id: &Watchable<Option<SourceId>>,
    events_tx: &broadcast::Sender<SubscriptionEvent>,
) {
    let st = state.lock().await;
    let candidates: Vec<Candidate<'_>> = desired
        .iter()
        .map(|s| Candidate {
            source: s,
            healthy: st.attached.contains_key(&s.id()),
        })
        .collect();
    let current = active_id.get();
    let next = policy.pick(&candidates, current.as_ref());
    drop(st);

    if current != next {
        if let Some(ref n) = next {
            info!(source = %n, "active source changed");
        } else {
            info!("no healthy source");
        }
        active_id.set(next.clone()).ok();
        emit_event(
            events_tx,
            SubscriptionEvent::ActiveChanged {
                previous: current,
                current: next,
            },
        );
    }
}

fn spawn_watchdog(
    session: MoqSession,
    id: SourceId,
    tx: mpsc::Sender<SourceId>,
) -> task::JoinHandle<()> {
    task::spawn(async move {
        let _ = session.closed().await;
        let _ = tx.send(id).await;
    })
}
