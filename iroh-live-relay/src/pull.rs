//! Pull mode: serving a broadcast ticket's broadcast from the relay.
//!
//! The relay's [`Node`] uses the cluster origin as its route table. When a
//! session names a valid [`BroadcastTicket`], the node subscribes to the
//! ticket's broadcast, and the publisher's session announces it straight into
//! the cluster at its own path. The node's base grant is nothing, and each
//! pull holds a [`PathClaim`] that lets the publisher announce exactly the
//! pulled path, so its other broadcasts stay out. The relay pulls only a path
//! the ticket's peer owns: its canonical `live/<peer>/<name>` path, or a room
//! path with the peer as member. Anything else is refused with
//! [`NotOwner`](iroh_live_node::Error::NotOwner). A session that named the
//! ticket reads it under the name it asked for through [`mounted`].
//!
//! A pull retires once no local session that named the ticket has held its
//! [`PullGuard`] for the linger window. Its path then leaves the publisher's
//! grant, which takes the broadcast out of the cluster, and when the last pull
//! of a publisher retires, the relay closes its session with that publisher.
//! An idle timer on the connection cannot stand in for the guards, since
//! iroh's keep-alives move its counters too.
//!
//! Every pull dials an iroh endpoint the session chose, so admission into
//! pulls is bounded. [`Access`] says which sessions may pull at all: by
//! default only one whose token grants reading the ticket's path. [`Limits`]
//! caps the pulls the relay holds at once, the pulls one [`Client`] holds, and
//! how fast one client starts new dials. A session names one broadcast in its
//! path, so it holds at most one pull.
//!
//! [`run`](crate::run) sets this up. The module is public for a binary that
//! embeds the relay, and for the tests.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::EndpointId;
use iroh_live::{BroadcastTicket, layout::LiveLayout, rooms::RoomsLayout};
use iroh_live_node::{Grant, Node, NodeConfig, PathClaim, Subscription, policy};
use moq_relay::cluster::Cluster;
use n0_error::e;
use n0_watcher::Watcher;
use tokio::{sync::watch, task::JoinSet, time::Instant};
use tracing::{debug, info, warn};

use crate::Error;

/// Default for [`Pulls::with_linger`].
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

/// Which sessions may pull the ticket they name.
///
/// [`admit`](crate::admit) and [`IrohSessions`](crate::IrohSessions) check it
/// before they start a pull. [`Pulls::pull`] and [`Pulls::start`] do not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Access {
    /// Only a session whose token grants reading the ticket's path.
    ///
    /// The relay verifies the token as it does for the session itself, but at
    /// the path of the ticket's broadcast. Without `--auth-key` no token
    /// verifies, so nobody pulls.
    #[default]
    Authorized,
    /// Any session, with or without a token.
    ///
    /// Anyone who reaches the relay can then make it dial any iroh endpoint,
    /// within the [`Limits`]. Use it for local and development relays, never
    /// for a public one.
    Open,
}

/// Bounds on the pulls a relay holds and starts.
///
/// `#[non_exhaustive]`, so start from [`Limits::default`] and set the fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// The most pulls, dialing, live or lingering, the relay holds at once.
    ///
    /// Defaults to 256.
    pub total: usize,
    /// The most pulls one [`Client`] holds at once, over all its sessions.
    ///
    /// Defaults to 16.
    pub per_client: usize,
    /// How many new dials one [`Client`] may start in a burst.
    ///
    /// Joining a pull that is already dialing or live starts no dial.
    /// Defaults to 8.
    pub dial_burst: u32,
    /// How long a [`Client`] waits to earn one more dial, up to [`dial_burst`](Self::dial_burst).
    ///
    /// Defaults to five seconds.
    pub dial_interval: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            total: 256,
            per_client: 16,
            dial_burst: 8,
            dial_interval: Duration::from_secs(5),
        }
    }
}

/// Who asks for a pull, as the per-client [`Limits`] count it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Client {
    /// A browser, by its IP address.
    Addr(IpAddr),
    /// An iroh client, by its endpoint id.
    ///
    /// Endpoint ids cost nothing to make, so the global
    /// [`total`](Limits::total) is what bounds a client that makes many.
    Peer(EndpointId),
    /// A session whose transport reports no address. All of them share one budget.
    Unknown,
}

impl Client {
    /// Returns the client at `addr`, with an IPv4-mapped IPv6 address folded to IPv4.
    #[must_use]
    pub fn from_addr(addr: Option<std::net::SocketAddr>) -> Self {
        addr.map_or(Self::Unknown, |addr| Self::Addr(addr.ip().to_canonical()))
    }
}

/// What one client holds and may still start.
#[derive(Debug)]
struct Budget {
    /// How many [`PullGuard`]s the client holds.
    holds: usize,
    /// How many dials the client may start now.
    dials: u32,
    /// When `dials` was last topped up.
    since: Instant,
}

impl Budget {
    /// Returns the budget of a client that has started nothing yet.
    fn full(limits: &Limits, now: Instant) -> Self {
        Self {
            holds: 0,
            dials: limits.dial_burst,
            since: now,
        }
    }

    /// Adds the dials earned since the last top-up, up to the burst.
    fn refill(&mut self, limits: &Limits, now: Instant) {
        let interval = limits.dial_interval.max(Duration::from_millis(1));
        let earned = now.saturating_duration_since(self.since).as_nanos() / interval.as_nanos();
        let missing = limits.dial_burst.saturating_sub(self.dials);
        match u32::try_from(earned) {
            Ok(earned) if earned < missing => {
                self.dials += earned;
                // Keeps the part of an interval already waited.
                self.since = interval
                    .checked_mul(earned)
                    .and_then(|spent| self.since.checked_add(spent))
                    .unwrap_or(now);
            }
            _ => {
                self.dials = limits.dial_burst;
                self.since = now;
            }
        }
    }

    /// Takes one dial, if the client has one left.
    fn take_dial(&mut self, limits: &Limits, now: Instant) -> bool {
        self.refill(limits, now);
        let Some(left) = self.dials.checked_sub(1) else {
            return false;
        };
        self.dials = left;
        true
    }

    /// Returns whether forgetting this budget changes nothing: no holds and every dial back.
    fn is_idle(&mut self, limits: &Limits, now: Instant) -> bool {
        self.refill(limits, now);
        self.holds == 0 && self.dials >= limits.dial_burst
    }
}

/// The budgets of the clients that hold a pull or started one recently.
type Clients = Arc<Mutex<HashMap<Client, Budget>>>;

/// How many budgets the relay keeps before it forgets the idle ones.
const CLIENTS_KEPT: usize = 1024;

/// Returns the layout whose paths a pull may claim: `live/` and room paths.
fn pull_layout() -> (LiveLayout, RoomsLayout) {
    (LiveLayout::default(), RoomsLayout)
}

/// Returns `cluster` with `requested` reading the broadcast its ticket names.
///
/// Hand it to the session that named the ticket, which then finds the pulled
/// broadcast at `requested`. `None` if `requested` is no ticket, or the origin
/// refuses the mount.
#[must_use]
pub fn mounted(cluster: &Cluster, requested: &str) -> Option<Cluster> {
    let ticket = requested.parse::<BroadcastTicket>().ok()?;
    match cluster.origin.mount(requested, ticket.path()) {
        Ok(origin) => {
            let mut cluster = cluster.clone();
            cluster.origin = origin;
            Some(cluster)
        }
        Err(err) => {
            warn!(%requested, error = %err, "cannot mount the ticket's broadcast");
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
/// use iroh_live_relay::pull::{Client, Pulls, mounted};
///
/// let pulls = Pulls::new(endpoint, cluster.clone());
/// if let Ok(ticket) = requested.parse::<BroadcastTicket>() {
///     // Hold the guard while the session that asked runs.
///     let _guard = pulls.pull(Client::Unknown, requested, &ticket).await?;
///     let _view = mounted(&cluster, requested);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, derive_more::Debug)]
pub struct Pulls {
    shared: Shared,
    /// Which sessions may pull, checked by the admissions.
    access: Access,
    /// The task of each pull: its dial, and then its hold.
    #[debug(skip)]
    tasks: Arc<Mutex<JoinSet<()>>>,
}

/// What the pull tasks share: all of [`Pulls`] but its tasks.
///
/// A task holds this, so it never keeps its own set alive.
#[derive(Clone, derive_more::Debug)]
struct Shared {
    /// The node that dials publishers.
    node: Node,
    /// How long a pull without sessions stays connected.
    linger: Duration,
    /// Live or dialing pulls, by local broadcast name.
    ///
    /// Claims and retirements happen under this lock, so an entry found here
    /// is still open.
    pulls: Arc<Mutex<HashMap<String, Arc<Pull>>>>,
    /// How many pulls, live or dialing, each publisher has.
    publishers: Publishers,
    /// The bounds on pulls.
    limits: Limits,
    /// What each client holds and may still start.
    ///
    /// Taken after `pulls` when both are held.
    clients: Clients,
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
    /// How many local sessions that named this ticket hold a guard on it.
    holders: watch::Sender<usize>,
}

impl Pull {
    /// Creates a pull whose dial is pending and that no session holds.
    fn new() -> Self {
        Self {
            dial: watch::Sender::new(Dial::Pending),
            holders: watch::Sender::new(0),
        }
    }

    /// Returns whether a local session that named this ticket still holds it.
    fn is_held(&self) -> bool {
        *self.holders.borrow() > 0
    }
}

/// A hold on a pull, which keeps it alive while the session that asked for it runs.
///
/// Dropping it releases the session's hold on the pull, and the client's
/// share of [`Limits::per_client`].
#[derive(derive_more::Debug)]
#[must_use = "dropping the guard releases the hold on the pull"]
pub struct PullGuard {
    pull: Arc<Pull>,
    client: Client,
    #[debug(skip)]
    clients: Clients,
    limits: Limits,
}

impl PullGuard {
    /// Waits until the pull's dial connects, or returns its error.
    ///
    /// A guard from [`Pulls::start`] holds the pull from the start; this
    /// only reports how its dial went. Every holder of one pull gets the same
    /// outcome.
    ///
    /// # Errors
    ///
    /// Fails as [`Pulls::pull`] does once its limits passed.
    pub async fn connected(&self) -> Result<(), Error> {
        let mut dial = self.pull.dial.subscribe();
        let outcome = dial
            .wait_for(|dial| !matches!(dial, Dial::Pending))
            .await
            .map_or_else(
                // The dialing task holds a sender until it reports.
                |_| Dial::Failed(Arc::new(e!(Error::Dropped))),
                |dial| dial.clone(),
            );
        match outcome {
            Dial::Failed(err) => Err(Error::clone(&err)),
            Dial::Pending | Dial::Connected => Ok(()),
        }
    }
}

impl Drop for PullGuard {
    fn drop(&mut self) {
        self.pull.holders.send_modify(|holders| *holders -= 1);
        let mut clients = self.clients.lock().expect("poisoned");
        if let Some(budget) = clients.get_mut(&self.client) {
            budget.holds -= 1;
            if budget.is_idle(&self.limits, Instant::now()) {
                clients.remove(&self.client);
            }
        }
    }
}

impl Pulls {
    /// Creates the pulls of a relay, which dial over `endpoint` into `cluster`.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn new(endpoint: iroh::Endpoint, cluster: Cluster) -> Self {
        let config = NodeConfig {
            table: Some(cluster.origin.clone()),
            layout: Some(Arc::new(pull_layout())),
            ..NodeConfig::default().with_policy(policy::Open(Grant::nothing()))
        };
        Self {
            shared: Shared {
                node: Node::new(endpoint, config),
                linger: DEFAULT_LINGER,
                pulls: Arc::new(Mutex::new(HashMap::new())),
                publishers: Arc::new(Mutex::new(HashMap::new())),
                limits: Limits::default(),
                clients: Arc::new(Mutex::new(HashMap::new())),
            },
            access: Access::default(),
            tasks: Arc::new(Mutex::new(JoinSet::new())),
        }
    }

    /// Sets which sessions may pull, which defaults to [`Access::Authorized`].
    #[must_use]
    pub fn with_access(mut self, access: Access) -> Self {
        self.access = access;
        self
    }

    /// Returns which sessions may pull.
    #[must_use]
    pub fn access(&self) -> Access {
        self.access
    }

    /// Sets the bounds on pulls, which default to [`Limits::default`].
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.shared.limits = limits;
        self
    }

    /// Sets how long an unused pull stays connected.
    ///
    /// Defaults to ten seconds, long enough for a page reload to reuse the
    /// connection.
    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.shared.linger = linger;
        self
    }

    /// Pulls the broadcast `ticket` names into the cluster, and waits for it to connect.
    ///
    /// The pull is for a session of `client` that asked for `requested`.
    /// Hold the returned [`PullGuard`] while that session runs, and serve it
    /// the cluster [`mounted`] at `requested`. Concurrent pulls of one name
    /// share a dial. It does not check [`Access`].
    ///
    /// Cancellation safe: dropping this future only drops the caller's hold.
    /// The dial runs on, and the pull retires as usual.
    ///
    /// # Errors
    ///
    /// Fails as [`start`](Self::start) does, then with [`Error::Claim`] if
    /// the ticket's peer does not own its path, [`Error::Connect`] or
    /// [`Error::Subscribe`] if the publisher cannot be reached or refuses the
    /// path, and [`Error::Timeout`] if it does not announce the broadcast
    /// within 15 seconds.
    pub async fn pull(
        &self,
        client: Client,
        requested: &str,
        ticket: &BroadcastTicket,
    ) -> Result<PullGuard, Error> {
        let guard = self.start(client, requested, ticket)?;
        guard.connected().await?;
        Ok(guard)
    }

    /// Starts pulling the broadcast `ticket` names, or joins the pull of `requested`.
    ///
    /// Returns at once with the hold, before the dial connects: await
    /// [`PullGuard::connected`] for its outcome. The [`Limits`] are checked
    /// here, so a session that exceeds them can be refused before it starts.
    /// It does not check [`Access`].
    ///
    /// # Errors
    ///
    /// Fails with [`Error::ClientPullLimit`] if `client` already holds
    /// [`Limits::per_client`] pulls. A new dial also fails with
    /// [`Error::PullLimit`] if the relay holds [`Limits::total`] pulls, and
    /// with [`Error::DialRateLimit`] if `client` has used up its dials.
    pub fn start(
        &self,
        client: Client,
        requested: &str,
        ticket: &BroadcastTicket,
    ) -> Result<PullGuard, Error> {
        // Keyed by the name the client asked for, as its mount is. Two
        // spellings of one ticket share the publisher's session, but are
        // held apart.
        let local_name = requested.to_owned();
        let limits = self.shared.limits;

        // Held under the lock retirement takes, so an entry found here
        // outlives this call.
        let (pull, dial) = {
            let mut pulls = self.shared.pulls.lock().expect("poisoned");
            let mut clients = self.shared.clients.lock().expect("poisoned");
            let now = Instant::now();
            if clients.len() >= CLIENTS_KEPT {
                clients.retain(|_, budget| !budget.is_idle(&limits, now));
            }
            let budget = clients
                .entry(client)
                .or_insert_with(|| Budget::full(&limits, now));
            if budget.holds >= limits.per_client {
                return Err(e!(Error::ClientPullLimit {
                    limit: limits.per_client
                }));
            }
            let (pull, dial) = match pulls.get(&local_name) {
                Some(pull) => (Arc::clone(pull), false),
                None => {
                    if pulls.len() >= limits.total {
                        return Err(e!(Error::PullLimit {
                            limit: limits.total
                        }));
                    }
                    if !budget.take_dial(&limits, now) {
                        return Err(e!(Error::DialRateLimit));
                    }
                    let pull = Arc::new(Pull::new());
                    pulls.insert(local_name.clone(), Arc::clone(&pull));
                    (pull, true)
                }
            };
            budget.holds += 1;
            pull.holders.send_modify(|holders| *holders += 1);
            (pull, dial)
        };
        let guard = PullGuard {
            pull,
            client,
            clients: Arc::clone(&self.shared.clients),
            limits,
        };

        if dial {
            let shared = self.shared.clone();
            let ticket = ticket.clone();
            let name = local_name;
            let pull = Arc::clone(&guard.pull);
            self.spawn(async move {
                match shared.establish(&ticket, &name).await {
                    Ok((subscription, publisher)) => {
                        pull.dial.send_replace(Dial::Connected);
                        shared
                            .hold(ticket, name, pull, subscription, publisher)
                            .await;
                    }
                    Err(err) => {
                        // Retire it, so the next pull dials again.
                        warn!(local_name = %name, error = %err, "pull dial failed");
                        shared.retire(&name, &pull);
                        pull.dial.send_replace(Dial::Failed(Arc::new(err)));
                    }
                }
            });
        } else {
            debug!(%local_name, "joining an existing pull");
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
                warn!(error = %err, "a pull task panicked");
            }
        }
        tasks.spawn(task);
    }
}

impl Shared {
    /// Resolves the ticket's broadcast, and returns it with the publisher's claim.
    async fn establish(
        &self,
        ticket: &BroadcastTicket,
        local_name: &str,
    ) -> Result<(Subscription, PublisherClaim), Error> {
        info!(
            peer = %ticket.peer().fmt_short(),
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
            peer = %ticket.peer().fmt_short(),
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
    /// still want it, the pull resolves it again, which redials. The pull
    /// retires once nobody wants it, or once resolving fails [`MAX_RESOLVES`]
    /// times in a row.
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
            if !pull.is_held() {
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
        // Closes the publisher's session if this was its last pull.
        drop(publisher);
    }

    /// Resolves the ticket's broadcast again, backing off between attempts.
    ///
    /// Does not look at the pull's holders. A pull that loses its last session
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
                    warn!(%local_name, attempt, error = %err, "could not resolve the pull again");
                    backoff = (backoff * 2).min(RESOLVE_BACKOFF_MAX);
                }
            }
        }
        None
    }

    /// Waits until no session has held the pull for the linger window.
    ///
    /// The final check runs under the lock that [`Pulls::pull`] takes a hold
    /// under, so a pull held during the linger keeps running.
    async fn wait_idle(&self, local_name: &str, pull: &Arc<Pull>) {
        loop {
            if pull.is_held() {
                let mut holders = pull.holders.subscribe();
                // `Err` needs every sender gone, and this task holds `pull`.
                let _ = holders.wait_for(|holders| *holders == 0).await;
                continue;
            }

            // Quiet for now. Wait out the linger, then confirm under the lock.
            tokio::time::sleep(self.linger).await;

            let mut pulls = self.pulls.lock().expect("poisoned");
            if pull.is_held() {
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
/// Dropping it takes the path out of the cluster. Dropping the last share of
/// a publisher closes every session the relay's node has with it.
struct PublisherClaim {
    publisher: EndpointId,
    /// The count this claim is part of, kept in [`Shared`].
    publishers: Publishers,
    node: Node,
    /// Released after [`Drop::drop`] closes the session, as fields drop after it.
    _path: PathClaim,
}

impl PublisherClaim {
    /// Claims the ticket's path for its peer, if the peer owns it.
    fn new(shared: &Shared, ticket: &BroadcastTicket) -> Result<Self, Error> {
        let publisher = ticket.peer();
        let path = shared
            .node
            .claim_path(publisher, ticket.path())
            .map_err(|source| {
                e!(Error::Claim {
                    path: ticket.path().clone(),
                    source: Box::new(source)
                })
            })?;
        *shared
            .publishers
            .lock()
            .expect("poisoned")
            .entry(publisher)
            .or_default() += 1;
        Ok(Self {
            publisher,
            publishers: Arc::clone(&shared.publishers),
            node: shared.node.clone(),
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
                    peer = %self.publisher.fmt_short(),
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

    /// A client spends its burst, then earns one dial per interval, never more than the burst.
    #[test]
    fn a_budget_refills_one_dial_per_interval() {
        let limits = Limits {
            dial_burst: 2,
            dial_interval: Duration::from_secs(10),
            ..Limits::default()
        };
        let start = Instant::now();
        let mut budget = Budget::full(&limits, start);
        assert!(budget.take_dial(&limits, start));
        assert!(budget.take_dial(&limits, start));
        assert!(!budget.take_dial(&limits, start + Duration::from_secs(9)));
        assert!(budget.take_dial(&limits, start + Duration::from_secs(10)));
        assert!(!budget.take_dial(&limits, start + Duration::from_secs(15)));
        // The five seconds already waited count towards the next dial.
        assert!(budget.take_dial(&limits, start + Duration::from_secs(20)));
        // A long wait refills to the burst, and no further.
        let later = start + Duration::from_secs(3600);
        assert!(budget.is_idle(&limits, later));
        assert!(budget.take_dial(&limits, later));
        assert!(budget.take_dial(&limits, later));
        assert!(!budget.take_dial(&limits, later));
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
