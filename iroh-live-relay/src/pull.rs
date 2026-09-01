//! Pull mode: fetch remote broadcasts via iroh-live tickets.
//!
//! When a browser subscribes to a broadcast whose name is a valid
//! `LiveTicket`, the relay connects to the remote publisher via iroh,
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
//! The cluster manages broadcast lifecycle: when all subscribers disconnect,
//! the broadcast is removed. A background task drives the session's protocol
//! loop and holds the session alive until it closes, then logs the teardown.
//! No idle timer is needed.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use iroh_live::ticket::LiveTicket;
use moq_net::Path;
use moq_relay::Cluster;
use tokio::sync::Notify;

/// Shared state for pull operations.
#[derive(Clone)]
pub(crate) struct PullState {
    endpoint: iroh::Endpoint,
    cluster: Cluster,
    /// In-flight connection attempts keyed by ticket string. Prevents
    /// duplicate concurrent connections to the same remote (TOCTOU guard).
    /// Entries are removed once the connection completes or fails.
    connecting: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

impl PullState {
    pub(crate) fn new(endpoint: iroh::Endpoint, cluster: Cluster) -> Self {
        Self {
            endpoint,
            cluster,
            connecting: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Pulls a remote broadcast identified by a ticket and makes it available
    /// locally in the cluster. Returns the local broadcast name.
    ///
    /// Idempotent: if the broadcast already exists in the cluster, returns
    /// immediately. Concurrent pulls for the same ticket block until the
    /// first one completes.
    pub(crate) async fn pull(&self, ticket: &LiveTicket) -> anyhow::Result<String> {
        let local_name = ticket.to_string();

        // Fast path: broadcast already exists in the cluster. `request_broadcast`
        // resolves synchronously either way here, since the cluster's origin never
        // registers a `Dynamic` handler: a hit returns the live broadcast, a miss
        // fails immediately with `Unroutable` rather than waiting on a handler that
        // will never show up.
        if self
            .cluster
            .origin
            .consume()
            .request_broadcast(&local_name)
            .await
            .is_ok()
        {
            tracing::debug!(
                local_name = %local_name,
                "pull: broadcast already available in cluster"
            );
            return Ok(local_name);
        }

        // Check if another task is already connecting for this ticket.
        enum Action {
            WaitForConnect(Arc<Notify>),
            StartConnect(Arc<Notify>),
        }
        let action = {
            let mut connecting = self.connecting.lock().expect("lock");
            if let Some(notify) = connecting.get(&local_name) {
                Action::WaitForConnect(Arc::clone(notify))
            } else {
                let notify = Arc::new(Notify::new());
                connecting.insert(local_name.clone(), Arc::clone(&notify));
                Action::StartConnect(notify)
            }
        };

        match action {
            Action::WaitForConnect(notify) => {
                notify.notified().await;
                // The connecting task finished. Check whether the broadcast
                // appeared in the cluster (success) or not (failure).
                if self
                    .cluster
                    .origin
                    .consume()
                    .request_broadcast(&local_name)
                    .await
                    .is_ok()
                {
                    Ok(local_name)
                } else {
                    anyhow::bail!("pull for ticket failed or was removed")
                }
            }
            Action::StartConnect(notify) => {
                let result = self.do_connect(ticket, &local_name).await;

                // Clear the connecting sentinel and wake waiters regardless
                // of success or failure.
                self.connecting.lock().expect("lock").remove(&local_name);
                notify.notify_waiters();

                result.map(|_| local_name)
            }
        }
    }

    /// Connects to the remote, subscribes to exactly the ticket's broadcast, and
    /// spawns tasks that drive the session and hold it alive until it closes.
    async fn do_connect(&self, ticket: &LiveTicket, local_name: &str) -> anyhow::Result<()> {
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

        let connection = self
            .endpoint
            .connect(ticket.endpoint.clone(), iroh_moq::ALPN)
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect to remote: {e}"))?;
        let transport = web_transport_iroh::Session::raw(connection);
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

        // Keep the session alive until it closes. The cluster manages
        // broadcast lifecycle via reference counting on the mirrored origin.
        // When the remote disconnects or the session errors, this task
        // ends and the session drops, which tears down the transport.
        let name_owned = local_name.to_owned();
        tokio::spawn(async move {
            let err = session.closed().await;
            tracing::info!(
                local_name = %name_owned,
                error = %err,
                "pull session closed"
            );
        });

        Ok(())
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
