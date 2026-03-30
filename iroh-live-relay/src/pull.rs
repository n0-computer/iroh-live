//! Pull mode: fetch remote broadcasts via iroh-live tickets.
//!
//! When a browser subscribes to a broadcast whose name is a valid
//! `LiveTicket`, the relay connects to the remote publisher via iroh,
//! subscribes to the broadcast, and forwards it locally so the browser
//! can consume it transparently.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use iroh_live::ticket::LiveTicket;
use iroh_moq::MoqSession;
use moq_relay::Cluster;
use tokio::sync::Notify;

/// How long an idle pull entry stays in the map before cleanup removes it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Shared state for pull operations.
#[derive(Clone)]
pub(crate) struct PullState {
    live: iroh_live::Live,
    cluster: Cluster,
    /// Active pulls keyed by the ticket string. Prevents duplicate pulls
    /// for the same remote broadcast.
    active: Arc<Mutex<HashMap<String, PullEntry>>>,
}

/// Tracks the lifecycle of a single pull: connecting, connected, or idle.
enum PullEntry {
    /// Connection in progress. Other callers wait on the `Notify`.
    Connecting(Arc<Notify>),
    /// Connected and actively forwarding.
    Connected(Box<PullHandle>),
}

/// Keeps a pull session alive. Dropping aborts the pull.
struct PullHandle {
    local_name: String,
    _session: MoqSession,
    /// Last time a caller referenced this pull. Used by the cleanup task
    /// to remove entries that have been idle for longer than [`IDLE_TIMEOUT`].
    last_used: Instant,
}

impl PullState {
    pub(crate) fn new(live: iroh_live::Live, cluster: Cluster) -> Self {
        let state = Self {
            live,
            cluster,
            active: Arc::new(Mutex::new(HashMap::new())),
        };
        state.spawn_cleanup_task();
        state
    }

    /// Pulls a remote broadcast identified by a ticket and makes it available
    /// locally in the cluster. Returns the local broadcast name.
    ///
    /// Idempotent: if the same ticket is already being pulled, returns the
    /// existing local name without creating a new connection. Concurrent
    /// pulls for the same ticket block until the first one completes.
    pub(crate) async fn pull(&self, ticket: &LiveTicket) -> anyhow::Result<String> {
        let ticket_str = ticket.to_string();

        // Determine what to do under a single short-lived lock acquisition.
        enum Action {
            AlreadyConnected(String),
            WaitForConnect(Arc<Notify>),
            StartConnect(Arc<Notify>),
        }
        let action = {
            let mut active = self.active.lock().expect("lock");
            match active.get_mut(&ticket_str) {
                Some(PullEntry::Connected(handle)) => {
                    handle.last_used = Instant::now();
                    tracing::debug!(
                        local_name = %handle.local_name,
                        "pull already active for ticket"
                    );
                    Action::AlreadyConnected(handle.local_name.clone())
                }
                Some(PullEntry::Connecting(notify)) => Action::WaitForConnect(Arc::clone(notify)),
                None => {
                    let notify = Arc::new(Notify::new());
                    active.insert(
                        ticket_str.clone(),
                        PullEntry::Connecting(Arc::clone(&notify)),
                    );
                    Action::StartConnect(notify)
                }
            }
        };
        // Lock is dropped here before any await point.

        match action {
            Action::AlreadyConnected(name) => Ok(name),
            Action::WaitForConnect(notify) => {
                notify.notified().await;
                let active = self.active.lock().expect("lock");
                match active.get(&ticket_str) {
                    Some(PullEntry::Connected(handle)) => Ok(handle.local_name.clone()),
                    _ => anyhow::bail!("pull for ticket failed or was removed"),
                }
            }
            Action::StartConnect(notify) => self.do_connect(ticket, ticket_str, notify).await,
        }
    }

    /// Performs the actual connection after inserting the `Connecting` sentinel.
    async fn do_connect(
        &self,
        ticket: &LiveTicket,
        ticket_str: String,
        notify: Arc<Notify>,
    ) -> anyhow::Result<String> {
        tracing::info!(
            remote = %ticket.endpoint.id.fmt_short(),
            broadcast = %ticket.broadcast_name,
            "pulling remote broadcast"
        );

        let result = self.connect_and_subscribe(ticket).await;

        match result {
            Ok((session, consumer, local_name)) => {
                self.cluster
                    .primary
                    .publish_broadcast(&local_name, consumer);

                tracing::info!(
                    local_name = %local_name,
                    remote = %ticket.endpoint.id.fmt_short(),
                    "remote broadcast available locally"
                );

                let handle = PullHandle {
                    local_name: local_name.clone(),
                    _session: session,
                    last_used: Instant::now(),
                };
                self.active
                    .lock()
                    .expect("lock")
                    .insert(ticket_str, PullEntry::Connected(Box::new(handle)));

                notify.notify_waiters();
                Ok(local_name)
            }
            Err(e) => {
                self.active.lock().expect("lock").remove(&ticket_str);
                notify.notify_waiters();
                Err(e)
            }
        }
    }

    /// Connects to the remote and subscribes to the broadcast.
    async fn connect_and_subscribe(
        &self,
        ticket: &LiveTicket,
    ) -> anyhow::Result<(MoqSession, moq_lite::BroadcastConsumer, String)> {
        let mut session = self
            .live
            .transport()
            .connect(ticket.endpoint.clone())
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect to remote: {e}"))?;

        let consumer = session
            .subscribe(&ticket.broadcast_name)
            .await
            .map_err(|e| anyhow::anyhow!("failed to subscribe to broadcast: {e}"))?;

        let local_name = ticket.to_string();
        Ok((session, consumer, local_name))
    }

    /// Spawns a background task that periodically removes idle pull entries.
    fn spawn_cleanup_task(&self) {
        let active = Arc::clone(&self.active);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let mut map = active.lock().expect("lock");
                let before = map.len();
                map.retain(|_, entry| match entry {
                    PullEntry::Connecting(_) => true,
                    PullEntry::Connected(handle) => handle.last_used.elapsed() < IDLE_TIMEOUT,
                });
                let removed = before - map.len();
                if removed > 0 {
                    tracing::debug!(
                        removed,
                        remaining = map.len(),
                        "cleaned up idle pull entries"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use iroh_live::ticket::LiveTicket;

    #[test]
    fn ticket_round_trip() {
        let key = iroh::SecretKey::generate(&mut rand::rng());
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
