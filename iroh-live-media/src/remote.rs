//! Remote broadcasts: a catalog and a subscription, not yet playing.
//!
//! [`RemoteBroadcast`] reads a broadcast's catalog and keeps the subscription
//! open. Any number of [`Player`]s and [`Recording`]s read it; each owns what
//! it does with the media.

use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use moq_mux::catalog::Stream as _;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::{Watchable, Watcher as _};
use tracing::{Instrument, debug, info, trace, warn};

use crate::{
    Catalog, LocalBroadcast, NetworkSignals, Player, PlayerConfig, RecordConfig, Recording,
    catalog::HangCatalog, error::Error, network::SharedSignals,
};

/// How long a broadcast that ended is looked for again through the route
/// table before it counts as gone.
///
/// A change of route ends a broadcast only for the next request to find it
/// through the new route at once, so a second is plenty; anything longer is a
/// publisher that left. [`RemoteBroadcast::closed`] documents the cost: a
/// deliberate hang-up closes this late too.
const REROUTE_PATIENCE: Duration = Duration::from_secs(3);

/// The broadcast consumer a player reads, and how many times it has changed.
///
/// Compared by generation, which is what a watcher needs: a new consumer is a
/// new generation even when it reaches the same broadcast.
#[derive(Clone, Default)]
pub(crate) struct Epoch {
    pub(crate) generation: u64,
    pub(crate) consumer: Option<moq_net::broadcast::Consumer>,
}

impl fmt::Debug for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Epoch")
            .field("generation", &self.generation)
            .field("connected", &self.consumer.is_some())
            .finish()
    }
}

impl PartialEq for Epoch {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
    }
}

impl Eq for Epoch {}

/// Where a remote broadcast comes from.
#[derive(Clone)]
enum Origin {
    /// One broadcast consumer, which ends the remote broadcast when it ends.
    Moq,
    /// A path in a route table, requested again when a change of route ends it.
    Routed {
        origin: moq_net::origin::Consumer,
        path: moq_net::PathOwned,
    },
}

struct Shared {
    origin: Origin,
    epoch: Watchable<Epoch>,
    catalog: Watchable<Option<Catalog>>,
    closed: Watchable<bool>,
    network: Mutex<Option<SharedSignals>>,
    span: tracing::Span,
    _task: AbortOnDropHandle<()>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteBroadcast")
            .field("epoch", &self.epoch.get())
            .field("closed", &self.closed.get())
            .finish_non_exhaustive()
    }
}

/// A broadcast being read.
///
/// Cheap to clone. Holding it keeps the broadcast subscription; its tracks are
/// subscribed by the players and recordings that read it. Constructing one
/// spawns the task that reads the catalog, so it has to happen within a Tokio
/// runtime.
#[derive(Debug, Clone)]
pub struct RemoteBroadcast {
    shared: Arc<Shared>,
}

impl RemoteBroadcast {
    /// Starts reading the catalog of `broadcast`.
    ///
    /// Does not wait: [`catalog`](Self::catalog) is `None` until it arrives,
    /// which keeps construction usable in a UI reconcile loop.
    pub fn from_moq(broadcast: moq_net::broadcast::Consumer) -> Self {
        let span = tracing::info_span!("remote");
        Self::spawn(Origin::Moq, Some(broadcast), span)
    }

    /// Follows `path` in a route table.
    ///
    /// When a change of route ends the broadcast, it is requested again through
    /// the next route, and players see a switch rather than an end. The
    /// broadcast counts as closed only once no route serves the path; see
    /// [`closed`](Self::closed) for how long that takes to tell.
    ///
    /// The first request waits for as long as it takes, since the publisher
    /// may not have announced the path yet. A caller that already resolved the
    /// path uses [`from_resolved`](Self::from_resolved) instead.
    pub fn from_origin(origin: moq_net::origin::Consumer, path: impl moq_net::AsPath) -> Self {
        let path = path.as_path().to_owned();
        let span = tracing::info_span!("remote", path = %path);
        Self::spawn(Origin::Routed { origin, path }, None, span)
    }

    /// Follows `path` in a route table, starting from `broadcast`, which the
    /// caller already resolved there.
    ///
    /// As [`from_origin`](Self::from_origin), except that nothing waits for a
    /// first route: every later request is a failover and gets the same
    /// bounded patience, so a publisher that is gone by the time this runs
    /// closes the broadcast rather than leaving it waiting forever. A
    /// transport's subscription resolves the path before it hands it out, and
    /// this is how it passes that on; `iroh-live` does it on subscribe.
    pub fn from_resolved(
        origin: moq_net::origin::Consumer,
        path: impl moq_net::AsPath,
        broadcast: moq_net::broadcast::Consumer,
    ) -> Self {
        let path = path.as_path().to_owned();
        let span = tracing::info_span!("remote", path = %path);
        Self::spawn(Origin::Routed { origin, path }, Some(broadcast), span)
    }

    /// Reads a local broadcast in-process, without a transport.
    pub fn local(broadcast: &LocalBroadcast) -> Self {
        Self::from_moq(moq_net::Consume::consume(broadcast))
    }

    /// Attaches the link's view, for automatic rendition selection.
    ///
    /// Transports call this; `iroh-live` does it on subscribe. Players started
    /// afterwards read it. The signals belong to the broadcast rather than to
    /// this handle, so every clone sees them, and attaching again replaces
    /// them for every clone. A transport that stops producing readings leaves
    /// players adapting on the last one, so it should be kept for as long as
    /// the broadcast is played.
    #[must_use]
    pub fn with_network(self, signals: impl NetworkSignals) -> Self {
        *self.shared.network.lock().expect("poisoned") = Some(SharedSignals(Arc::new(signals)));
        self
    }

    /// Returns a watcher over the catalog, `None` until the first arrives.
    pub fn catalog(&self) -> n0_watcher::Direct<Option<Catalog>> {
        self.shared.catalog.watch()
    }

    /// Starts one playback of the broadcast.
    ///
    /// # Errors
    ///
    /// Fails for an invalid config: a latency whose minimum is above its
    /// maximum.
    pub fn play(&self, config: PlayerConfig) -> Result<Player, Error> {
        Player::start(self.clone(), config)
    }

    /// Starts recording the broadcast into `out`, remuxed without decoding.
    ///
    /// # Errors
    ///
    /// Fails if the rendition the config names is not in the catalog.
    pub fn record(
        &self,
        out: impl tokio::io::AsyncWrite + Send + Unpin + 'static,
        config: RecordConfig,
    ) -> Result<Recording, Error> {
        Recording::start(self, Box::new(out), config)
    }

    /// Waits until the broadcast has closed.
    ///
    /// A broadcast read with [`from_moq`](Self::from_moq) or
    /// [`local`](Self::local) closes when its consumer does. One that follows a
    /// route table, from [`from_origin`](Self::from_origin) or
    /// [`from_resolved`](Self::from_resolved), cannot tell a publisher that
    /// ended its broadcast from a change of route, which also ends it: it asks
    /// the table again, and closes only once no route has answered for three
    /// seconds. So a hang-up shows here about three seconds after the
    /// publisher closed.
    ///
    /// Cancellation safe.
    pub async fn closed(&self) {
        let mut closed = self.shared.closed.watch();
        loop {
            if closed.get() {
                return;
            }
            if closed.updated().await.is_err() {
                return;
            }
        }
    }

    /// Reports whether the broadcast has closed.
    ///
    /// Subject to the same re-resolve window as [`closed`](Self::closed).
    pub fn is_closed(&self) -> bool {
        self.shared.closed.get()
    }

    /// Returns a watcher over the consumer players read.
    pub(crate) fn epoch(&self) -> n0_watcher::Direct<Epoch> {
        self.shared.epoch.watch()
    }

    /// Returns the network signals, if a transport attached any.
    pub(crate) fn network(&self) -> Option<SharedSignals> {
        self.shared.network.lock().expect("poisoned").clone()
    }

    /// Returns the span everything reading this broadcast logs under.
    pub(crate) fn span(&self) -> &tracing::Span {
        &self.shared.span
    }

    /// Returns the route table and path, for a broadcast that follows one.
    pub(crate) fn routed(&self) -> Option<(moq_net::origin::Consumer, moq_net::PathOwned)> {
        match &self.shared.origin {
            Origin::Routed { origin, path } => Some((origin.clone(), path.clone())),
            Origin::Moq => None,
        }
    }

    fn spawn(
        origin: Origin,
        first: Option<moq_net::broadcast::Consumer>,
        span: tracing::Span,
    ) -> Self {
        let epoch = Watchable::new(Epoch::default());
        let catalog = Watchable::new(None);
        let closed = Watchable::new(false);
        let task = {
            let origin = origin.clone();
            let epoch = epoch.clone();
            let catalog = catalog.clone();
            let closed = closed.clone();
            n0_future::task::spawn(
                follow(origin, first, epoch, catalog, closed).instrument(span.clone()),
            )
        };
        Self {
            shared: Arc::new(Shared {
                origin,
                epoch,
                catalog,
                closed,
                network: Mutex::new(None),
                span,
                _task: AbortOnDropHandle::new(task),
            }),
        }
    }
}

/// Reads the broadcast, and requests it again through the route table when a
/// change of route ends it.
async fn follow(
    origin: Origin,
    first: Option<moq_net::broadcast::Consumer>,
    epoch: Watchable<Epoch>,
    catalog: Watchable<Option<Catalog>>,
    closed: Watchable<bool>,
) {
    let mut generation = 0u64;
    let mut next = first;
    loop {
        let consumer = match next.take() {
            Some(consumer) => consumer,
            None => match &origin {
                Origin::Moq => break,
                Origin::Routed { origin, path } => {
                    // The first resolution waits as long as it takes, since the
                    // publisher may not have announced yet. Later ones are a
                    // failover, which the next route answers at once.
                    let resolved = match generation {
                        0 => origin.routed_broadcast(path).await,
                        _ => match tokio::time::timeout(
                            REROUTE_PATIENCE,
                            origin.routed_broadcast(path),
                        )
                        .await
                        {
                            Ok(resolved) => resolved,
                            Err(_) => {
                                info!("no route serves the broadcast any more");
                                break;
                            }
                        },
                    };
                    match resolved {
                        Ok(consumer) => consumer,
                        Err(err) => {
                            info!(error = %err, "the broadcast cannot be resolved");
                            break;
                        }
                    }
                }
            },
        };
        generation += 1;
        if generation > 1 {
            info!(generation, "the broadcast came back through another route");
        }
        epoch
            .set(Epoch {
                generation,
                consumer: Some(consumer.clone()),
            })
            .ok();
        read_catalog(&consumer, &catalog).await;
        consumer.closed().await;
        debug!("the broadcast ended");
    }
    epoch
        .set(Epoch {
            generation: generation + 1,
            consumer: None,
        })
        .ok();
    closed.set(true).ok();
}

/// Reads catalog updates into `catalog` until the catalog track ends.
async fn read_catalog(
    consumer: &moq_net::broadcast::Consumer,
    catalog: &Watchable<Option<Catalog>>,
) {
    let mut reader = match moq_mux::catalog::Consumer::<()>::new(consumer, Default::default()).await
    {
        Ok(reader) => reader,
        Err(err) => {
            warn!(error = %err, "the catalog track could not be read");
            return;
        }
    };
    loop {
        match reader.next().await {
            Ok(Some(next)) => {
                // At trace, because a catalog is the first thing to look at when
                // a publisher and a subscriber disagree about what is on the wire.
                if tracing::enabled!(tracing::Level::TRACE)
                    && let Ok(json) = serde_json::to_string(&next)
                {
                    trace!(catalog = %json, "catalog");
                }
                let next: HangCatalog = next;
                catalog.set(Some(Catalog::new(next))).ok();
            }
            Ok(None) => {
                debug!("catalog track ended");
                return;
            }
            Err(err) => {
                warn!(error = %err, "catalog track failed");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_closed_local_broadcast_closes_its_remote() {
        let broadcast = LocalBroadcast::new();
        let remote = RemoteBroadcast::local(&broadcast);
        broadcast.close();
        tokio::time::timeout(Duration::from_secs(5), remote.closed())
            .await
            .expect("the remote saw the close");
        assert!(remote.is_closed());
    }

    /// A broadcast seeded with the consumer its caller resolved closes once
    /// the publisher goes, rather than waiting forever for a first route.
    #[tokio::test]
    async fn a_resolved_broadcast_closes_once_its_route_is_gone() {
        let (origin, driver) = moq_net::origin::Producer::new(Default::default());
        let _driver = AbortOnDropHandle::new(n0_future::task::spawn(moq_net::time::run(driver)));
        let published = origin
            .publish("live/cam", moq_net::origin::Route::default())
            .expect("published");
        let consumer = origin
            .consume()
            .routed_broadcast("live/cam")
            .await
            .expect("resolved");
        // Gone before the remote broadcast even starts.
        drop(published);
        let remote = RemoteBroadcast::from_resolved(origin.consume(), "live/cam", consumer);
        tokio::time::timeout(REROUTE_PATIENCE + Duration::from_secs(5), remote.closed())
            .await
            .expect("the remote closed within its re-resolve window");
    }

    /// A route change ends the broadcast it served; the remote asks again and
    /// players see a new epoch rather than an end.
    #[tokio::test]
    async fn a_routed_broadcast_survives_a_change_of_route() {
        let (origin, driver) = moq_net::origin::Producer::new(Default::default());
        let _driver = AbortOnDropHandle::new(n0_future::task::spawn(moq_net::time::run(driver)));
        let first = origin
            .publish("live/cam", moq_net::origin::Route::default())
            .expect("published");
        let remote = RemoteBroadcast::from_origin(origin.consume(), "live/cam");
        let mut epoch = remote.epoch();
        let seen = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if epoch.get().consumer.is_some() {
                    return epoch.get().generation;
                }
                epoch.updated().await.expect("epochs keep coming");
            }
        })
        .await
        .expect("the broadcast resolved");

        // The first publication goes, and another takes the path.
        drop(first);
        let _second = origin
            .publish("live/cam", moq_net::origin::Route::default())
            .expect("published again");
        let next = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = epoch.get();
                if current.generation > seen && current.consumer.is_some() {
                    return current.generation;
                }
                epoch.updated().await.expect("epochs keep coming");
            }
        })
        .await
        .expect("the broadcast was requested again");
        assert!(next > seen);
        assert!(!remote.is_closed());
    }
}
