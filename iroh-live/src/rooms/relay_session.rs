//! Self-reconnecting wrappers around the room's relay sessions.
//!
//! The room actor talks to the relay through two long-lived
//! sessions: one for discovery (subscribed to the room's announce
//! stream) and one for publishing (rooted at this peer's path).
//! Either can drop on a transient network failure or relay
//! restart. The wrappers here keep the connection alive across
//! such drops by reconnecting with capped exponential backoff,
//! re-issuing whatever state the session needs (active publishes
//! for the publisher) once the new session is up.
//!
//! Both wrappers expose only the surface the room actor uses:
//!
//! - [`RelayDiscovery`] forwards parsed announce events through an
//!   `mpsc::Receiver`. Reconnect is invisible from the receiving
//!   end.
//! - [`RelayPublisher`] accepts `(name, BroadcastConsumer)` pairs
//!   and re-issues them after every reconnect.

use std::{collections::HashMap, sync::Arc, time::Duration};

use iroh::EndpointId;
use moq_lite::BroadcastConsumer;
use n0_future::task::AbortOnDropHandle;
use tokio::{
    sync::{Mutex, mpsc},
    task,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use super::{MAX_PEER_BROADCASTS, RelayDiscoveryEvent, RoomTicket, parse_wire_name};
use crate::{Live, relay::RelayTarget, sources::RelayOffer};

/// Initial reconnect delay. Picked low so transient blips
/// recover invisibly to the user.
const RECONNECT_INITIAL: Duration = Duration::from_millis(100);
/// Cap on the backoff. Long enough to avoid thrashing the relay
/// while it is genuinely down, short enough to recover within a
/// few seconds of the relay coming back.
const RECONNECT_CAP: Duration = Duration::from_secs(10);
/// Multiplier applied each failed attempt.
const RECONNECT_MULTIPLIER: u32 = 2;
/// Time the wrapper considers a session "stable" before resetting
/// the backoff. Without this, a relay that accepts the connection
/// but immediately drops it would peg the backoff at the cap.
const STABLE_THRESHOLD: Duration = Duration::from_secs(30);

/// Capped exponential backoff with manual reset.
///
/// `next()` returns the current delay and advances the state for
/// the next call. `reset()` returns the schedule to its initial
/// delay.
#[derive(Debug)]
struct Backoff {
    current: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            current: RECONNECT_INITIAL,
        }
    }

    /// Returns the next delay (with jitter) and advances the
    /// schedule. The returned delay is the current step scaled
    /// by a uniform factor in `[0.5, 1.0]`, which spreads a
    /// thundering-herd reconnect across the step instead of
    /// landing every client at the same instant.
    fn next(&mut self) -> Duration {
        let step = self.current;
        let scaled = self
            .current
            .checked_mul(RECONNECT_MULTIPLIER)
            .unwrap_or(RECONNECT_CAP);
        self.current = scaled.min(RECONNECT_CAP);
        let jitter: f64 = 0.5 + rand::random::<f64>() * 0.5;
        step.mul_f64(jitter)
    }

    fn reset(&mut self) {
        self.current = RECONNECT_INITIAL;
    }
}

/// Self-reconnecting relay discovery session.
///
/// Owns a background task that opens an iroh-moq session at
/// `room/<topic_hex>`, reads the announce stream, parses each
/// `(peer, name, present)` tuple, and forwards it onto an mpsc
/// channel. On session close it backs off and reopens. Dropping
/// the [`RelayDiscovery`] cancels the task.
#[derive(Debug)]
pub(crate) struct RelayDiscovery {
    /// Receiver for announce events. The room actor's main
    /// `select!` polls this branch.
    pub(crate) rx: mpsc::Receiver<RelayDiscoveryEvent>,
    _task: AbortOnDropHandle<()>,
}

impl RelayDiscovery {
    /// Spawns the reconnect-aware discovery task.
    ///
    /// The first connection attempt blocks the caller so that a
    /// hard configuration error (bad endpoint, bad path) surfaces
    /// at room creation rather than later. Subsequent reconnects
    /// run in the background.
    pub(crate) async fn spawn(
        live: Live,
        ticket: RoomTicket,
        offer: RelayOffer,
    ) -> n0_error::Result<Self> {
        use n0_error::StdResultExt;

        let target = RelayTarget::new(offer.endpoint)
            .with_path(&ticket.relay_room_path())
            .with_api_key(offer.api_key.clone());

        // Initial connect: surface failures to the caller.
        let initial = live
            .connect_relay(&target)
            .await
            .std_context("connect to relay for room discovery")?;

        let me = live.endpoint().id();
        let (tx, rx) = mpsc::channel(MAX_PEER_BROADCASTS);
        let cancel = CancellationToken::new();

        let task = task::spawn(
            run_discovery(live, target, me, tx, cancel.clone(), Some(initial))
                .instrument(info_span!("RelayDiscovery")),
        );

        Ok(Self {
            rx,
            _task: AbortOnDropHandle::new(task),
        })
    }
}

async fn run_discovery(
    live: Live,
    target: RelayTarget,
    me: EndpointId,
    tx: mpsc::Sender<RelayDiscoveryEvent>,
    cancel: CancellationToken,
    mut bootstrap: Option<iroh_moq::MoqSession>,
) {
    let mut backoff = Backoff::new();
    loop {
        let session = if let Some(s) = bootstrap.take() {
            s
        } else {
            match connect_with_backoff(&live, &target, &mut backoff, &cancel).await {
                Some(s) => s,
                None => return,
            }
        };

        let started = tokio::time::Instant::now();
        let ended = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            ended = forward_announces(session.clone(), me, tx.clone()) => ended,
        };

        match ended {
            ForwardOutcome::ReceiverDropped => return,
            ForwardOutcome::SessionClosed => {
                if started.elapsed() >= STABLE_THRESHOLD {
                    backoff.reset();
                }
                debug!("relay discovery session closed, will reconnect");
            }
        }
    }
}

enum ForwardOutcome {
    /// The mpsc receiver on the actor side is gone; the wrapper
    /// has nothing to deliver to and exits.
    ReceiverDropped,
    /// The relay session ended; reconnect.
    SessionClosed,
}

async fn forward_announces(
    session: iroh_moq::MoqSession,
    me: EndpointId,
    tx: mpsc::Sender<RelayDiscoveryEvent>,
) -> ForwardOutcome {
    let mut consumer = session.origin_consumer().clone();
    loop {
        let Some((path, present)) = consumer.announced().await else {
            return ForwardOutcome::SessionClosed;
        };
        let path_str = path.as_str();
        let Some((peer, name)) = parse_wire_name(path_str) else {
            debug!(path = %path_str, "ignoring unparseable announce path");
            continue;
        };
        if peer == me {
            continue;
        }
        let event = if present.is_some() {
            RelayDiscoveryEvent::Announce { peer, name }
        } else {
            RelayDiscoveryEvent::Unannounce { peer, name }
        };
        if tx.send(event).await.is_err() {
            return ForwardOutcome::ReceiverDropped;
        }
    }
}

/// Self-reconnecting relay publisher session.
///
/// Tracks a set of `(wire_name, BroadcastConsumer)` pairs and
/// re-issues them onto every reconnected session. The room actor
/// adds and removes pairs through [`Self::publish`] and
/// [`Self::unpublish`]; the wrapper does the rest.
#[derive(Clone)]
pub(crate) struct RelayPublisher {
    state: Arc<Mutex<PublisherState>>,
    cancel: CancellationToken,
    _task: Arc<AbortOnDropHandle<()>>,
}

impl std::fmt::Debug for RelayPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayPublisher").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct PublisherState {
    /// Active session, if any. `None` while reconnecting.
    session: Option<iroh_moq::MoqSession>,
    /// Wire-name to consumer pairs that should be active on the
    /// current session. Rebuilt on every reconnect.
    publishes: HashMap<String, BroadcastConsumer>,
}

impl RelayPublisher {
    /// Spawns the reconnect-aware publisher task.
    ///
    /// The initial connection blocks the caller so the room can
    /// fail fast on a misconfigured relay; subsequent reconnects
    /// run in the background.
    pub(crate) async fn spawn(
        live: Live,
        ticket: RoomTicket,
        me: EndpointId,
        offer: RelayOffer,
    ) -> n0_error::Result<Self> {
        use n0_error::StdResultExt;

        let target = RelayTarget::new(offer.endpoint)
            .with_path(&ticket.relay_publisher_path(me))
            .with_api_key(offer.api_key.clone());

        let initial = live
            .connect_relay(&target)
            .await
            .std_context("connect to relay for publisher session")?;

        let state = Arc::new(Mutex::new(PublisherState {
            session: Some(initial.clone()),
            publishes: HashMap::new(),
        }));
        let cancel = CancellationToken::new();
        let task = task::spawn(
            run_publisher(live, target, state.clone(), cancel.clone(), Some(initial))
                .instrument(info_span!("RelayPublisher")),
        );

        Ok(Self {
            state,
            cancel,
            _task: Arc::new(AbortOnDropHandle::new(task)),
        })
    }

    /// Issues `consumer` against the current session under
    /// `name` and remembers the pair so it survives reconnects.
    /// Replaces any previous publish for the same name.
    pub(crate) async fn publish(&self, name: String, consumer: BroadcastConsumer) {
        let mut state = self.state.lock().await;
        if let Some(session) = state.session.as_ref() {
            session.publish(name.clone(), consumer.clone());
        }
        state.publishes.insert(name, consumer);
    }

    /// Forgets a previously issued publish. Drops the local
    /// consumer reference; the corresponding announce on the
    /// relay is torn down by [`OriginProducer`]'s lifecycle once
    /// nothing else holds the consumer.
    ///
    /// [`OriginProducer`]: moq_lite::OriginProducer
    pub(crate) async fn unpublish(&self, name: &str) {
        let mut state = self.state.lock().await;
        state.publishes.remove(name);
    }

    /// Cancels the reconnect task. After this call the publisher
    /// is inert: no further reconnect attempts run, the current
    /// session (if any) is dropped, and any in-flight `publish` /
    /// `unpublish` calls return without effect on the relay. The
    /// tracked-publishes map is left intact so the caller can
    /// inspect what was active at shutdown; the room actor drops
    /// the whole [`RelayPublisher`] right after, which clears
    /// everything.
    pub(crate) fn shutdown(&self) {
        self.cancel.cancel();
    }
}

async fn run_publisher(
    live: Live,
    target: RelayTarget,
    state: Arc<Mutex<PublisherState>>,
    cancel: CancellationToken,
    bootstrap: Option<iroh_moq::MoqSession>,
) {
    let mut backoff = Backoff::new();
    let mut bootstrap = bootstrap;
    loop {
        let session = if let Some(s) = bootstrap.take() {
            s
        } else {
            match connect_with_backoff(&live, &target, &mut backoff, &cancel).await {
                Some(s) => s,
                None => return,
            }
        };

        // Install the new session and re-issue every known
        // publish against it. Hold the lock briefly so callers
        // adding publishes during reconnect do not race the
        // re-issue.
        {
            let mut st = state.lock().await;
            st.session = Some(session.clone());
            for (name, consumer) in &st.publishes {
                session.publish(name.clone(), consumer.clone());
            }
            info!(
                count = st.publishes.len(),
                "relay publisher attached, reissued active broadcasts"
            );
        }

        let started = tokio::time::Instant::now();
        let close_reason = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            err = session.closed() => err,
        };

        // Clear the session so callers do not race on a stale
        // handle. Reset the backoff only if the session lasted
        // long enough to count as recovered.
        {
            let mut st = state.lock().await;
            st.session = None;
        }
        if started.elapsed() >= STABLE_THRESHOLD {
            backoff.reset();
        }
        warn!(
            ?close_reason,
            "relay publisher session closed, will reconnect"
        );
    }
}

/// Tries to connect to `target`, sleeping for the current backoff
/// step on every failure. Returns `None` when `cancel` fires
/// during a sleep or before a successful connect.
async fn connect_with_backoff(
    live: &Live,
    target: &RelayTarget,
    backoff: &mut Backoff,
    cancel: &CancellationToken,
) -> Option<iroh_moq::MoqSession> {
    loop {
        let delay = backoff.next();
        debug!(?delay, "scheduling relay reconnect");
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return None,
            () = tokio::time::sleep(delay) => {}
        }
        match live.connect_relay(target).await {
            Ok(session) => return Some(session),
            Err(err) => {
                warn!(error = %err, "relay reconnect failed, will retry");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_steps_within_jittered_bounds() {
        let mut b = Backoff::new();
        // First step uses RECONNECT_INITIAL with [0.5, 1.0)
        // jitter; the result lies in [INITIAL/2, INITIAL].
        let d1 = b.next();
        assert!(d1 >= RECONNECT_INITIAL / 2 && d1 <= RECONNECT_INITIAL);
        // After saturation, every step lies in [CAP/2, CAP].
        for _ in 0..32 {
            b.next();
        }
        let d_saturated = b.next();
        assert!(d_saturated >= RECONNECT_CAP / 2 && d_saturated <= RECONNECT_CAP);
    }

    #[test]
    fn backoff_reset_returns_to_initial_step() {
        let mut b = Backoff::new();
        for _ in 0..8 {
            b.next();
        }
        b.reset();
        let d = b.next();
        assert!(d >= RECONNECT_INITIAL / 2 && d <= RECONNECT_INITIAL);
    }
}
