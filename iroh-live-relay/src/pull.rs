//! Pull mode: fetch remote broadcasts via iroh-live tickets.
//!
//! When a browser subscribes to a broadcast whose name is a valid
//! `LiveTicket`, the relay connects to the remote publisher via iroh,
//! subscribes to the broadcast, and forwards it locally so the browser
//! can consume it transparently.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use iroh_live::ticket::LiveTicket;
use iroh_moq::MoqSession;
use moq_relay::Cluster;

/// Shared state for pull operations.
#[derive(Clone)]
pub(crate) struct PullState {
    live: iroh_live::Live,
    cluster: Cluster,
    /// Active pulls keyed by the ticket string. Prevents duplicate pulls
    /// for the same remote broadcast.
    active: Arc<Mutex<HashMap<String, PullHandle>>>,
}

/// Keeps a pull session alive. Dropping aborts the pull.
struct PullHandle {
    local_name: String,
    _session: MoqSession,
}

impl PullState {
    pub(crate) fn new(live: iroh_live::Live, cluster: Cluster) -> Self {
        Self {
            live,
            cluster,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Pulls a remote broadcast identified by a ticket and makes it available
    /// locally in the cluster. Returns the local broadcast name.
    ///
    /// Idempotent: if the same ticket is already being pulled, returns the
    /// existing local name without creating a new connection.
    pub(crate) async fn pull(&self, ticket: &LiveTicket) -> anyhow::Result<String> {
        let ticket_str = ticket.to_string();

        // Check if already pulling this ticket.
        {
            let active = self.active.lock().expect("lock");
            if let Some(handle) = active.get(&ticket_str) {
                tracing::debug!(
                    local_name = %handle.local_name,
                    "pull already active for ticket"
                );
                return Ok(handle.local_name.clone());
            }
        }

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

        // Use the full ticket string as the local name. This matches what the
        // browser subscribes to (the URL `?name=iroh-live:ADDR/broadcast` passes
        // the full ticket as the moq-lite broadcast path).
        let local_name = ticket.to_string();

        // Inject the remote broadcast into the relay's cluster.
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
        };
        self.active.lock().expect("lock").insert(ticket_str, handle);

        Ok(local_name)
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
