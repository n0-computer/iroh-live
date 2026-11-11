//! Pull mode: serving a broadcast ticket's broadcast from the relay.
//!
//! The relay's [`Node`] uses the cluster origin as its route table. When a
//! session names a valid [`BroadcastTicket`], the node subscribes to the
//! ticket's broadcast, and the publisher's session announces it straight into
//! the cluster at its own path. Nothing is copied: cluster readers read the
//! session's broadcast. The node's base grant is nothing, and each pull holds
//! a [`PathClaim`] that lets the publisher announce exactly the pulled path,
//! so its other broadcasts stay out. The relay pulls only a path the ticket's
//! peer owns: its canonical `live/<peer>/<name>` path, or a room path with the
//! peer as member, as the node's [`Layout`](iroh_live::layout::Layout) says.
//! Anything else, such as a path in a scheme of the publisher's own, is
//! refused with [`NotOwner`](iroh_live_node::Error::NotOwner). A session that named the
//! ticket reads it under the name it asked for through a mount, see
//! [`mounted`].
//!
//! A pull retires once no local session that named the ticket has held its
//! [`PullGuard`] for the linger window. Its path then leaves the publisher's
//! grant, which takes the broadcast out of the cluster, and when the last pull
//! of a publisher retires, the relay closes its session with that publisher.
//! An idle timer on the connection cannot stand in for the guards, since
//! iroh's keep-alives move its counters too. A reader that reaches the
//! broadcast at its own path without naming the ticket holds no guard, and
//! loses it when the pull retires.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::EndpointId;
use iroh_live::{BroadcastTicket, layout::LiveLayout, rooms::RoomsLayout};
use iroh_live_node::{Grant, Node, NodeConfig, PathClaim, Subscription, policy};
use moq_relay::cluster::Cluster;
use n0_error::e;
use n0_watcher::Watcher;
use tokio::{sync::watch, task::JoinSet};
use tracing::{debug, info, warn};

use crate::Error;

/// Default for [`PullState::with_linger`].
///
/// Long enough for a page reload to reuse the connection.
const DEFAULT_LINGER: Duration = Duration::from_secs(10);

/// How long to wait for the publisher's broadcast to resolve.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(15);

/// The first wait before resolving a broadcast again after it ended.
///
/// Doubles per failure, up to [`RESOLVE_BACKOFF_MAX`].
const RESOLVE_BACKOFF: Duration = Duration::from_millis(500);

/// The longest wait between attempts to resolve a broadcast again.
const RESOLVE_BACKOFF_MAX: Duration = Duration::from_secs(8);

/// How many times in a row resolving again may fail before the pull retires.
const MAX_RESOLVES: u32 = 5;

/// Returns the layout whose paths a pull may claim: `live/` and room paths.
///
/// A path is a peer's if it is exactly the canonical `live/<peer>/<name>`
/// path of that peer, or a room path `rooms/<topic>/<member>/<name>` whose
/// member is that peer. Any other path has no owner the relay can verify.
fn pull_layout() -> (LiveLayout, RoomsLayout) {
    (LiveLayout::default(), RoomsLayout)
}

/// Returns `cluster` with `requested` reading the broadcast its ticket names.
///
/// Hand it to the session that named the ticket: under it, the session finds
/// the pulled broadcast at `requested`, which moq resolves at the publisher's
/// path. `None` if `requested` is no ticket, or the origin refuses the mount.
pub fn mounted(cluster: &Cluster, requested: &str) -> Option<Cluster> {
    let ticket = requested.parse::<BroadcastTicket>().ok()?;
    match cluster.origin.mount(requested, ticket.path()) {
        Ok(origin) => {
            let mut cluster = cluster.clone();
            cluster.origin = origin;
            Some(cluster)
        }
        Err(err) => {
            warn!(%requested, %err, "cannot mount the ticket's broadcast");
            None
        }
    }
}

/// The relay's pulls, shared by every session that names a ticket.
///
/// Cheap to clone. Each pull runs in a task that the clones own together, so
/// dropping the last clone ends every pull.
///
/// # Examples
///
/// ```no_run
/// # async fn run(
/// #     endpoint: iroh::Endpoint,
/// #     cluster: moq_relay::cluster::Cluster,
/// #     requested: &str,
/// # ) -> Result<(), iroh_live_relay::Error> {
/// use iroh_live::BroadcastTicket;
/// use iroh_live_relay::pull::{PullState, mounted};
///
/// let pulls = PullState::new(endpoint, cluster.clone());
/// if let Ok(ticket) = requested.parse::<BroadcastTicket>() {
///     // Hold the guard while the session that asked runs.
///     let _guard = pulls.pull(requested, &ticket).await?;
///     let _view = mounted(&cluster, requested);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, derive_more::Debug)]
pub struct PullState {
    inner: Pulls,
    /// The task of each pull: its dial, and then its hold.
    #[debug(skip)]
    tasks: Arc<Mutex<JoinSet<()>>>,
}

/// What the pull tasks share: all of [`PullState`] but its tasks.
///
/// A task holds this, so it never keeps its own set alive.
#[derive(Clone, derive_more::Debug)]
struct Pulls {
    /// The node that dials publishers, with the cluster origin as its route table.
    node: Node,
    /// How long a pull without sessions stays connected.
    linger: Duration,
    /// Live or dialing pulls, by local broadcast name.
    ///
    /// Claims and retirements happen under this lock, so an entry found here
    /// is still open.
    pulls: Arc<Mutex<HashMap<String, Arc<Pull>>>>,
    /// How many pulls, live or dialing, each publisher has.
    ///
    /// The last one to go closes the session with the publisher.
    publishers: Publishers,
}

/// How far a pull's dial has got.
#[derive(Debug, Clone)]
enum Dial {
    /// Still in flight. Every caller for this ticket waits on it.
    Pending,
    /// The session is up and the publisher announced the broadcast.
    Connected,
    /// The dial failed. Every waiter gets a copy of the error.
    Failed(Arc<Error>),
}

/// One pulled ticket.
#[derive(Debug)]
struct Pull {
    /// The outcome of the dial.
    dial: watch::Sender<Dial>,
    /// Local sessions that named this ticket and have not disconnected yet.
    claims: watch::Sender<usize>,
}

impl Pull {
    /// Creates a pull whose dial is pending and that no session has claimed.
    fn new() -> Self {
        Self {
            dial: watch::Sender::new(Dial::Pending),
            claims: watch::Sender::new(0),
        }
    }

    /// Returns whether a local session that named this ticket is still connected.
    fn claimed(&self) -> bool {
        *self.claims.borrow() > 0
    }
}

/// Keeps a pull alive while the local session that asked for it runs.
///
/// Dropping it withdraws the session's claim on the pull.
#[derive(Debug)]
#[must_use = "dropping the guard withdraws the claim on the pull"]
pub struct PullGuard {
    pull: Arc<Pull>,
}

impl Drop for PullGuard {
    fn drop(&mut self) {
        self.pull.claims.send_modify(|claims| *claims -= 1);
    }
}

impl PullState {
    /// Creates pull state that dials over `endpoint` into `cluster`.
    ///
    /// The node writes what publishers announce into the cluster origin
    /// directly. Its base grant is nothing, so a publisher announces only the
    /// paths a pull claims, and only if it owns them in the `live/` or the
    /// rooms layout.
    #[must_use]
    pub fn new(endpoint: iroh::Endpoint, cluster: Cluster) -> Self {
        let config = NodeConfig {
            table: Some(cluster.origin.clone()),
            layout: Some(Arc::new(pull_layout())),
            ..NodeConfig::default().with_policy(policy::Open(Grant::nothing()))
        };
        Self {
            inner: Pulls {
                node: Node::new(endpoint, config),
                linger: DEFAULT_LINGER,
                pulls: Arc::new(Mutex::new(HashMap::new())),
                publishers: Arc::new(Mutex::new(HashMap::new())),
            },
            tasks: Arc::new(Mutex::new(JoinSet::new())),
        }
    }

    /// Sets how long an unused pull stays connected.
    ///
    /// Defaults to ten seconds, long enough for a page reload to reuse the
    /// connection.
    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.inner.linger = linger;
        self
    }

    /// Pulls the broadcast `ticket` names into the cluster.
    ///
    /// The pull is for a session that asked for `requested`. Hold the
    /// returned [`PullGuard`] while that session runs, and serve it the
    /// cluster [`mounted`] at `requested`. Concurrent pulls of one name share
    /// a dial.
    ///
    /// Cancellation safe: dropping this future only drops the caller's claim.
    /// The dial runs on, and the pull retires as usual.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Claim`] if the ticket's peer does not own its
    /// path, [`Error::Connect`] or [`Error::Subscribe`] if the publisher
    /// cannot be reached or refuses the path, and [`Error::Timeout`] if it does
    /// not announce the broadcast within 15 seconds.
    pub async fn pull(
        &self,
        requested: &str,
        ticket: &BroadcastTicket,
    ) -> Result<PullGuard, Error> {
        // Keyed by the name the client asked for, as its mount is. Two
        // spellings of one ticket share the publisher's session, but are
        // claimed apart.
        let local_name = requested.to_owned();

        // Claimed under the lock retirement takes, so an entry found here
        // outlives this call.
        let (pull, dial) = {
            let mut pulls = self.inner.pulls.lock().expect("poisoned");
            let (pull, dial) = match pulls.get(&local_name) {
                Some(pull) => (Arc::clone(pull), false),
                None => {
                    let pull = Arc::new(Pull::new());
                    pulls.insert(local_name.clone(), Arc::clone(&pull));
                    (pull, true)
                }
            };
            pull.claims.send_modify(|claims| *claims += 1);
            (pull, dial)
        };
        let guard = PullGuard { pull };

        if dial {
            let inner = self.inner.clone();
            let ticket = ticket.clone();
            let name = local_name.clone();
            let pull = Arc::clone(&guard.pull);
            self.spawn(async move {
                match inner.establish(&ticket, &name).await {
                    Ok((subscription, publisher)) => {
                        pull.dial.send_replace(Dial::Connected);
                        inner
                            .hold(ticket, name, pull, subscription, publisher)
                            .await;
                    }
                    Err(err) => {
                        // Retire it, so the next pull dials again.
                        warn!(local_name = %name, %err, "pull dial failed");
                        inner.retire(&name, &pull);
                        pull.dial.send_replace(Dial::Failed(Arc::new(err)));
                    }
                }
            });
        } else {
            debug!(%local_name, "joining an existing pull");
        }

        // Dialing or joining, every caller waits on the one dial's outcome.
        let mut dial = guard.pull.dial.subscribe();
        let outcome = dial
            .wait_for(|dial| !matches!(dial, Dial::Pending))
            .await
            .map_or_else(
                // The dialing task holds a sender until it reports.
                |_| Dial::Failed(Arc::new(e!(Error::Dropped))),
                |dial| dial.clone(),
            );
        if let Dial::Failed(err) = outcome {
            return Err(Error::clone(&err));
        }

        Ok(guard)
    }

    /// Spawns a pull task into the set, and drops the tasks that finished.
    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().expect("poisoned");
        while let Some(finished) = tasks.try_join_next() {
            if let Err(err) = finished
                && err.is_panic()
            {
                warn!(%err, "a pull task panicked");
            }
        }
        tasks.spawn(task);
    }
}

impl Pulls {
    /// Resolves the ticket's broadcast, and returns it with the publisher's claim.
    ///
    /// The caller holds the pull with [`hold`](Self::hold).
    async fn establish(
        &self,
        ticket: &BroadcastTicket,
        local_name: &str,
    ) -> Result<(Subscription, PublisherClaim), Error> {
        info!(
            remote = %ticket.peer().fmt_short(),
            path = %ticket.path(),
            "pulling remote broadcast"
        );
        // Held for the whole of the pull, dial included: it grants the
        // publisher its path, and a retiring pull cannot close the session
        // this one is about to use.
        let publisher = PublisherClaim::new(self, ticket)?;
        let subscription = self.resolve(ticket).await?;
        info!(
            %local_name,
            remote = %ticket.peer().fmt_short(),
            path = %subscription.path(),
            "remote broadcast available in the cluster"
        );
        Ok((subscription, publisher))
    }

    /// Waits for the publisher to announce the ticket's broadcast into the cluster.
    ///
    /// The caller's [`PublisherClaim`] already lets the publisher announce
    /// the path, so a session dialed now is authorized for it before its
    /// handshake. The path resolves through that session, so no other peer
    /// can stand in for the publisher.
    async fn resolve(&self, ticket: &BroadcastTicket) -> Result<Subscription, Error> {
        // A publisher that never announces the path to us would keep the
        // subscription waiting forever.
        tokio::time::timeout(SUBSCRIBE_TIMEOUT, async {
            let session = self.node.connect(ticket.peer()).await.map_err(|source| {
                e!(Error::Connect {
                    peer: ticket.peer(),
                    source: Box::new(source)
                })
            })?;
            session.subscribe(ticket.path()).await.map_err(|source| {
                e!(Error::Subscribe {
                    path: ticket.path().clone(),
                    source: Box::new(source)
                })
            })
        })
        .await
        .map_err(|_| {
            e!(Error::Timeout {
                path: ticket.path().clone(),
                timeout: SUBSCRIBE_TIMEOUT
            })
        })?
    }

    /// Holds the session with the publisher until nothing wants the pull.
    ///
    /// When no route serves the publisher's broadcast any more while sessions
    /// still want it (the session dropped, say), the pull resolves it again,
    /// which redials. A publisher that re-publishes over a live session needs
    /// nothing: its session announces the new broadcast into the cluster. The
    /// pull retires once nobody wants it, or once resolving fails
    /// [`MAX_RESOLVES`] times in a row.
    async fn hold(
        &self,
        ticket: BroadcastTicket,
        local_name: String,
        pull: Arc<Pull>,
        mut current: Subscription,
        publisher: PublisherClaim,
    ) {
        loop {
            let broadcast_ended = tokio::select! {
                () = current.closed() => {
                    info!(%local_name, "nothing serves the pulled broadcast any more");
                    true
                }
                () = self.wait_idle(&local_name, &pull) => {
                    info!(%local_name, "pull went idle, retiring it");
                    false
                }
            };
            if !broadcast_ended {
                break;
            }
            if !pull.claimed() {
                self.retire(&local_name, &pull);
                break;
            }
            match self.resolve_again(&ticket, &local_name).await {
                Some(next) => current = next,
                None => {
                    self.retire(&local_name, &pull);
                    break;
                }
            }
        }
        // Closes the publisher's session if this was its last pull, which
        // takes the broadcast out of the cluster.
        drop(publisher);
    }

    /// Resolves the ticket's broadcast again, backing off between attempts.
    ///
    /// Does not look at the pull's claims. A pull that loses its last session
    /// while this runs still tries [`MAX_RESOLVES`] times, about 90 seconds at
    /// worst, and redials the publisher for nobody.
    async fn resolve_again(
        &self,
        ticket: &BroadcastTicket,
        local_name: &str,
    ) -> Option<Subscription> {
        let mut backoff = RESOLVE_BACKOFF;
        for attempt in 1..=MAX_RESOLVES {
            tokio::time::sleep(backoff).await;
            match self.resolve(ticket).await {
                Ok(next) => return Some(next),
                Err(err) => {
                    warn!(%local_name, attempt, %err, "could not resolve the pull again");
                    backoff = (backoff * 2).min(RESOLVE_BACKOFF_MAX);
                }
            }
        }
        None
    }

    /// Waits until no session has claimed the pull for the linger window.
    ///
    /// Then it retires the pull's entry. The final check runs under the lock
    /// that [`Self::pull`] claims under, so a pull claimed during the linger
    /// keeps running.
    async fn wait_idle(&self, local_name: &str, pull: &Arc<Pull>) {
        loop {
            if pull.claimed() {
                let mut claims = pull.claims.subscribe();
                // `Err` needs every sender gone, and this task holds `pull`.
                let _ = claims.wait_for(|claims| *claims == 0).await;
                continue;
            }

            // Quiet for now. Wait out the linger, then confirm under the lock.
            tokio::time::sleep(self.linger).await;

            let mut pulls = self.pulls.lock().expect("poisoned");
            if pull.claimed() {
                continue;
            }
            remove_current(&mut pulls, local_name, pull);
            return;
        }
    }

    /// Drops `pull` from the map, if it is still the entry for `local_name`.
    ///
    /// A retired pull may have been replaced by a fresh dial for the same name.
    fn retire(&self, local_name: &str, pull: &Arc<Pull>) {
        remove_current(&mut self.pulls.lock().expect("poisoned"), local_name, pull);
    }
}

/// Removes the entry for `local_name` if it is `pull`.
fn remove_current(pulls: &mut HashMap<String, Arc<Pull>>, local_name: &str, pull: &Arc<Pull>) {
    if pulls
        .get(local_name)
        .is_some_and(|entry| Arc::ptr_eq(entry, pull))
    {
        pulls.remove(local_name);
    }
}

/// How many pulls each publisher has.
type Publishers = Arc<Mutex<HashMap<EndpointId, usize>>>;

/// One pull's share in the session with a publisher, and the claim on its path.
///
/// The claim lets the publisher announce the pulled path into the cluster.
/// Dropping it takes the path out again, and so out of the cluster. Dropping
/// the last share of a publisher closes every session the relay's node has
/// with it.
struct PublisherClaim {
    publisher: EndpointId,
    /// The count this claim is part of, shared with [`Pulls`].
    publishers: Publishers,
    node: Node,
    /// Released after [`Drop::drop`] closes the session, as fields drop after it.
    _path: PathClaim,
}

impl PublisherClaim {
    /// Claims the ticket's path for its peer, if the peer owns it.
    fn new(state: &Pulls, ticket: &BroadcastTicket) -> Result<Self, Error> {
        let publisher = ticket.peer();
        let path = state
            .node
            .claim_path(publisher, ticket.path())
            .map_err(|source| {
                e!(Error::Claim {
                    path: ticket.path().clone(),
                    source: Box::new(source)
                })
            })?;
        *state
            .publishers
            .lock()
            .expect("poisoned")
            .entry(publisher)
            .or_default() += 1;
        Ok(Self {
            publisher,
            publishers: Arc::clone(&state.publishers),
            node: state.node.clone(),
            _path: path,
        })
    }
}

impl Drop for PublisherClaim {
    fn drop(&mut self) {
        // Decided and done under the lock a new claim takes, so a pull that
        // starts now either counts before this and keeps the session, or
        // counts after the close and dials anew.
        let mut publishers = self.publishers.lock().expect("poisoned");
        let Some(count) = publishers.get_mut(&self.publisher) else {
            return;
        };
        *count -= 1;
        if *count > 0 {
            return;
        }
        publishers.remove(&self.publisher);
        for session in self.node.sessions().get() {
            if session.peer() == self.publisher {
                info!(
                    remote = %self.publisher.fmt_short(),
                    "last pull of the publisher retired, closing its session"
                );
                session.close(moq_net::Error::Cancel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use iroh_live::layout::Layout;
    use moq_net::Path;

    use super::*;

    /// Parses `path`, which must be valid.
    fn path(path: &str) -> Path {
        Path::new(path).expect("a path")
    }

    #[test]
    fn owner_accepts_only_canonical_live_and_rooms_paths() {
        let owner = |path: &Path| pull_layout().owner(path.as_str());
        let peer = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[4; 32]).public();
        assert_eq!(owner(&path(&format!("live/{peer}/cam"))), Some(peer));
        assert_eq!(owner(&path(&format!("live/{peer}/cam/hd"))), Some(peer));
        let topic = iroh_live::rooms::RoomTicket::generate().topic_id();
        assert_eq!(
            owner(&path(&format!("rooms/{topic}/{peer}/cam"))),
            Some(peer)
        );
        assert_eq!(owner(&path(&format!("rooms/{topic}/{peer}"))), None);
        assert_eq!(owner(&path(&format!("rooms/topic/{peer}/cam"))), None);
        assert_eq!(owner(&path("fleet/robot/cam")), None);
        assert_eq!(owner(&path(&format!("live/{peer}"))), None);
        assert_eq!(owner(&path(&format!("other/{peer}/cam"))), None);
        let base32 = data_encoding::BASE32_NOPAD
            .encode(other.as_bytes())
            .to_ascii_lowercase();
        assert_eq!(owner(&path(&format!("live/{base32}/cam"))), None);
    }
}
