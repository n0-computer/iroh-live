//! Multi-source publisher that fans a broadcast out to a
//! configurable set of sessions.
//!
//! A [`Broadcaster`] owns one [`BroadcastProducer`] and attaches
//! clones of it to any number of MoQ sessions. The set of target
//! sessions is controlled through a [`SourceSetHandle`] and can
//! change at runtime: adding a source opens a session (if needed)
//! and starts the announce there; removing a source drops the
//! consumer, which ends the announce on that session only.
//!
//! Unlike [`Moq::publish`], which binds a producer to every current
//! and future session on the transport, a `Broadcaster` is opt-in
//! per session. Use it when you want control over which peers a
//! broadcast is reachable from, for instance to route through a
//! relay only or to deny direct peer fan-out in favour of a
//! centralised relay.

use std::{collections::HashMap, sync::Arc};

use iroh_moq::MoqSession;
use moq_lite::{BroadcastConsumer, BroadcastProducer};
use n0_error::{Result, StdResultExt};
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher;
use tokio::{sync::Mutex, task};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{
    Live,
    sources::{SourceId, SourceSet, SourceSetHandle, TransportSource},
};

/// Fans a single broadcast producer out to a dynamic set of
/// sessions.
///
/// Construct with [`Broadcaster::spawn`] and mutate the source set
/// through its [`SourceSetHandle`]. Dropping the broadcaster stops
/// the fan-out task, closes each attached session's announce, and
/// leaves the producer intact for the caller to drop at their
/// leisure.
#[derive(Debug)]
pub struct Broadcaster {
    handle: SourceSetHandle,
    _task: Arc<AbortOnDropHandle<()>>,
    shutdown: CancellationToken,
}

impl Broadcaster {
    /// Spawns a broadcaster that publishes `producer` on the
    /// sessions named by `sources`.
    ///
    /// The broadcaster reacts to changes in the source set
    /// reactively: pushing a new [`TransportSource`] causes the
    /// broadcaster to open a session and announce there; removing a
    /// source ends the announce. The source set may be updated any
    /// number of times.
    pub fn spawn(
        live: Live,
        name: impl ToString,
        producer: BroadcastProducer,
        sources: impl Into<SourceSetHandle>,
    ) -> Self {
        let name = name.to_string();
        let sources: SourceSetHandle = sources.into();
        let shutdown = CancellationToken::new();
        let task = spawn_broadcaster(
            live,
            name.clone(),
            producer,
            sources.clone(),
            shutdown.clone(),
        );
        Self {
            handle: sources,
            _task: Arc::new(AbortOnDropHandle::new(task)),
            shutdown,
        }
    }

    /// Returns the handle used to mutate the set of sources.
    pub fn sources(&self) -> &SourceSetHandle {
        &self.handle
    }

    /// Adds a source. Equivalent to `self.sources().push(source)`.
    pub fn add_source(&self, source: TransportSource) {
        self.handle.push(source);
    }

    /// Removes a source by id.
    pub fn remove_source(&self, id: &SourceId) -> bool {
        self.handle.remove(id)
    }

    /// Stops the broadcaster, ending the announce on every attached
    /// session. Equivalent to dropping the broadcaster.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

fn spawn_broadcaster(
    live: Live,
    name: String,
    producer: BroadcastProducer,
    sources: SourceSetHandle,
    shutdown: CancellationToken,
) -> task::JoinHandle<()> {
    let span = info_span!("Broadcaster", name = %name);
    task::spawn(
        async move {
            let state = Arc::new(Mutex::new(BroadcasterState::default()));
            // Reconcile whenever the watcher signals a change.
            let mut watcher = sources.watch();
            loop {
                let current = sources.get();
                if let Err(err) = reconcile(&live, &name, &producer, &current, state.clone()).await
                {
                    warn!("broadcaster reconcile failed: {err:#}");
                }

                tokio::select! {
                    _ = shutdown.cancelled() => {
                        debug!("broadcaster shutting down");
                        break;
                    }
                    res = watcher.updated() => {
                        if res.is_err() {
                            debug!("source set watcher closed");
                            break;
                        }
                    }
                }
            }
            // Release every attached consumer so the announce ends
            // on each session.
            state.lock().await.attached.clear();
        }
        .instrument(span),
    )
}

#[derive(Default)]
struct BroadcasterState {
    /// Map from source id to the consumer currently attached to that
    /// source's session. Dropping the consumer ends the announce.
    attached: HashMap<SourceId, AttachedSource>,
}

/// Per-source state held by the broadcaster.
///
/// Owns the outbound MoQ session opened for the target. Dropping
/// the wrapper closes the session, which ends every announce that
/// rode it: there is no per-broadcast unpublish in moq-lite, so
/// "remove this source" is implemented by tearing down the
/// session entirely.
struct AttachedSource {
    session: MoqSession,
    /// Held so the consumer ref-count stays high while the
    /// announce is live. Drop order is consumer-then-session.
    _consumer: BroadcastConsumer,
}

impl Drop for AttachedSource {
    fn drop(&mut self) {
        // Close the session so the announce ends on the wire.
        // Code 0 + empty reason is the conventional "clean close".
        self.session.close(0, b"broadcaster source removed");
    }
}

/// Drives `state.attached` toward `desired` for one source set
/// snapshot.
///
/// For every source already attached but absent from `desired`,
/// the corresponding [`AttachedSource`] is dropped, which closes
/// the session and ends the announce. For every source present
/// in `desired` but not yet attached, a new direct or relay
/// session is opened in parallel and the producer is published
/// against it. Failed attaches surface through the returned
/// [`Result`] but do not stop the loop; partially-applied state
/// is left in `state.attached` for the next reconcile pass.
async fn reconcile(
    live: &Live,
    name: &str,
    producer: &BroadcastProducer,
    desired: &SourceSet,
    state: Arc<Mutex<BroadcasterState>>,
) -> Result<()> {
    let mut state = state.lock().await;
    let desired_ids: Vec<SourceId> = desired.iter().map(|s| s.id()).collect();

    // Remove any attached source that is no longer desired.
    state.attached.retain(|id, _| desired_ids.contains(id));

    // Attach any desired source that is not yet attached.
    for source in desired.iter() {
        let id = source.id();
        if state.attached.contains_key(&id) {
            continue;
        }
        match attach(live, name, producer, source).await {
            Ok(attached) => {
                info!(source=%id, "attached broadcast");
                state.attached.insert(id, attached);
            }
            Err(err) => {
                warn!(source=%id, "failed to attach broadcast: {err:#}");
            }
        }
    }
    Ok(())
}

async fn attach(
    live: &Live,
    name: &str,
    producer: &BroadcastProducer,
    source: &TransportSource,
) -> Result<AttachedSource> {
    let session = match source {
        TransportSource::Direct(direct) => live
            .transport()
            .connect(direct.peer.clone())
            .await
            .std_context("connect to direct source")?,
        TransportSource::Relay(target) => live
            .connect_relay(target)
            .await
            .std_context("connect to relay source")?,
    };
    let consumer = producer.consume();
    session.publish(name.to_string(), consumer.clone());
    Ok(AttachedSource {
        session,
        _consumer: consumer,
    })
}
