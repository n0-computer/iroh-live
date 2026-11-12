//! Remote broadcasts: a catalog and a subscription, not yet playing.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use hang::catalog::Catalog;
use moq_ext_sources::Error;
use moq_mux::catalog::Stream as _;
use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, info, info_span, trace, warn};

use crate::{
    NetworkSample, Player, PlayerConfig, clean_close::CleanClose as _, network::NetworkSignals,
};

/// How long to look for an ended broadcast again before it counts as gone.
///
/// After a change of route, the next request finds the broadcast at once. A
/// longer wait means the publisher left.
const REROUTE_PATIENCE: Duration = Duration::from_secs(3);

/// The first wait before asking again for a catalog track that is not there yet.
///
/// Doubles per attempt, up to [`CATALOG_RETRY_MAX`].
const CATALOG_RETRY: Duration = Duration::from_millis(50);

/// The longest wait between two requests for a catalog track that is not there yet.
const CATALOG_RETRY_MAX: Duration = Duration::from_secs(2);

/// The broadcast consumer a player reads, and how many times it has changed.
#[derive(derive_more::Debug, Clone, Default)]
pub(crate) struct Epoch {
    /// Counts the consumers handed out so far, so a reader sees a new route.
    pub(crate) generation: u64,
    /// The consumer to read, or `None` while waiting for a route and after the end.
    #[debug(skip)]
    pub(crate) consumer: Option<moq_net::broadcast::Consumer>,
}

/// Where a remote broadcast comes from.
#[derive(Clone)]
enum Origin {
    /// One broadcast consumer, which ends the remote broadcast when it ends.
    Consumer,
    /// A path in a route table, requested again when a change of route ends it.
    Routed {
        /// Boxed: a route table consumer is large.
        origin: Box<moq_net::origin::Consumer>,
        path: moq_net::Path,
    },
}

/// The state every clone of a [`RemoteBroadcast`] shares.
#[derive(derive_more::Debug)]
struct Shared {
    #[debug(skip)]
    #[cfg_attr(
        not(feature = "record"),
        allow(dead_code, reason = "only a moq-mux source asks for the route table")
    )]
    origin: Origin,
    /// The empty route table of a moq-mux source over a bare consumer.
    #[cfg(feature = "record")]
    #[debug(skip)]
    local: std::sync::OnceLock<LocalOrigin>,
    #[debug("{:?}", epoch.borrow())]
    epoch: watch::Sender<Epoch>,
    #[debug(skip)]
    catalog: watch::Sender<Option<Arc<Catalog>>>,
    closed: CancellationToken,
    #[debug(skip)]
    network: Mutex<Option<NetworkSignals>>,
    span: Span,
    #[debug(skip)]
    _task: AbortOnDropHandle<()>,
}

/// A broadcast being read.
///
/// Cheap to clone. Holding it keeps the broadcast subscription open, and any
/// number of [`Player`]s and recordings can read it. Constructing one spawns a
/// task, so it must happen inside a Tokio runtime.
#[derive(Debug, Clone)]
pub struct RemoteBroadcast {
    shared: Arc<Shared>,
}

impl RemoteBroadcast {
    /// Starts reading the catalog of `broadcast`.
    ///
    /// The remote broadcast ends when `broadcast` ends. This reads a broadcast
    /// that lives in the same process too: pass it
    /// `moq_ext_publisher::LocalBroadcast::consume`.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn from_consumer(broadcast: moq_net::broadcast::Consumer) -> Self {
        let span = info_span!("remote");
        Self::spawn(Origin::Consumer, Some(broadcast), span)
    }

    /// Follows `path` in a route table, starting from a resolved `broadcast`.
    ///
    /// When a change of route ends the broadcast, it is requested again through
    /// the next route, and players see a switch instead of an end.
    /// [`closed`](Self::closed) explains when a publisher counts as gone.
    /// `iroh-live` uses this on subscribe.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn from_resolved(
        origin: moq_net::origin::Consumer,
        path: moq_net::Path,
        broadcast: moq_net::broadcast::Consumer,
    ) -> Self {
        Self::routed_to(origin, path, Some(broadcast))
    }

    /// Follows `path` in a route table, from `first` if given.
    ///
    /// Without `first`, the first request waits without limit.
    fn routed_to(
        origin: moq_net::origin::Consumer,
        path: moq_net::Path,
        first: Option<moq_net::broadcast::Consumer>,
    ) -> Self {
        let span = info_span!("remote", path = %path);
        Self::spawn(
            Origin::Routed {
                origin: Box::new(origin),
                path,
            },
            first,
            span,
        )
    }

    /// Attaches network signals for automatic rendition selection.
    ///
    /// `signals` returns the link as it is now. Players call it a few times a
    /// second and on every [`Player::stats`], so it must not block. The
    /// signals belong to the broadcast: every clone sees them, and attaching
    /// again replaces them. Players started afterwards use them.
    pub fn set_network(&self, signals: impl Fn() -> NetworkSample + Send + Sync + 'static) {
        *self.shared.network.lock().expect("poisoned") = Some(Arc::new(signals));
    }

    /// Returns a receiver for the catalog, `None` until the first arrives.
    ///
    /// A publisher may announce its broadcast before it creates the catalog
    /// track. The catalog is then asked for again, with a growing wait, until
    /// it appears or the broadcast closes.
    ///
    /// A borrow of the receiver blocks the next catalog update, so keep it
    /// short.
    #[must_use]
    pub fn catalog(&self) -> watch::Receiver<Option<Arc<Catalog>>> {
        self.shared.catalog.subscribe()
    }

    /// Waits for the first catalog, at most `timeout`.
    ///
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Closed`] if the broadcast closes first, and with
    /// [`Error::Catalog`] if no catalog this build can read arrives in time.
    pub async fn catalog_within(&self, timeout: Duration) -> Result<Arc<Catalog>, Error> {
        let mut catalog = self.catalog();
        let first = async {
            // A broadcast that closes sends no catalog update to wake on.
            tokio::select! {
                catalog = catalog.wait_for(Option::is_some) => catalog.ok().and_then(|catalog| catalog.clone()),
                () = self.closed() => None,
            }
        };
        match tokio::time::timeout(timeout, first).await {
            Ok(Some(catalog)) => Ok(catalog),
            Ok(None) => Err(n0_error::e!(Error::Closed)),
            Err(_) => Err(Error::catalog_msg(format!(
                "the broadcast sent no catalog within {}s, or sent one this build could not read",
                timeout.as_secs()
            ))),
        }
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

    /// Returns a [`moq_mux::Source`] of the current route, for a moq-mux export.
    ///
    /// Waits until the broadcast has a route and a catalog, since an exporter's
    /// catalog reader does not ask again for a catalog track that is not there
    /// yet. The source reads one route, so a change of route ends the exports
    /// that read it. One of a broadcast from [`from_resolved`](Self::from_resolved)
    /// resolves sibling broadcasts through the route table.
    ///
    /// Recording composes this with moq-mux's fragmented MP4 or Matroska
    /// export, which remuxes without decoding:
    ///
    /// ```no_run
    /// # async fn example(
    /// #     broadcast: moq_ext_player::RemoteBroadcast,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// use moq_mux::{catalog::Stream as _, container::fmp4};
    /// use tokio::io::AsyncWriteExt as _;
    ///
    /// let source = broadcast.mux_source().await?;
    /// let catalog = moq_mux::catalog::Consumer::<()>::new(&source.broadcast(), Default::default())
    ///     .await?
    ///     .select(moq_mux::select::Broadcast::default());
    /// let mut export = fmp4::Export::new(source, catalog);
    /// let mut file = tokio::fs::File::create("out.mp4").await?;
    /// while let Some(chunk) = export.next().await? {
    ///     file.write_all(&chunk).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Closed`] if the broadcast closes first.
    #[cfg(feature = "record")]
    pub async fn mux_source(&self) -> Result<moq_mux::Source, Error> {
        // The broadcast held here keeps the watch senders alive, so only
        // `closed` ends a wait on a broadcast that is gone.
        let mut epoch = self.epoch();
        let consumer = tokio::select! {
            epoch = epoch.wait_for(|epoch| epoch.consumer.is_some()) => {
                epoch.ok().and_then(|epoch| epoch.consumer.clone())
            }
            () = self.closed() => None,
        };
        let consumer = consumer.ok_or_else(|| n0_error::e!(Error::Closed))?;
        let mut catalog = self.catalog();
        let known = tokio::select! {
            seen = catalog.wait_for(Option::is_some) => seen.is_ok(),
            () = self.closed() => false,
        };
        if !known {
            return Err(n0_error::e!(Error::Closed));
        }
        let origin = match &self.shared.origin {
            Origin::Routed { origin, .. } => origin.as_ref().clone(),
            Origin::Consumer => self.shared.local.get_or_init(LocalOrigin::new).consume(),
        };
        Ok(moq_mux::Source::new(origin, consumer))
    }

    /// Waits until the broadcast has closed.
    ///
    /// A broadcast read with [`from_consumer`](Self::from_consumer) closes when
    /// its consumer does. One that follows a route table, from
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
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.closed.is_cancelled()
    }

    /// Returns a receiver for the consumer players read, and its generation.
    pub(crate) fn epoch(&self) -> watch::Receiver<Epoch> {
        self.shared.epoch.subscribe()
    }

    /// Returns the network signals, if a transport attached any.
    pub(crate) fn network(&self) -> Option<NetworkSignals> {
        self.shared.network.lock().expect("poisoned").clone()
    }

    /// Returns the span everything reading this broadcast logs under.
    pub(crate) fn span(&self) -> &Span {
        &self.shared.span
    }

    fn spawn(origin: Origin, first: Option<moq_net::broadcast::Consumer>, span: Span) -> Self {
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
                #[cfg(feature = "record")]
                local: std::sync::OnceLock::new(),
                network: Mutex::new(None),
                span,
                _task: AbortOnDropHandle::new(task),
            }),
        }
    }
}

/// An empty route table with the task that drives it.
///
/// A moq-mux source resolves sibling broadcasts through a route table, and a
/// bare consumer has none.
#[cfg(feature = "record")]
struct LocalOrigin {
    origin: moq_net::origin::Producer,
    _driver: AbortOnDropHandle<()>,
}

#[cfg(feature = "record")]
impl LocalOrigin {
    fn new() -> Self {
        let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
        let driver = n0_future::task::spawn(async move {
            let err = moq_net::time::run(driver).await;
            debug!(error = %err, "the moq-mux source's route table stopped");
        });
        Self {
            origin,
            _driver: AbortOnDropHandle::new(driver),
        }
    }

    fn consume(&self) -> moq_net::origin::Consumer {
        self.origin.consume()
    }
}

/// Reads the broadcast, requesting it again when a change of route ends it.
async fn follow(
    origin: Origin,
    first: Option<moq_net::broadcast::Consumer>,
    epoch: watch::Sender<Epoch>,
    catalog: watch::Sender<Option<Arc<Catalog>>>,
    closed: CancellationToken,
) {
    let mut generation = 0u64;
    let mut next = first;
    let mut routes = match &origin {
        Origin::Consumer => None,
        Origin::Routed { origin, path } => match origin.follow(path.clone(), None) {
            Ok(routes) => Some(routes),
            Err(err) => {
                info!(error = %err, "the broadcast cannot be followed");
                None
            }
        },
    };
    if let (Some(_), Some(routes)) = (&next, &mut routes) {
        // The caller resolved the first broadcast already, and the table's first
        // answer names it again. Take that answer here, so it does not count as
        // a change of route.
        let _ = tokio::time::timeout(REROUTE_PATIENCE, routes.available()).await;
    }
    loop {
        let consumer = match next.take() {
            Some(consumer) => consumer,
            None => match &mut routes {
                None => break,
                Some(routes) => {
                    // The publisher may not have announced yet, so only a
                    // failover has a time limit.
                    let resolved = match generation {
                        0 => routes.available().await,
                        _ => match tokio::time::timeout(REROUTE_PATIENCE, routes.available()).await
                        {
                            Ok(resolved) => resolved,
                            Err(_) => {
                                info!("no route serves the broadcast any more");
                                break;
                            }
                        },
                    };
                    match resolved {
                        Ok(Some(consumer)) => consumer,
                        // The path went offline: wait for whatever serves it next.
                        Ok(None) => continue,
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
        // When another instance takes the path over, this broadcast stays open
        // until its own routes go, and its stream may already be dead to the
        // publisher. Switch as soon as the table names the new instance. An error,
        // or `None` for a path that went offline, disables this arm, and the
        // broadcast's own end drives the loop.
        let taken_over = async {
            match &mut routes {
                Some(routes) => routes.available().await.ok().flatten(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(taken) = taken_over => {
                debug!("another instance took the broadcast over");
                next = Some(taken);
            }
            _ = async {
                read_catalog(&consumer, &catalog).await;
                consumer.closed().await;
            } => debug!("the broadcast ended"),
        }
    }
    epoch.send_replace(Epoch {
        generation: generation + 1,
        consumer: None,
    });
    closed.cancel();
}

/// Reads catalog updates into `catalog` until the catalog track ends.
///
/// Asks again for a catalog track that is not there yet, as
/// [`RemoteBroadcast::catalog`] describes. Cancellation safe.
async fn read_catalog(
    consumer: &moq_net::broadcast::Consumer,
    catalog: &watch::Sender<Option<Arc<Catalog>>>,
) {
    let mut retry = CATALOG_RETRY;
    let mut reader = loop {
        match moq_mux::catalog::Consumer::<()>::new(consumer, Default::default()).await {
            Ok(reader) => break reader,
            // A local origin answers `NotFound`. A peer's answer arrives as a
            // stream error.
            Err(moq_mux::Error::Moq(
                moq_net::Error::NotFound | moq_net::Error::Stream(moq_net::StreamError::NotFound),
            )) => {
                debug!(?retry, "no catalog track yet, asking again");
                tokio::select! {
                    () = tokio::time::sleep(retry) => {}
                    () = consumer.closed() => {
                        debug!("the broadcast closed before its catalog appeared");
                        return;
                    }
                }
                retry = (retry * 2).min(CATALOG_RETRY_MAX);
            }
            Err(err) if err.is_clean_close() => {
                debug!(error = %err, "the catalog track closed before it was read");
                return;
            }
            Err(err) => {
                warn!(error = %err, "the catalog track could not be read");
                return;
            }
        }
    };
    loop {
        match reader.next().await {
            Ok(Some(next)) => {
                trace!(catalog = ?next, "catalog updated");
                catalog.send_replace(Some(Arc::new(next)));
            }
            Ok(None) => {
                debug!("catalog track ended");
                return;
            }
            Err(err) if err.is_clean_close() => {
                debug!(error = %err, "catalog track closed");
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
    use moq_net::{Instance, origin::RouteOptions};

    use super::*;

    /// A broadcast that closes before it has a catalog ends the wait for a moq-mux source.
    #[cfg(feature = "record")]
    #[tokio::test]
    async fn a_close_before_the_catalog_ends_the_mux_source_wait() {
        let producer = moq_net::broadcast::Info::new().produce();
        let remote = RemoteBroadcast::from_consumer(producer.consume());
        let source = remote.mux_source();
        let close = async {
            // Long enough for the source to wait on the catalog.
            tokio::time::sleep(Duration::from_millis(200)).await;
            producer.close();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(source, close)
        })
        .await
        .expect("the wait ended with the broadcast");
        assert!(matches!(result, Err(Error::Closed { .. })));
    }

    #[tokio::test]
    async fn a_catalog_wait_ends_on_timeout_and_on_close() {
        let producer = moq_net::broadcast::Info::new().produce();
        let remote = RemoteBroadcast::from_consumer(producer.consume());
        let waited = remote.catalog_within(Duration::from_millis(100)).await;
        assert!(matches!(waited, Err(Error::Catalog { .. })), "{waited:?}");
        drop(producer);
        let closed = remote.catalog_within(Duration::from_secs(10)).await;
        assert!(matches!(closed, Err(Error::Closed { .. })), "{closed:?}");
    }

    /// A broadcast that describes itself ends the wait with its catalog.
    #[tokio::test]
    async fn a_catalog_wait_returns_the_first_catalog() {
        let broadcast = moq_ext_publisher::LocalBroadcast::new();
        let remote = RemoteBroadcast::from_consumer(broadcast.consume());
        broadcast
            .set_video(
                moq_ext_sources::VideoSource::test_pattern(
                    moq_video::Size::new(320, 180),
                    moq_video::FrameRate::new(30, 1).expect("a valid rate"),
                )
                .expect("the pattern thread starts"),
                crate::test_util::single(None),
            )
            .expect("a valid encoding");
        let catalog = remote
            .catalog_within(Duration::from_secs(10))
            .await
            .expect("a catalog");
        assert!(
            catalog.video.renditions.contains_key("video"),
            "{catalog:?}"
        );
        // A later wait returns at once with what the remote holds.
        let again = remote
            .catalog_within(Duration::from_millis(1))
            .await
            .expect("the catalog is held");
        assert!(again.video.renditions.contains_key("video"));
    }

    /// A catalog track created after the broadcast is read is still found.
    #[tokio::test]
    async fn a_catalog_created_late_is_read() {
        let producer = moq_net::broadcast::Info::new().produce();
        let remote = RemoteBroadcast::from_consumer(producer.consume());
        // Long enough for the first request to find no catalog.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(remote.catalog().borrow().is_none());

        let broadcast =
            moq_ext_publisher::LocalBroadcast::new_in(producer).expect("no catalog yet");
        // An empty catalog is written once there is media to describe.
        broadcast
            .set_video(
                moq_ext_sources::VideoSource::test_pattern(
                    moq_video::Size::new(320, 180),
                    moq_video::FrameRate::new(30, 1).expect("a valid rate"),
                )
                .expect("the pattern thread starts"),
                crate::test_util::single(None),
            )
            .expect("a valid encoding");
        let mut catalog = remote.catalog();
        tokio::time::timeout(Duration::from_secs(5), catalog.wait_for(Option::is_some))
            .await
            .expect("the late catalog was read")
            .expect("the remote is alive");
    }

    /// A broadcast that closes while its catalog is awaited closes its remote.
    #[tokio::test]
    async fn a_broadcast_closed_before_its_catalog_closes_its_remote() {
        let producer = moq_net::broadcast::Info::new().produce();
        let remote = RemoteBroadcast::from_consumer(producer.consume());
        tokio::time::sleep(Duration::from_millis(200)).await;
        producer.close();
        tokio::time::timeout(Duration::from_secs(5), remote.closed())
            .await
            .expect("the remote saw the close");
    }

    #[tokio::test]
    async fn a_closed_local_broadcast_closes_its_remote() {
        let broadcast = moq_ext_publisher::LocalBroadcast::new();
        let remote = RemoteBroadcast::from_consumer(broadcast.consume());
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
            .route("live/cam", RouteOptions::default())
            .expect("published");
        let consumer = origin
            .consume()
            .request("live/cam", None)
            .await
            .expect("resolved");
        // The publisher leaves before the remote broadcast starts.
        drop(published);
        let remote = RemoteBroadcast::from_resolved(
            origin.consume(),
            moq_net::Path::new("live/cam").expect("a valid path"),
            consumer,
        );
        tokio::time::timeout(REROUTE_PATIENCE + Duration::from_secs(5), remote.closed())
            .await
            .expect("the remote closed within its re-resolve window");
    }

    /// A new instance taking the path over switches players at once.
    ///
    /// The old instance still serves its readers meanwhile.
    #[tokio::test]
    async fn a_takeover_switches_while_the_old_instance_lives() {
        let (origin, driver) = moq_net::origin::Producer::new(Default::default());
        let _driver = AbortOnDropHandle::new(n0_future::task::spawn(moq_net::time::run(driver)));
        let _first = origin
            .route(
                "live/cam",
                RouteOptions::default().with_instance(Instance::random(), 1),
            )
            .expect("published");
        let remote = RemoteBroadcast::routed_to(
            origin.consume(),
            moq_net::Path::new("live/cam").expect("a valid path"),
            None,
        );
        let mut epoch = remote.epoch();
        let seen = tokio::time::timeout(
            Duration::from_secs(5),
            epoch.wait_for(|epoch| epoch.consumer.is_some()),
        )
        .await
        .expect("the broadcast resolved")
        .expect("epochs keep coming")
        .generation;

        let _second = origin
            .route(
                "live/cam",
                RouteOptions::default().with_instance(Instance::random(), 2),
            )
            .expect("published again");
        tokio::time::timeout(
            Duration::from_secs(5),
            epoch.wait_for(|epoch| epoch.generation > seen && epoch.consumer.is_some()),
        )
        .await
        .expect("players switched to the new instance")
        .expect("epochs keep coming");
        assert!(!remote.is_closed());
    }

    /// A change of route gives players a new epoch instead of a close.
    #[tokio::test]
    async fn a_routed_broadcast_survives_a_change_of_route() {
        let (origin, driver) = moq_net::origin::Producer::new(Default::default());
        let _driver = AbortOnDropHandle::new(n0_future::task::spawn(moq_net::time::run(driver)));
        let first = origin
            .route("live/cam", RouteOptions::default())
            .expect("published");
        let remote = RemoteBroadcast::routed_to(
            origin.consume(),
            moq_net::Path::new("live/cam").expect("a valid path"),
            None,
        );
        let mut epoch = remote.epoch();
        let seen = tokio::time::timeout(
            Duration::from_secs(5),
            epoch.wait_for(|epoch| epoch.consumer.is_some()),
        )
        .await
        .expect("the broadcast resolved")
        .expect("epochs keep coming")
        .generation;

        // The first route goes, and another takes the path.
        drop(first);
        let _second = origin
            .route("live/cam", RouteOptions::default())
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
