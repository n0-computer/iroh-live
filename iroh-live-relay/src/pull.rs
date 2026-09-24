//! Pull mode: fetch remote broadcasts via iroh-live tickets.
//!
//! When a browser subscribes to a broadcast whose name is a valid
//! [`BroadcastTicket`], the relay resolves the ticket's broadcast through an
//! iroh-moq node of its own and mirrors it into the cluster under the name the
//! browser asked for, so the browser consumes it transparently.
//!
//! Mirroring is a splice: the cluster gets one dynamic route at the asked-for
//! name, answered with the subscribed broadcast itself, so nothing is copied
//! and nothing else the publisher happens to publish lands in the cluster.
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

use iroh_moq::{BroadcastTicket, Moq, MoqConfig, Reach, Subscription};
use moq_net::{broadcast, origin};
use moq_relay::cluster::Cluster;
use tokio::sync::watch;
use tracing::{debug, info, warn};

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
    /// Dials publishers and resolves ticket paths, old path layout included.
    moq: Moq,
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
            .field("endpoint", &self.moq.endpoint().id())
            .field("linger", &self.linger)
            .field("pulls", &self.pulls.lock().map(|pulls| pulls.len()).ok())
            .finish_non_exhaustive()
    }
}

/// How far a pull's dial has got.
#[derive(Debug, Clone)]
enum Dial {
    /// Still in flight. Every caller for this ticket waits on it.
    Pending,
    /// The session is up and the broadcast is mirroring into the cluster.
    Connected,
    /// The dial failed, and this is why. Carried as a string because every
    /// waiter gets a copy of it.
    Failed(String),
}

/// One pulled ticket: the state [`PullState::pull`] and the task holding the
/// session agree on.
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
            moq: Moq::new(endpoint, MoqConfig::default()),
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
    #[must_use]
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
    pub async fn pull(
        &self,
        requested: &str,
        ticket: &BroadcastTicket,
    ) -> anyhow::Result<PullGuard> {
        // The name the client asked for, not `ticket.to_string()`. A subscriber
        // is announced the exact path it subscribed to, so mirroring under the
        // canonical spelling serves a broadcast nobody asked for: a browser that
        // pasted the ticket without its `iroh-live:` scheme, which parses and is
        // what the QR code and the publish line both hand out, connected, waited,
        // and was never announced anything.
        //
        // Two spellings of one ticket therefore pull twice. That is the price of
        // serving what was asked for, and it is the right way round: a duplicate
        // upstream session costs a connection, and a mismatch costs the viewer
        // the stream.
        let local_name = requested.to_owned();

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
                let outcome = match state.do_connect(&ticket, &name, &pull).await {
                    Ok(()) => Dial::Connected,
                    Err(err) => {
                        // Retire the failed entry so the next pull for this
                        // ticket dials again rather than joining a session that
                        // never came up.
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

        // Dialling or joining, every caller waits on the one dial's outcome.
        let mut dial = guard.pull.dial.subscribe();
        let outcome = dial
            .wait_for(|dial| !matches!(dial, Dial::Pending))
            .await
            .map_or_else(
                // Needs every sender gone, and the dialling task holds one until
                // it reports, so this is the pull being dropped underneath us.
                |_| Dial::Failed("the pull was dropped before it connected".to_owned()),
                |dial| dial.clone(),
            );
        if let Dial::Failed(err) = outcome {
            anyhow::bail!("failed to pull {local_name}: {err}");
        }

        Ok(guard)
    }

    /// Resolves the ticket's broadcast, mirrors it into the cluster under
    /// `local_name`, and spawns the task that holds it and decides when to
    /// retire it.
    async fn do_connect(
        &self,
        ticket: &BroadcastTicket,
        local_name: &str,
        pull: &Arc<Pull>,
    ) -> anyhow::Result<()> {
        info!(
            remote = %ticket.peer().fmt_short(),
            broadcast = %ticket.name(),
            "pulling remote broadcast"
        );
        // Through the node rather than a hand-rolled session, so the pull dials
        // every MoQ version this build speaks and reaches a publisher on the
        // path layout before paths named their publisher as well.
        let subscription = self
            .moq
            .subscribe(ticket.path(), Reach::Direct)
            .await
            .map_err(|err| anyhow::anyhow!("failed to subscribe to the remote: {err:#}"))?;
        let mirror = self
            .cluster
            .origin
            .dynamic(local_name, origin::Route::default())
            .map_err(|err| anyhow::anyhow!("failed to mirror {local_name}: {err}"))?;
        info!(
            local_name = %local_name,
            remote = %ticket.peer().fmt_short(),
            path = %subscription.path(),
            "remote broadcast available locally"
        );
        tokio::spawn(self.clone().hold(
            local_name.to_owned(),
            Arc::clone(pull),
            subscription,
            mirror,
        ));
        Ok(())
    }

    /// Serves the mirror until either end is done with it, then retracts it.
    async fn hold(
        self,
        local_name: String,
        pull: Arc<Pull>,
        subscription: Subscription,
        mirror: origin::Dynamic,
    ) {
        let serve = async {
            // Each request is answered with the broadcast the subscription
            // resolves to now, so a change of route upstream is picked up by
            // the next reader.
            while let Ok(request) = mirror.requested_broadcast().await {
                request.accept(subscription.as_moq());
            }
        };
        tokio::select! {
            () = subscription.closed() => {
                info!(local_name = %local_name, "the publisher ended the pulled broadcast");
                self.retire(&local_name, &pull);
            }
            () = self.wait_idle(&local_name, &pull) => {
                info!(local_name = %local_name, "pull went idle, retracting the mirror");
            }
            () = serve => {
                warn!(local_name = %local_name, "the cluster stopped serving the mirror");
                self.retire(&local_name, &pull);
            }
        }
        // Dropping the route retracts the mirror from the cluster. The session
        // with the publisher stays with the node, which shares it with any
        // other pull of the same publisher.
        drop(mirror);
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
            self.cluster.origin.consume().routed_broadcast(local_name),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .map(|broadcast| broadcast.demand());
        if demand.is_none() {
            warn!(
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
    use iroh_moq::BroadcastTicket;

    #[test]
    fn ticket_round_trip() {
        let key = iroh::SecretKey::from_bytes(&[23u8; 32]);
        let ticket = BroadcastTicket::new(key.public(), "test-stream");
        let ticket_str = ticket.to_string();

        let parsed: BroadcastTicket = ticket_str.parse().expect("parse ticket");
        assert_eq!(parsed.name(), "test-stream");
        assert_eq!(parsed, ticket);
    }

    #[test]
    fn reject_invalid_ticket() {
        let result: Result<BroadcastTicket, _> = "not-a-valid-ticket".parse();
        assert!(result.is_err());
    }

    #[test]
    fn non_ticket_name_does_not_parse() {
        // Regular broadcast names should NOT parse as tickets.
        let result: Result<BroadcastTicket, _> = "hello".parse();
        assert!(result.is_err());
        let result: Result<BroadcastTicket, _> = "my-stream-360p".parse();
        assert!(result.is_err());
    }
}
