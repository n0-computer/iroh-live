//! Remote broadcasts: a catalog and a subscription, not yet playing.
//!
//! [`RemoteBroadcast`] reads a broadcast's catalog and keeps the subscription
//! open. Any number of [`Player`]s and [`Recording`]s can read it.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use moq_mux::catalog::Stream as _;
use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, trace, warn};

use crate::{
    Catalog, LocalBroadcast, NetworkSample, Player, PlayerConfig, RecordConfig, Recording,
    error::Error, network::NetworkSignals,
};

/// How long to look for an ended broadcast again before it counts as gone.
///
/// After a change of route, the next request finds the broadcast at once. A
/// longer wait means the publisher left. A deliberate hang-up also closes this
/// late, as [`RemoteBroadcast::closed`] documents.
const REROUTE_PATIENCE: Duration = Duration::from_secs(3);

/// The broadcast consumer a player reads, and how many times it has changed.
#[derive(derive_more::Debug, Clone, Default)]
pub(crate) struct Epoch {
    pub(crate) generation: u64,
    #[debug(skip)]
    pub(crate) consumer: Option<moq_net::broadcast::Consumer>,
}

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

#[derive(derive_more::Debug)]
struct Shared {
    #[debug(skip)]
    origin: Origin,
    #[debug("{:?}", epoch.borrow())]
    epoch: watch::Sender<Epoch>,
    #[debug(skip)]
    catalog: watch::Sender<Option<Catalog>>,
    closed: CancellationToken,
    #[debug(skip)]
    network: Mutex<Option<NetworkSignals>>,
    span: tracing::Span,
    #[debug(skip)]
    _task: AbortOnDropHandle<()>,
}

/// A broadcast being read.
///
/// Cheap to clone. Holding it keeps the broadcast subscription open, and the
/// players and recordings that read it subscribe to its tracks. Constructing
/// one spawns a task, so it must happen inside a Tokio runtime.
#[derive(Debug, Clone)]
pub struct RemoteBroadcast {
    shared: Arc<Shared>,
}

impl RemoteBroadcast {
    /// Starts reading the catalog of `broadcast`.
    ///
    /// The remote broadcast ends when `broadcast` ends.
    pub(crate) fn from_moq(broadcast: moq_net::broadcast::Consumer) -> Self {
        let span = tracing::info_span!("remote");
        Self::spawn(Origin::Moq, Some(broadcast), span)
    }

    /// Follows `path` in a route table.
    ///
    /// When a change of route ends the broadcast, it is requested again through
    /// the next route, and players see a switch instead of an end. The
    /// broadcast closes once no route serves the path, as
    /// [`closed`](Self::closed) explains.
    ///
    /// The first request waits without limit, since the publisher may not have
    /// announced the path yet. If you already resolved the path, use
    /// [`from_resolved`](Self::from_resolved).
    pub fn from_origin(origin: moq_net::origin::Consumer, path: impl moq_net::AsPath) -> Self {
        let path = path.as_path().to_owned();
        let span = tracing::info_span!("remote", path = %path);
        Self::spawn(Origin::Routed { origin, path }, None, span)
    }

    /// Follows `path` in a route table, starting from a resolved `broadcast`.
    ///
    /// Works like [`from_origin`](Self::from_origin), except that nothing
    /// waits for a first route. Every later request is a failover with a time
    /// limit, so a publisher that is already gone closes the broadcast. This is
    /// what `iroh-live` uses on subscribe, after resolving the path.
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

    /// Attaches network signals for automatic rendition selection.
    ///
    /// `signals` returns the link as it is now. Players call it a few times a
    /// second and on every [`Player::stats`], so it must not block. `iroh-live`
    /// attaches it on subscribe. The signals belong to the broadcast, not to
    /// this handle: every clone sees them, and attaching again replaces them
    /// for all clones. Players started afterwards use them.
    #[must_use]
    pub fn with_network(self, signals: impl Fn() -> NetworkSample + Send + Sync + 'static) -> Self {
        *self.shared.network.lock().expect("poisoned") = Some(Arc::new(signals));
        self
    }

    /// Returns the catalog, `None` until the first arrives.
    ///
    /// A borrow of the receiver blocks the next catalog update, so keep it
    /// short.
    pub fn catalog(&self) -> watch::Receiver<Option<Catalog>> {
        self.shared.catalog.subscribe()
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
    /// A broadcast read with [`local`](Self::local) closes when its local
    /// broadcast does. One that follows a route table, from
    /// [`from_origin`](Self::from_origin) or
    /// [`from_resolved`](Self::from_resolved), cannot tell a publisher that
    /// left from a change of route. It asks the table again and closes once no
    /// route has answered for three seconds. So a hang-up shows here about
    /// three seconds late.
    ///
    /// Cancellation safe.
    pub async fn closed(&self) {
        self.shared.closed.cancelled().await;
    }

    /// Reports whether the broadcast has closed.
    ///
    /// Lags a routed publisher's hang-up by the same three seconds as
    /// [`closed`](Self::closed).
    pub fn is_closed(&self) -> bool {
        self.shared.closed.is_cancelled()
    }

    /// Returns the consumer players read.
    pub(crate) fn epoch(&self) -> watch::Receiver<Epoch> {
        self.shared.epoch.subscribe()
    }

    /// Returns the network signals, if a transport attached any.
    pub(crate) fn network(&self) -> Option<NetworkSignals> {
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
        let epoch = watch::Sender::new(Epoch::default());
        let catalog = watch::Sender::new(None);
        let closed = CancellationToken::new();
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

/// Reads the broadcast, requesting it again when a change of route ends it.
async fn follow(
    origin: Origin,
    first: Option<moq_net::broadcast::Consumer>,
    epoch: watch::Sender<Epoch>,
    catalog: watch::Sender<Option<Catalog>>,
    closed: CancellationToken,
) {
    let mut generation = 0u64;
    let mut next = first;
    loop {
        let consumer = match next.take() {
            Some(consumer) => consumer,
            None => match &origin {
                Origin::Moq => break,
                Origin::Routed { origin, path } => {
                    // The publisher may not have announced yet, so only a
                    // failover has a time limit.
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
        epoch.send_replace(Epoch {
            generation,
            consumer: Some(consumer.clone()),
        });
        read_catalog(&consumer, &catalog).await;
        consumer.closed().await;
        debug!("the broadcast ended");
    }
    epoch.send_replace(Epoch {
        generation: generation + 1,
        consumer: None,
    });
    closed.cancel();
}

/// Reads catalog updates into `catalog` until the catalog track ends.
async fn read_catalog(
    consumer: &moq_net::broadcast::Consumer,
    catalog: &watch::Sender<Option<Catalog>>,
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
                trace!(catalog = ?next, "catalog");
                catalog.send_replace(Some(Catalog::from(next)));
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

    /// A resolved broadcast closes once its publisher is gone.
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
        // The publisher leaves before the remote broadcast starts.
        drop(published);
        let remote = RemoteBroadcast::from_resolved(origin.consume(), "live/cam", consumer);
        tokio::time::timeout(REROUTE_PATIENCE + Duration::from_secs(5), remote.closed())
            .await
            .expect("the remote closed within its re-resolve window");
    }

    /// A change of route gives players a new epoch instead of a close.
    #[tokio::test]
    async fn a_routed_broadcast_survives_a_change_of_route() {
        let (origin, driver) = moq_net::origin::Producer::new(Default::default());
        let _driver = AbortOnDropHandle::new(n0_future::task::spawn(moq_net::time::run(driver)));
        let first = origin
            .publish("live/cam", moq_net::origin::Route::default())
            .expect("published");
        let remote = RemoteBroadcast::from_origin(origin.consume(), "live/cam");
        let mut epoch = remote.epoch();
        let seen = tokio::time::timeout(
            Duration::from_secs(5),
            epoch.wait_for(|epoch| epoch.consumer.is_some()),
        )
        .await
        .expect("the broadcast resolved")
        .expect("epochs keep coming")
        .generation;

        // The first publication goes, and another takes the path.
        drop(first);
        let _second = origin
            .publish("live/cam", moq_net::origin::Route::default())
            .expect("published again");
        let next = tokio::time::timeout(
            Duration::from_secs(5),
            epoch.wait_for(|epoch| epoch.generation > seen && epoch.consumer.is_some()),
        )
        .await
        .expect("the broadcast was requested again")
        .expect("epochs keep coming")
        .generation;
        assert!(next > seen);
        assert!(!remote.is_closed());
    }
}
