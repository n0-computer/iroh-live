//! Pull mode: serving a broadcast ticket's broadcast from the relay.
//!
//! When a session names a valid [`BroadcastTicket`], the relay subscribes to
//! the ticket's broadcast through its own iroh-moq node, and splices it into
//! the cluster under the name the session asked for.
//!
//! A pull retires once two signals have been quiet for the linger window. Each
//! local session that named the ticket holds a [`PullGuard`], and the mirrored
//! broadcast's [`Demand`](moq_net::broadcast::Demand) shows whether anything
//! reads it. When the last pull of a publisher retires, the relay closes its
//! session with that publisher. An idle timer on the connection cannot replace
//! either signal, since iroh's keep-alives move its counters too.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh::EndpointId;
use iroh_live::BroadcastTicket;
use iroh_moq::{Moq, Reach, Subscription};
use moq_net::{broadcast, origin};
use moq_relay::cluster::Cluster;
use n0_watcher::Watcher;
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// Default for [`PullState::with_linger`].
///
/// Long enough for a page reload to reuse the connection.
const DEFAULT_LINGER: Duration = Duration::from_secs(10);

/// How long to wait for the mirror's announcement before watching guards alone.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared state for pull operations.
#[derive(Clone, derive_more::Debug)]
pub struct PullState {
    moq: Moq,
    #[debug(skip)]
    cluster: Cluster,
    linger: Duration,
    /// Live or dialing pulls, by local broadcast name.
    ///
    /// Claims and retirements happen under this lock, so an entry found here
    /// is still open.
    pulls: Arc<Mutex<HashMap<String, Arc<Pull>>>>,
    /// How many pulls, live or dialing, each publisher has.
    ///
    /// The last one to go closes the session with the publisher.
    publishers: Arc<Mutex<HashMap<EndpointId, usize>>>,
}

/// How far a pull's dial has got.
#[derive(Debug, Clone)]
enum Dial {
    /// Still in flight. Every caller for this ticket waits on it.
    Pending,
    /// The session is up and the broadcast is mirroring into the cluster.
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
    /// Creates pull state that dials over `endpoint` and mirrors into `cluster`.
    pub fn new(endpoint: iroh::Endpoint, cluster: Cluster) -> Self {
        Self {
            moq: Moq::new(endpoint, iroh_live::moq_config()),
            cluster,
            linger: DEFAULT_LINGER,
            pulls: Arc::new(Mutex::new(HashMap::new())),
            publishers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Sets how long an unused pull stays connected.
    ///
    /// Defaults to ten seconds.
    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Pulls the broadcast `ticket` names into the cluster under `requested`.
    ///
    /// Hold the returned [`PullGuard`] while the session that asked runs.
    /// Concurrent pulls of one name share a dial. Cancelling this future only
    /// drops the caller's claim: the dial runs on, and the pull retires as
    /// usual.
    pub async fn pull(
        &self,
        requested: &str,
        ticket: &BroadcastTicket,
    ) -> anyhow::Result<PullGuard> {
        // The name the client asked for, not `ticket.to_string()`: a subscriber
        // is announced the exact path it subscribed to. Two spellings of one
        // ticket pull twice.
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
                let publisher = PublisherClaim::new(&state, ticket.peer());
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

        // Dialling or joining, every caller waits on the one dial's outcome.
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

    /// Subscribes to the ticket's broadcast and mirrors it into the cluster.
    ///
    /// Spawns the task that holds the pull until it retires.
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
        let subscription = self
            .moq
            .subscribe(ticket.path(), Reach::Direct(ticket.peer()))
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
            publisher,
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
        publisher: PublisherClaim,
    ) {
        let serve = async {
            // The broadcast the subscription resolves to now, so the next reader
            // follows a route change upstream.
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
        // Retracts the mirror, and closes the publisher's session if this was
        // its last pull.
        drop(mirror);
        drop(publisher);
    }

    /// Waits until the pull has nothing left to serve, then retires its entry.
    ///
    /// Both signals must be quiet: the guard count covers a session that named
    /// the ticket but has not subscribed yet, and demand covers a reader that
    /// holds no guard. The final check runs under the lock [`Self::pull`]
    /// claims under, so a pull claimed during the linger keeps running.
    async fn wait_idle(&self, local_name: &str, pull: &Arc<Pull>) {
        // Demand exists once the mirror is announced.
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
                // `Err` means every producer is gone: retire.
                && demand.unused().await.is_ok()
            {
                continue;
            }

            // Quiet for now. Wait out the linger, then confirm under the lock.
            tokio::time::sleep(self.linger).await;

            let mut pulls = self.pulls.lock().expect("poisoned");
            if pull.claimed() || demand.as_ref().is_some_and(broadcast::Demand::is_used) {
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

/// One pull's share in the session with a publisher.
///
/// Dropping the last share of a publisher closes every session the relay's
/// node has with it.
struct PublisherClaim {
    publisher: EndpointId,
    publishers: Arc<Mutex<HashMap<EndpointId, usize>>>,
    moq: Moq,
}

impl PublisherClaim {
    fn new(state: &PullState, publisher: EndpointId) -> Self {
        *state
            .publishers
            .lock()
            .expect("poisoned")
            .entry(publisher)
            .or_default() += 1;
        Self {
            publisher,
            publishers: Arc::clone(&state.publishers),
            moq: state.moq.clone(),
        }
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
        for session in self.moq.sessions().get() {
            if session.remote_id() == self.publisher {
                info!(remote = %self.publisher.fmt_short(), "last pull of the publisher retired, closing its session");
                session.close("no pull left");
            }
        }
    }
}
