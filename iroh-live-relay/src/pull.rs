//! Pull mode: fetch remote broadcasts via iroh-live tickets.
//!
//! When a browser subscribes to a broadcast whose name is a valid
//! `LiveTicket`, the relay connects to the remote publisher via iroh,
//! subscribes to the broadcast, and forwards it locally so the browser
//! can consume it transparently.
//!
//! The cluster manages broadcast lifecycle: when all subscribers
//! disconnect, the broadcast is removed. A background task holds the
//! `MoqSession` alive until the session closes, then logs the teardown.
//! No idle timer is needed.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use iroh_live::ticket::LiveTicket;
use moq_relay::Cluster;
use tokio::sync::Notify;

/// Shared state for pull operations.
#[derive(Clone)]
pub(crate) struct PullState {
    live: iroh_live::Live,
    cluster: Cluster,
    /// In-flight connection attempts keyed by ticket string. Prevents
    /// duplicate concurrent connections to the same remote (TOCTOU guard).
    /// Entries are removed once the connection completes or fails.
    connecting: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

impl PullState {
    pub(crate) fn new(live: iroh_live::Live, cluster: Cluster) -> Self {
        Self {
            live,
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

        // Fast path: broadcast already exists in the cluster.
        if self
            .cluster
            .primary
            .consume_broadcast(&local_name)
            .is_some()
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
                    .primary
                    .consume_broadcast(&local_name)
                    .is_some()
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

    /// Connects to the remote, subscribes, publishes into the cluster, and
    /// spawns a keepalive task that holds the session until it closes.
    async fn do_connect(&self, ticket: &LiveTicket, local_name: &str) -> anyhow::Result<()> {
        tracing::info!(
            remote = %ticket.endpoint.id.fmt_short(),
            broadcast = %ticket.broadcast_name,
            "pulling remote broadcast"
        );

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

        self.cluster.primary.publish_broadcast(local_name, consumer);

        tracing::info!(
            local_name = %local_name,
            remote = %ticket.endpoint.id.fmt_short(),
            "remote broadcast available locally"
        );

        // Keep the session alive until it closes. The cluster manages
        // broadcast lifecycle via reference counting on the consumer side.
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
