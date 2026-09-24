//! Sessions with direct peers: dialing, admitting, and what a session offers.

use std::{
    fmt,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use iroh::{EndpointAddr, EndpointId, endpoint::Connection};
use moq_net::{AsPath, origin, server::Handshake};
use n0_error::e;
use tracing::{info, warn};

use crate::{
    ConnectOptions, Error, Grant, LinkId, LinkKind, LinkSample, OfferGuard, Publication, Reject,
    SessionRequest, Subscription, link::LinkState, node::Shared, path::hop_for, transport,
};

/// The transport a MoQ session runs over.
///
/// web-transport-iroh implements the async transport interface, and moq-net
/// accepts only the poll one, so every session goes through moq-tokio's
/// adapter. It costs an allocation per operation and a copy per write; a
/// native poll implementation in web-transport-iroh would remove both.
pub(crate) type Transport = moq_tokio::transport::Session<web_transport_iroh::Session>;

/// Returns the instant a session's driver starts from.
///
/// From tokio's clock rather than `std`'s, because `moq_net::time::run` polls
/// the driver with tokio's, and a driver refuses time that moves backwards.
/// Under `tokio::time::pause` the two diverge, and seeding from `std` puts the
/// first poll behind the seed.
pub(crate) fn driver_now() -> std::time::Instant {
    tokio::time::Instant::now().into_std()
}

/// One session with one peer.
///
/// Cheap to clone; dropping the handles does not close it. It ends when either
/// side closes it, the connection fails, or the node shuts down.
#[derive(Clone)]
pub struct Session {
    pub(crate) inner: Arc<SessionInner>,
}

pub(crate) struct SessionInner {
    pub(crate) link: u64,
    pub(crate) remote: EndpointId,
    pub(crate) dialed: bool,
    pub(crate) grant: Grant,
    pub(crate) request: SessionRequest,
    pub(crate) connection: Connection,
    pub(crate) moq: moq_net::Session,
    pub(crate) ingest: origin::Producer,
    pub(crate) link_state: LinkState,
    pub(crate) shared: Weak<Shared>,
    /// Set by [`Session::close`], which closes asynchronously, so that a
    /// connect right after it dials anew rather than getting this session.
    pub(crate) closing: AtomicBool,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("remote", &self.inner.remote.fmt_short().to_string())
            .field("link", &self.inner.link)
            .field("dialed", &self.inner.dialed)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Session {}

impl Session {
    /// Returns the peer's endpoint id.
    pub fn remote_id(&self) -> EndpointId {
        self.inner.remote
    }

    /// Returns the id of the link this session is.
    ///
    /// The [`RouteInfo::via`](crate::RouteInfo::via) of every route that
    /// arrived over it, so a route can be matched to its session. Two sessions
    /// with one peer, one after the other, have different ids.
    pub fn link_id(&self) -> LinkId {
        LinkId(self.inner.link)
    }

    /// Returns what kind of link the session is.
    ///
    /// Always [`LinkKind::Direct`], since relays are
    /// [`RelayLink`](crate::RelayLink)s.
    pub fn kind(&self) -> LinkKind {
        LinkKind::Direct
    }

    /// Reports whether this node dialed the session, rather than accepted it.
    ///
    /// Two peers that dial each other at once end up with one session of each
    /// kind, so this is what tells two sessions with one peer apart.
    pub fn dialed(&self) -> bool {
        self.inner.dialed
    }

    /// Returns what the peer may do on this node.
    pub fn grant(&self) -> Grant {
        self.inner.grant.clone()
    }

    /// Returns what the peer asked for when it opened the session.
    ///
    /// Empty for a session this node dialed.
    pub fn request(&self) -> &SessionRequest {
        &self.inner.request
    }

    /// Offers `publication` on this session, within its grant.
    ///
    /// Dropping the guard withdraws the offer, unless the publication's
    /// audience admits this peer anyway.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotGranted`] if the session's grant does not cover
    /// the publication's path, and with [`Error::ShutDown`] once the node has
    /// shut down.
    pub fn offer(&self, publication: &Publication) -> Result<OfferGuard, Error> {
        let shared = self
            .inner
            .shared
            .upgrade()
            .ok_or_else(|| e!(Error::ShutDown))?;
        if !self
            .inner
            .grant
            .allows_subscribe(publication.path().as_str())
        {
            return Err(e!(Error::NotGranted {
                path: publication.path().to_owned()
            }));
        }
        Ok(OfferGuard::new(&shared, publication.id(), self.inner.link))
    }

    /// Resolves `path` through this session only, whatever the route table holds.
    ///
    /// For a path that means something on this session alone. Waits for the
    /// peer to announce the path; cancellation safe.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotAnnounced`] if the session ends before the peer
    /// announces the path.
    pub async fn subscribe(&self, path: impl AsPath) -> Result<Subscription, Error> {
        let path = path.as_path().to_owned();
        let ingest = self.inner.ingest.consume();
        let broadcast = tokio::select! {
            resolved = ingest.routed_broadcast(&path) => match resolved {
                Ok(broadcast) => broadcast,
                Err(moq_net::Error::Closed) => return Err(e!(Error::NotAnnounced { path })),
                Err(source) => return Err(e!(Error::Unresolved { path, source })),
            },
            _ = self.inner.moq.closed() => return Err(e!(Error::NotAnnounced { path })),
        };
        Ok(Subscription::new(
            path,
            ingest,
            broadcast,
            Some(self.inner.link),
            self.inner.shared.clone(),
        ))
    }

    /// Returns the link as this session's connection monitor last read it.
    pub fn link(&self) -> LinkSample {
        self.inner.link_state.get()
    }

    /// Returns the QUIC connection under the session.
    ///
    /// Integration point: follows iroh's versioning.
    pub fn connection(&self) -> &Connection {
        &self.inner.connection
    }

    /// Closes the session, and every subscription over it, in both directions.
    ///
    /// `reason` is logged here; the peer sees a clean close.
    pub fn close(&self, reason: &str) {
        info!(remote = %self.inner.remote.fmt_short(), reason, "closing session");
        self.inner.closing.store(true, Ordering::Release);
        self.inner.moq.abort(moq_net::Error::Cancel);
    }

    /// Reports whether the session is closed or on its way to closing.
    pub(crate) fn is_closing(&self) -> bool {
        self.inner.closing.load(Ordering::Acquire) || self.inner.connection.close_reason().is_some()
    }

    /// Waits until the session ends, and returns why.
    ///
    /// A close either side asked for is [`Error::SessionClosed`] with
    /// [`moq_net::Error::Cancel`] as its source. Cancellation safe.
    pub async fn closed(&self) -> Error {
        let source = self.inner.moq.closed().await;
        e!(Error::SessionClosed { source })
    }
}

/// An incoming session waiting for admission.
///
/// Yielded by [`Moq::accept`](crate::Moq::accept) under
/// [`Admission::Manual`](crate::Admission::Manual). Dropping it without
/// admitting rejects the session.
pub struct Incoming {
    pub(crate) remote: EndpointId,
    pub(crate) request: SessionRequest,
    pub(crate) connection: Connection,
    pub(crate) handshake: Handshake<Transport>,
    pub(crate) shared: Weak<Shared>,
    /// When the session was queued for admission, to reject stale ones.
    pub(crate) queued_at: tokio::time::Instant,
}

impl fmt::Debug for Incoming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Incoming")
            .field("remote", &self.remote.fmt_short().to_string())
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl Incoming {
    /// Returns the peer's endpoint id, which iroh has authenticated.
    pub fn remote_id(&self) -> EndpointId {
        self.remote
    }

    /// Returns what the peer asked for.
    pub fn request(&self) -> &SessionRequest {
        &self.request
    }

    /// Admits the session with `grant`.
    ///
    /// Cancellation safe: dropping the future before the handshake completes
    /// rejects the session; dropped after that, the session is admitted all
    /// the same and shows up in [`Moq::sessions`](crate::Moq::sessions).
    ///
    /// # Errors
    ///
    /// Fails if the MoQ handshake does not complete, or the node has shut down,
    /// in which case the peer is refused rather than admitted and closed.
    pub async fn admit(self, grant: Grant) -> Result<Session, Error> {
        let Some(shared) = self
            .shared
            .upgrade()
            .filter(|shared| !shared.shutdown.is_cancelled())
        else {
            self.close(moq_net::Error::Cancel);
            return Err(e!(Error::ShutDown));
        };
        info!(remote = %self.remote.fmt_short(), ?grant, "admitting session");
        let origins = Origins::new(&shared);
        let mut handshake = self
            .handshake
            .with_publisher(origins.publish.consume())
            .with_peer_hop(hop_for(&self.remote));
        if let Some(subscriber) = origins.subscriber(&grant) {
            handshake = handshake.with_subscriber(subscriber);
        }
        let (moq, driver) = handshake
            .ok()
            .await
            .map_err(|source| e!(Error::Moq { source }))?;
        let parts = SessionParts {
            remote: self.remote,
            connection: self.connection,
            dialed: false,
            grant,
            request: self.request,
            moq,
            driver,
            origins,
        };
        shared.register(parts).await
    }

    /// Rejects the session.
    pub fn reject(self, reason: Reject) {
        info!(remote = %self.remote.fmt_short(), ?reason, "rejecting session");
        self.handshake.close(reason.into());
    }

    /// Refuses the session with a moq error code.
    pub(crate) fn close(self, err: moq_net::Error) {
        self.handshake.close(err);
    }
}

/// A session's two origins: what its peer is offered, and what it announces.
pub(crate) struct Origins {
    pub(crate) publish: origin::Producer,
    pub(crate) publish_driver: origin::Driver,
    pub(crate) ingest: origin::Producer,
    pub(crate) ingest_driver: origin::Driver,
}

impl Origins {
    /// Creates both origins under the node's identity.
    pub(crate) fn new(shared: &Shared) -> Self {
        let (publish, publish_driver) = origin::Producer::new(shared.table.config());
        let (ingest, ingest_driver) = origin::Producer::new(shared.table.config());
        Self {
            publish,
            publish_driver,
            ingest,
            ingest_driver,
        }
    }

    /// Returns the ingest origin scoped to what `grant` lets the peer publish.
    ///
    /// `None` if it lets the peer publish nothing.
    pub(crate) fn subscriber(&self, grant: &Grant) -> Option<origin::Producer> {
        if grant.publish.is_empty() {
            return None;
        }
        match self.ingest.scope("", &grant.publish) {
            Ok(scoped) => Some(scoped),
            Err(err) => {
                warn!(%err, "the grant's publish patterns fit no path; ingesting nothing");
                None
            }
        }
    }
}

/// Everything a session needs to run, produced by a dial or an admission.
pub(crate) struct SessionParts {
    pub(crate) remote: EndpointId,
    pub(crate) connection: Connection,
    pub(crate) dialed: bool,
    pub(crate) grant: Grant,
    pub(crate) request: SessionRequest,
    pub(crate) moq: moq_net::Session,
    pub(crate) driver: moq_net::Driver<Transport>,
    pub(crate) origins: Origins,
}

/// Dials `remote` and completes the MoQ handshake as the client.
pub(crate) async fn dial_session(
    shared: Arc<Shared>,
    remote: EndpointAddr,
    options: ConnectOptions,
) -> Result<SessionParts, Error> {
    let remote_id = remote.id;
    let transport = transport::dial_with(&shared.endpoint, remote, &options).await?;
    let connection = transport.conn().clone();
    let origins = Origins::new(&shared);
    let mut client = moq_net::Client::new()
        .with_publisher(origins.publish.consume())
        .with_peer_hop(hop_for(&remote_id));
    if let Some(subscriber) = origins.subscriber(&options.grant) {
        client = client.with_subscriber(subscriber);
    }
    if let Some(cost) = options.cost {
        client = client.with_cost(cost);
    }
    // Raw QUIC carries the token in the setup path; an H3 session already put
    // it in its CONNECT URL and must not send one.
    if !transport.is_h3()
        && let Some(path) = options.setup_path()
    {
        client = client.with_path(path);
    }
    let (moq, driver) = client
        .connect(driver_now(), Transport::new(transport.session))
        .await
        .map_err(|source| match source {
            moq_net::Error::Unauthorized | moq_net::Error::App(_) => e!(Error::Refused { source }),
            source => e!(Error::Moq { source }),
        })?;
    Ok(SessionParts {
        remote: remote_id,
        connection,
        dialed: true,
        grant: options.grant,
        request: SessionRequest::default(),
        moq,
        driver,
        origins,
    })
}
