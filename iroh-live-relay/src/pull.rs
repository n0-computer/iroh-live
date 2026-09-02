//! Pull mode: fetch remote broadcasts via iroh-live tickets.
//!
//! When a browser subscribes to a broadcast whose name is a valid
//! [`LiveTicket`], the relay connects to the remote publisher via iroh,
//! subscribes to its broadcast, and mirrors it locally so the browser can
//! consume it transparently.
//!
//! Mirroring goes through the same mechanism a cluster peer connection uses:
//! a MoQ session handed a subscriber [`moq_net::origin::Producer`]
//! auto-ingests whatever the remote side announces into that origin. The
//! subscriber producer here is scoped down to the one broadcast the ticket
//! names (via [`moq_net::origin::Producer::scope`]) and re-rooted to the
//! ticket's local name (via [`moq_net::origin::Producer::with_root`]), so
//! only that broadcast lands in the cluster and nothing else the remote node
//! happens to publish leaks through.
//!
//! Nothing in the cluster owns the pulled QUIC connection, so it has to be
//! retired deliberately, and two signals decide when. Every local session that
//! named the ticket holds a [`PullGuard`], which accounts for a browser that has
//! connected but not subscribed yet. The mirrored broadcast's
//! [`Demand`](moq_net::broadcast::Demand) reports whether anything is reading
//! it, which accounts for a subscriber that reached the broadcast over some
//! other session and holds no guard. Once both have been quiet for
//! [`PullState::with_linger`]'s window the session is dropped, closing the
//! connection, and the ticket's entry is retired so the next pull dials afresh.
//!
//! A transport-level idle timer cannot stand in for either signal. Every counter
//! [`moq_net::Session::stats`] reports is a QUIC counter, and iroh sends
//! keep-alives every five seconds, so a connection nobody is reading moves them
//! exactly like a busy one does.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh_live::ticket::LiveTicket;
use moq_net::{Path, broadcast};
use moq_relay::Cluster;
use tokio::sync::watch;

/// Default for [`PullState::with_linger`].
///
/// Long enough that a page reload, or a viewer flipping between two streams,
/// reuses the connection instead of paying for another iroh dial and MoQ
/// handshake. Short enough that an abandoned ticket stops occupying a slot on
/// the publisher within seconds rather than minutes.
const DEFAULT_LINGER: Duration = Duration::from_secs(10);

/// How long to wait for the pulled broadcast to be announced into the cluster
/// before giving up on watching its demand.
///
/// The announce follows the handshake by a round trip in the normal case. A pull
/// that never sees one falls back to the guard count alone, which is the more
/// conservative of the two signals: it holds the session while a local session
/// that named the ticket is connected, and retires it a linger after the last
/// one leaves.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared state for pull operations.
#[derive(Clone)]
pub struct PullState {
    endpoint: iroh::Endpoint,
    cluster: Cluster,
    linger: Duration,
    /// One entry per ticket with a live or in-flight pull, keyed by the local
    /// broadcast name.
    ///
    /// Doubles as the TOCTOU guard for concurrent pulls of one ticket and as the
    /// handoff between [`PullState::pull`] and the task holding the session: a
    /// claim is taken and a pull is retired under this single lock, so an entry
    /// found here is always a session that is still open.
    pulls: Arc<Mutex<HashMap<String, Arc<Pull>>>>,
}

impl fmt::Debug for PullState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PullState")
            .field("endpoint", &self.endpoint.id())
            .field("linger", &self.linger)
            .field("pulls", &self.pulls.lock().map(|pulls| pulls.len()).ok())
            .finish_non_exhaustive()
    }
}

/// One pulled ticket: the state [`PullState::pull`] and the task holding the
/// session agree on.
#[derive(Debug)]
struct Pull {
    /// How the dial ended, or `None` while it is still in flight. The error is
    /// carried as a string because every waiter gets a copy of it.
    connected: watch::Sender<Option<Result<(), String>>>,
    /// Local sessions that named this ticket and have not disconnected yet.
    claims: watch::Sender<usize>,
}

impl Pull {
    fn new() -> Self {
        Self {
            connected: watch::Sender::new(None),
            claims: watch::Sender::new(0),
        }
    }

    /// Returns whether a local session that named this ticket is still connected.
    fn claimed(&self) -> bool {
        *self.claims.borrow() > 0
    }
}

/// Keeps a pulled broadcast connected while the local session that asked for it
/// is still around.
///
/// Returned by [`PullState::pull`]; hold it for as long as that session runs.
/// Dropping it closes nothing by itself, it only withdraws this session's
/// interest in the ticket.
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
    /// Creates pull state that dials over `endpoint` and mirrors into `cluster`.
    pub fn new(endpoint: iroh::Endpoint, cluster: Cluster) -> Self {
        Self {
            endpoint,
            cluster,
            linger: DEFAULT_LINGER,
            pulls: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Sets how long a pull stays connected after both the last local session
    /// holding a [`PullGuard`] and the last reader of the mirrored broadcast are
    /// gone.
    ///
    /// Defaults to ten seconds. Tests shorten it to keep the retirement path
    /// fast; a relay has no reason to change it.
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Pulls the remote broadcast a ticket names and makes it available in the
    /// cluster under the ticket's string form.
    ///
    /// Returns a [`PullGuard`] to hold for the lifetime of the local session
    /// that asked for the broadcast. Idempotent: a ticket that is already pulled
    /// hands back another guard on the running session, and concurrent pulls for
    /// one ticket share a single dial.
    ///
    /// Cancelling this future withdraws the caller's interest and nothing more.
    /// The dial it may have started runs to completion in its own task, and the
    /// session it produces is retired on the usual terms, so a caller that gives
    /// up (a timeout, say) cannot strand a ticket on an entry nobody will ever
    /// connect.
    pub async fn pull(&self, ticket: &LiveTicket) -> anyhow::Result<PullGuard> {
        let local_name = ticket.to_string();

        // Claiming and creating happen under the lock the holder task retires
        // under, so a claim and a retirement can never both believe they won: an
        // entry found here is guaranteed to outlive this call.
        let (pull, dial) = {
            let mut pulls = self.pulls.lock().expect("lock");
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
                let result = state.do_connect(&ticket, &name, &pull).await;
                if let Err(err) = &result {
                    // Retire the failed entry so the next pull for this ticket
                    // dials again rather than joining a session that never
                    // came up.
                    tracing::warn!(local_name = %name, %err, "pull dial failed");
                    state.retire(&name, &pull);
                }
                pull.connected
                    .send_replace(Some(result.map_err(|err| format!("{err:#}"))));
            });
        } else {
            tracing::debug!(local_name = %local_name, "pull: joining an existing pull");
        }

        // Dialling or joining, every caller waits on the one dial's outcome.
        let mut connected = guard.pull.connected.subscribe();
        connected
            .wait_for(|connected| connected.is_some())
            .await
            .ok()
            .and_then(|connected| (*connected).clone())
            .unwrap_or_else(|| Err("the pull was dropped before it connected".to_owned()))
            .map_err(|err| anyhow::anyhow!("failed to pull {local_name}: {err}"))?;

        Ok(guard)
    }

    /// Connects to the remote, subscribes to exactly the ticket's broadcast, and
    /// spawns the tasks that drive the session and decide when to retire it.
    async fn do_connect(
        &self,
        ticket: &LiveTicket,
        local_name: &str,
        pull: &Arc<Pull>,
    ) -> anyhow::Result<()> {
        tracing::info!(
            remote = %ticket.endpoint.id.fmt_short(),
            broadcast = %ticket.broadcast_name,
            "pulling remote broadcast"
        );

        // `local_name` is always `<prefix>/<broadcast_name>` (see
        // `LiveTicket::to_string`), and `broadcast_name` may itself contain
        // slashes, so split on the *first* one to recover the prefix.
        let prefix = local_name
            .split_once('/')
            .map_or(local_name, |(prefix, _)| prefix);
        let subscriber = self
            .cluster
            .origin
            .with_root(prefix)
            .and_then(|origin| origin.scope(&[Path::new(&ticket.broadcast_name)]))
            .ok_or_else(|| anyhow::anyhow!("failed to scope pull origin for {local_name}"))?;

        // Through `iroh_moq::dial` rather than a bare `connect`, so the pull
        // negotiates every MoQ version this build speaks and handles whichever
        // one the publisher chose. Dialing a single ALPN here would make a
        // publisher on an older release unpullable.
        let transport = iroh_moq::dial(&self.endpoint, ticket.endpoint.clone())
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect to remote: {e}"))?;
        let (session, driver) = moq_net::Client::new()
            .with_subscriber(subscriber)
            .connect(transport)
            .await
            .map_err(|e| anyhow::anyhow!("failed to open MoQ session to remote: {e}"))?;

        // Drives the session's protocol loop; the session makes no progress
        // without it, mirroring `moq_native::spawn_session`.
        tokio::spawn(driver);

        tracing::info!(
            local_name = %local_name,
            remote = %ticket.endpoint.id.fmt_short(),
            "remote broadcast available locally"
        );

        // The holder task owns the only `Session` clone from here on, so the
        // transport lives exactly as long as it decides to keep it.
        tokio::spawn(
            self.clone()
                .hold(local_name.to_owned(), Arc::clone(pull), session),
        );

        Ok(())
    }

    /// Holds a pulled session until either end is done with it, then closes it.
    async fn hold(self, local_name: String, pull: Arc<Pull>, session: moq_net::Session) {
        tokio::select! {
            err = session.closed() => {
                tracing::info!(local_name = %local_name, %err, "pull session closed by the publisher");
                self.retire(&local_name, &pull);
            }
            () = self.wait_idle(&local_name, &pull) => {
                tracing::info!(local_name = %local_name, "pull went idle, closing the session");
            }
        }

        // The only clone, so this closes the transport to the publisher; the
        // driver task notices and finishes on its own.
        drop(session);
    }

    /// Blocks until the pull has nothing left to serve, then retires its entry.
    ///
    /// Two signals have to agree. The guard count covers a local session that
    /// named the ticket and has not subscribed yet, which has no demand to show
    /// for itself. Demand on the mirrored broadcast covers a subscriber that
    /// reached it over some other session, which holds no guard. Either signal
    /// on its own would retire a pull somebody is still using.
    ///
    /// The decision is taken under the same lock [`Self::pull`] claims under, so
    /// a pull claimed while this was waiting out the linger keeps running, and a
    /// pull retired here can no longer be claimed. That is what makes the
    /// re-dial path safe: the next pull for the ticket finds no entry and starts
    /// a fresh session instead of joining one that is about to close.
    async fn wait_idle(&self, local_name: &str, pull: &Arc<Pull>) {
        // Demand is only watchable once the remote's announce has been mirrored
        // into the cluster, which is also the first moment there is anything to
        // read.
        let demand = tokio::time::timeout(
            ANNOUNCE_TIMEOUT,
            self.cluster
                .origin
                .consume()
                .announced_broadcast(local_name),
        )
        .await
        .ok()
        .flatten()
        .map(|broadcast| broadcast.demand());
        if demand.is_none() {
            tracing::warn!(
                local_name = %local_name,
                "pulled broadcast was never announced; falling back to the guard count alone"
            );
        }

        loop {
            if pull.claimed() {
                let mut claims = pull.claims.subscribe();
                // `Err` needs every sender gone, and this task holds `pull`.
                let _ = claims.wait_for(|claims| *claims == 0).await;
                continue;
            }

            if let Some(demand) = &demand
                && demand.is_used()
                // `Err` means every producer of the mirrored broadcast is gone,
                // which is as unread as it gets: fall through and retire.
                && demand.unused().await.is_ok()
            {
                continue;
            }

            // Quiet for now. Wait out the linger, then confirm under the lock.
            tokio::time::sleep(self.linger).await;

            let mut pulls = self.pulls.lock().expect("lock");
            if pull.claimed() || demand.as_ref().is_some_and(broadcast::Demand::is_used) {
                continue;
            }
            if pulls
                .get(local_name)
                .is_some_and(|entry| Arc::ptr_eq(entry, pull))
            {
                pulls.remove(local_name);
            }
            return;
        }
    }

    /// Drops `pull` from the map, if it is still the entry for `local_name`.
    ///
    /// Identity-checked: a pull that was already retired may have been replaced
    /// by a fresh dial for the same ticket, and that one belongs to its own
    /// holder task.
    fn retire(&self, local_name: &str, pull: &Arc<Pull>) {
        let mut pulls = self.pulls.lock().expect("lock");
        if pulls
            .get(local_name)
            .is_some_and(|entry| Arc::ptr_eq(entry, pull))
        {
            pulls.remove(local_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use iroh_live::ticket::LiveTicket;

    #[test]
    fn ticket_round_trip() {
        let key = iroh::SecretKey::from_bytes(&[23u8; 32]);
        let addr = iroh::EndpointAddr::from(key.public());
        let ticket = LiveTicket::new(addr, "test-stream");
        let ticket_str = ticket.to_string();

        let parsed: LiveTicket = ticket_str.parse().expect("parse ticket");
        assert_eq!(parsed.broadcast_name, "test-stream");
        assert_eq!(parsed.endpoint, ticket.endpoint);
    }

    #[test]
    fn reject_invalid_ticket() {
        let result: Result<LiveTicket, _> = "not-a-valid-ticket".parse();
        assert!(result.is_err());
    }

    #[test]
    fn non_ticket_name_does_not_parse() {
        // Regular broadcast names should NOT parse as tickets.
        let result: Result<LiveTicket, _> = "hello".parse();
        assert!(result.is_err());
        let result: Result<LiveTicket, _> = "my-stream-360p".parse();
        assert!(result.is_err());
    }
}
