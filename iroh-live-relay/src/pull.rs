//! Pull mode: serving a broadcast ticket's broadcast from the relay.
//!
//! The relay's iroh-live-node node uses the cluster origin as its route table. When
//! a session names a valid [`BroadcastTicket`], the node subscribes to the
//! ticket's broadcast, and the publisher's session announces it straight into
//! the cluster at its own path, `live/<publisher>/<name>`. Nothing is copied:
//! cluster readers read the session's broadcast. The session's grant lets the
//! publisher announce exactly the pulled paths, so its other broadcasts stay
//! out. A session that named the ticket reads it under the name it asked for
//! through a mount, see [`mounted`].
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
use iroh_live::BroadcastTicket;
use iroh_live_node::{ConnectOptions, Grant, Node, NodeConfig, Subscription};
use moq_net::{Pattern, Patterns};
use moq_relay::cluster::Cluster;
use n0_watcher::Watcher;
use tokio::sync::watch;
use tracing::{debug, info, warn};

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

/// Returns the cluster to serve a session that dialed `path`.
///
/// [`mounted`] at the name `path` asks for, if it names a ticket, and
/// `cluster` as it is otherwise.
pub fn serving(cluster: &Cluster, path: &str) -> Cluster {
    crate::iroh_sessions::requested_name(path)
        .and_then(|name| mounted(cluster, &name))
        .unwrap_or_else(|| cluster.clone())
}

/// Shared state for pull operations.
#[derive(Clone, derive_more::Debug)]
pub struct PullState {
    moq: Node,
    linger: Duration,
    /// Live or dialing pulls, by local broadcast name.
    ///
    /// Claims and retirements happen under this lock, so an entry found here
    /// is still open.
    pulls: Arc<Mutex<HashMap<String, Arc<Pull>>>>,
    /// The paths pulled, live or dialing, from each publisher.
    ///
    /// They make up the grant of the session with the publisher, and the last
    /// one to go closes that session.
    publishers: Publishers,
}

/// How far a pull's dial has got.
#[derive(Debug, Clone)]
enum Dial {
    /// Still in flight. Every caller for this ticket waits on it.
    Pending,
    /// The session is up and the publisher announced the broadcast.
    Connected,
    /// The dial failed. A string, since every waiter gets a copy.
    Failed(String),
}

/// One pulled ticket.
#[derive(Debug)]
struct Pull {
    dial: watch::Sender<Dial>,
    /// Local sessions that named this ticket and have not disconnected yet.
    claims: watch::Sender<usize>,
}

impl Pull {
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
    /// directly, each publisher kept to its own paths by the grant.
    pub fn new(endpoint: iroh::Endpoint, cluster: Cluster) -> Self {
        let config = NodeConfig {
            table: Some(cluster.origin.clone()),
            ..iroh_live::node_config_with(&[iroh_live::rooms::publish_scope])
        };
        Self {
            moq: Node::new(endpoint, config),
            linger: DEFAULT_LINGER,
            pulls: Arc::new(Mutex::new(HashMap::new())),
            publishers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Sets how long an unused pull stays connected.
    ///
    /// Defaults to ten seconds, long enough for a page reload to reuse the
    /// connection.
    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Pulls the broadcast `ticket` names into the cluster, for a session that asked for `requested`.
    ///
    /// Hold the returned [`PullGuard`] while the session that asked runs, and
    /// serve that session the cluster [`mounted`] at `requested`. Concurrent
    /// pulls of one name share a dial.
    ///
    /// Cancellation safe: dropping this future only drops the caller's claim.
    /// The dial runs on, and the pull retires as usual.
    ///
    /// # Errors
    ///
    /// Fails if the publisher cannot be reached, or does not announce the
    /// broadcast within [`SUBSCRIBE_TIMEOUT`].
    pub async fn pull(
        &self,
        requested: &str,
        ticket: &BroadcastTicket,
    ) -> anyhow::Result<PullGuard> {
        // Keyed by the name the client asked for, as its mount is. Two
        // spellings of one ticket share the publisher's session, but are
        // claimed apart.
        let local_name = requested.to_owned();

        // Claimed under the lock retirement takes, so an entry found here
        // outlives this call.
        let (pull, dial) = {
            let mut pulls = self.pulls.lock().expect("poisoned");
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
            let state = self.clone();
            let ticket = ticket.clone();
            let name = local_name.clone();
            let pull = Arc::clone(&guard.pull);
            tokio::spawn(async move {
                // Counted for the whole of the pull, dial included, so a
                // retiring pull cannot close the session this one is about to
                // use.
                let publisher = PublisherClaim::new(&state, &ticket);
                let outcome = match state.do_connect(&ticket, &name, &pull, publisher).await {
                    Ok(()) => Dial::Connected,
                    Err(err) => {
                        // Retire it, so the next pull dials again.
                        warn!(local_name = %name, %err, "pull dial failed");
                        state.retire(&name, &pull);
                        Dial::Failed(format!("{err:#}"))
                    }
                };
                pull.dial.send_replace(outcome);
            });
        } else {
            debug!(local_name = %local_name, "joining an existing pull");
        }

        // Dialing or joining, every caller waits on the one dial's outcome.
        let mut dial = guard.pull.dial.subscribe();
        let outcome = dial
            .wait_for(|dial| !matches!(dial, Dial::Pending))
            .await
            .map_or_else(
                // The dialing task holds a sender until it reports.
                |_| Dial::Failed("the pull was dropped before it connected".to_owned()),
                |dial| dial.clone(),
            );
        if let Dial::Failed(err) = outcome {
            anyhow::bail!("failed to pull {local_name}: {err}");
        }

        Ok(guard)
    }

    /// Resolves the ticket's broadcast, and spawns the task that holds the pull until it retires.
    async fn do_connect(
        &self,
        ticket: &BroadcastTicket,
        local_name: &str,
        pull: &Arc<Pull>,
        publisher: PublisherClaim,
    ) -> anyhow::Result<()> {
        info!(
            remote = %ticket.peer().fmt_short(),
            broadcast = %ticket.name(),
            "pulling remote broadcast"
        );
        let subscription = self.resolve(ticket, &publisher).await?;
        info!(
            local_name = %local_name,
            remote = %ticket.peer().fmt_short(),
            path = %subscription.path(),
            "remote broadcast available in the cluster"
        );
        tokio::spawn(self.clone().hold(
            ticket.clone(),
            local_name.to_owned(),
            Arc::clone(pull),
            subscription,
            publisher,
        ));
        Ok(())
    }

    /// Waits for the publisher to announce the ticket's broadcast into the cluster.
    ///
    /// Grants the session with the publisher the pulled paths before waiting,
    /// so only those enter the cluster, and resolves the path through that
    /// session, so no other peer can stand in for the publisher.
    async fn resolve(
        &self,
        ticket: &BroadcastTicket,
        claim: &PublisherClaim,
    ) -> anyhow::Result<Subscription> {
        // A publisher that never announces the path to us would keep the
        // subscription waiting forever.
        tokio::time::timeout(SUBSCRIBE_TIMEOUT, async {
            // A new session is authorized for the pulled paths before its
            // handshake, so the publisher's other broadcasts never enter the
            // cluster, not even for a moment.
            let options = ConnectOptions {
                grant: Some(claim.grant()),
                ..Default::default()
            };
            let session = self
                .moq
                .connect_with(ticket.peer(), options)
                .await
                .map_err(|err| anyhow::anyhow!("failed to reach the remote: {err:#}"))?;
            // A session that was up already ignores the options, and claims
            // may have changed the paths since the dial started.
            claim.regrant();
            session
                .subscribe(ticket.path())
                .await
                .map_err(|err| anyhow::anyhow!("failed to subscribe to the remote: {err:#}"))
        })
        .await
        .map_err(|_| anyhow::anyhow!("timed out subscribing to the remote"))?
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
        self,
        ticket: BroadcastTicket,
        local_name: String,
        pull: Arc<Pull>,
        mut current: Subscription,
        publisher: PublisherClaim,
    ) {
        loop {
            let ended = tokio::select! {
                () = current.closed() => {
                    info!(local_name = %local_name, "nothing serves the pulled broadcast any more");
                    true
                }
                () = self.wait_idle(&local_name, &pull) => {
                    info!(local_name = %local_name, "pull went idle, retiring it");
                    false
                }
            };
            if !ended {
                break;
            }
            if !pull.claimed() {
                self.retire(&local_name, &pull);
                break;
            }
            match self.resolve_again(&ticket, &local_name, &publisher).await {
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
    async fn resolve_again(
        &self,
        ticket: &BroadcastTicket,
        local_name: &str,
        publisher: &PublisherClaim,
    ) -> Option<Subscription> {
        let mut backoff = RESOLVE_BACKOFF;
        for attempt in 1..=MAX_RESOLVES {
            tokio::time::sleep(backoff).await;
            match self.resolve(ticket, publisher).await {
                Ok(next) => return Some(next),
                Err(err) => {
                    warn!(local_name = %local_name, attempt, %err, "could not resolve the pull again");
                    backoff = (backoff * 2).min(RESOLVE_BACKOFF_MAX);
                }
            }
        }
        None
    }

    /// Waits until no session has claimed the pull for the linger window, then retires its entry.
    ///
    /// The final check runs under the lock [`Self::pull`] claims under, so a
    /// pull claimed during the linger keeps running.
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

/// The paths pulled from each publisher, with how many pulls want each.
type Publishers = Arc<Mutex<HashMap<EndpointId, HashMap<String, usize>>>>;

/// Returns the grant of a session with a publisher whose `paths` are pulled.
///
/// The publisher may put exactly those paths into the cluster, and sees
/// nothing of it: its other broadcasts stay out, and a pull that retires takes
/// its path out again.
fn grant_for(paths: &HashMap<String, usize>) -> Grant {
    Grant {
        subscribe: Patterns::new(),
        publish: paths
            .keys()
            .filter_map(|path| Pattern::literal(path).ok())
            .collect(),
    }
}

/// Re-grants every session with `publisher` for `paths`.
fn regrant(moq: &Node, publisher: EndpointId, paths: &HashMap<String, usize>) {
    let grant = grant_for(paths);
    for session in moq.sessions().get() {
        if session.remote_id() == publisher {
            session.set_grant(grant.clone());
        }
    }
}

/// One pull's share in the session with a publisher, and in the path it pulls.
///
/// Dropping the last share of a path takes it out of the session's grant, and
/// so out of the cluster. Dropping the last share of a publisher closes every
/// session the relay's node has with it.
struct PublisherClaim {
    publisher: EndpointId,
    path: String,
    publishers: Publishers,
    moq: Node,
}

impl PublisherClaim {
    fn new(state: &PullState, ticket: &BroadcastTicket) -> Self {
        let publisher = ticket.peer();
        let path = ticket.path();
        let mut publishers = state.publishers.lock().expect("poisoned");
        let paths = publishers.entry(publisher).or_default();
        *paths.entry(path.clone()).or_default() += 1;
        // A session already up takes the path at once.
        regrant(&state.moq, publisher, paths);
        Self {
            publisher,
            path,
            publishers: Arc::clone(&state.publishers),
            moq: state.moq.clone(),
        }
    }

    /// Returns the grant of a session with this claim's publisher.
    fn grant(&self) -> Grant {
        self.publishers
            .lock()
            .expect("poisoned")
            .get(&self.publisher)
            .map(grant_for)
            .unwrap_or_else(|| grant_for(&HashMap::new()))
    }

    /// Re-grants every session with this claim's publisher for the paths pulled now.
    ///
    /// Runs under the lock claims change the paths under, as [`Self::new`]
    /// and the drop do, so a grant read before a concurrent claim or
    /// retirement never lands after it.
    fn regrant(&self) {
        let publishers = self.publishers.lock().expect("poisoned");
        let none = HashMap::new();
        let paths = publishers.get(&self.publisher).unwrap_or(&none);
        regrant(&self.moq, self.publisher, paths);
    }
}

impl Drop for PublisherClaim {
    fn drop(&mut self) {
        // Decided and done under the lock a new claim takes, so a pull that
        // starts now either counts before this and keeps the session, or
        // counts after the close and dials anew.
        let mut publishers = self.publishers.lock().expect("poisoned");
        let Some(paths) = publishers.get_mut(&self.publisher) else {
            return;
        };
        if let Some(count) = paths.get_mut(&self.path) {
            *count -= 1;
            if *count == 0 {
                paths.remove(&self.path);
            }
        }
        if !paths.is_empty() {
            regrant(&self.moq, self.publisher, paths);
            return;
        }
        publishers.remove(&self.publisher);
        for session in self.moq.sessions().get() {
            if session.remote_id() == self.publisher {
                info!(remote = %self.publisher.fmt_short(), "last pull of the publisher retired, closing its session");
                session.close("no pull left");
            }
        }
    }
}
