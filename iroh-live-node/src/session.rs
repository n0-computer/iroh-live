//! Direct sessions with peers: the handle an application holds.

use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use iroh::{EndpointId, endpoint::Connection};
use moq_net::Path;
use n0_error::e;
use tracing::info;

use crate::{
    Direction, Error, Grant, LinkId, SessionRequest, Subscription, handshake::Hop,
    link::LatestSample, node::Shared, route,
};

/// A direct session with a peer.
///
/// Cheap to clone. Dropping the handles does not close it: it ends when either
/// side closes it, the connection fails, or the node shuts down.
///
/// # Examples
///
/// ```no_run
/// # async fn run(node: iroh_live_node::Node, peer: iroh::EndpointId)
/// # -> Result<(), iroh_live_node::Error> {
/// let session = node.connect(peer).await?;
/// // Resolves through this session only, never through a relay.
/// let subscription = session.subscribe("live/camera").await?;
/// # drop(subscription);
/// session.close(moq_net::Error::Cancel);
/// let reason = session.closed().await;
/// # drop(reason);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, derive_more::Debug)]
#[debug("Session({}, {}, {:?})", inner.peer.fmt_short(), inner.link, inner.direction)]
pub struct Session {
    pub(crate) inner: Arc<SessionInner>,
}

pub(crate) struct SessionInner {
    pub(crate) link: LinkId,
    pub(crate) peer: EndpointId,
    pub(crate) direction: Direction,
    pub(crate) request: SessionRequest,
    pub(crate) connection: Connection,
    pub(crate) moq: moq_net::Session,
    /// The identity the peer's routes are attributed to in the route table.
    pub(crate) hop: Hop,
    pub(crate) latest_sample: LatestSample,
    pub(crate) shared: Weak<Shared>,
    /// Set by [`Session::close`], so a connect right after it dials anew.
    pub(crate) closing: AtomicBool,
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Session {}

impl Session {
    /// Returns the peer's endpoint id.
    #[must_use]
    pub fn peer(&self) -> EndpointId {
        self.inner.peer
    }

    /// Returns which side opened the session.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.inner.direction
    }

    /// Returns what the peer may do on this session.
    ///
    /// That is the grant the policy decided for this session, widened by the
    /// paths that live ticket subscriptions claimed for the peer (see
    /// [`Node::claim_path`](crate::Node::claim_path)). It is
    /// [`Grant::nothing`] after a rejection, once the authorization expired,
    /// and once the session has ended.
    #[must_use]
    pub fn grant(&self) -> Grant {
        self.inner
            .shared
            .upgrade()
            .map(|shared| {
                let state = shared.state.lock().expect("poisoned");
                state.session_grant(self.inner.link)
            })
            .unwrap_or_else(Grant::nothing)
    }

    /// Returns what the peer asked for when it opened the session.
    ///
    /// For a dialed session, the path this node requested, without a token.
    #[must_use]
    pub fn request(&self) -> &SessionRequest {
        &self.inner.request
    }

    /// Resolves `path` through this session's peer, and not through a relay.
    ///
    /// Waits for the peer to announce the path, then pins the subscription to
    /// the stream the peer announced: moq may move it to another route of
    /// that stream, but never to another stream at the path.
    ///
    /// The session is told from other links by the hop its routes carry: the
    /// one derived from the peer's id, or, for a peer that declares a hop of
    /// its own (as moq-relay does), one no other link carries. This node
    /// cannot read the declaration on a session it dialed, so the guarantee
    /// is only as strong as the grants: a peer whose grant lets it publish
    /// the path could declare this peer's hop and be resolved here.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::NotGranted`] if the session's grant does not let
    /// the peer publish the path, [`Error::NotAnnounced`] if the session ends
    /// before the peer announces it, [`Error::Unresolved`] if the route table
    /// refuses the path, and [`Error::ShutDown`] once the node is gone.
    pub async fn subscribe(
        &self,
        path: impl TryInto<Path, Error: Into<moq_net::Error>>,
    ) -> Result<Subscription, Error> {
        let path = crate::error::to_path(path)?;
        let grant = self.grant();
        if !grant.allows_publish(path.as_str()) {
            return Err(Error::not_granted(path, self.inner.peer, &grant));
        }
        let Some(shared) = self.inner.shared.upgrade() else {
            return Err(e!(Error::ShutDown));
        };
        let view = route::direct_view(&shared.table);
        // A hop no other link carries is taken to be the peer's own declaration.
        let own = self.inner.hop;
        let link = self.inner.link;
        let ours = |hop: Hop| {
            hop == own
                || shared
                    .state
                    .lock()
                    .expect("poisoned")
                    .link_with_hop(hop)
                    .is_none_or(|other| other == link)
        };
        let instance = tokio::select! {
            found = route::announced_by(&view, &path, ours) => match found {
                Ok(instance) => instance,
                Err(moq_net::Error::Closed) => return Err(e!(Error::NotAnnounced { path })),
                Err(source) => return Err(e!(Error::Unresolved { path, source })),
            },
            _ = self.inner.moq.closed() => return Err(e!(Error::NotAnnounced { path })),
        };
        let broadcast = match view.request(&path, instance.clone()).await {
            Ok(broadcast) => broadcast,
            Err(moq_net::Error::Closed) => return Err(e!(Error::NotAnnounced { path })),
            Err(source) => return Err(e!(Error::Unresolved { path, source })),
        };
        Ok(Subscription::new(
            path,
            view,
            broadcast,
            Some((self.inner.link, instance)),
            self.inner.shared.clone(),
        ))
    }

    /// Returns the iroh connection under the session.
    ///
    /// Its paths are iroh's: [`Connection::paths`] lists the open ones, with
    /// each one's remote address, whether it is selected, and its RTT, and
    /// [`Connection::paths_stream`] yields the list again whenever it
    /// changes. [`Connection::stats`] holds the counters of the whole
    /// connection.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn run(session: iroh_live_node::Session) {
    /// for path in session.connection().paths().iter() {
    ///     println!(
    ///         "{:?} selected={} rtt={:?}",
    ///         path.remote_addr(),
    ///         path.is_selected(),
    ///         path.rtt()
    ///     );
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.inner.connection
    }

    /// Returns the moq-net session, for its statistics, bandwidth estimates and announcements.
    ///
    /// Closing it closes this session, as [`close`](Self::close) does.
    #[must_use]
    pub fn moq(&self) -> &moq_net::Session {
        &self.inner.moq
    }

    /// Closes the session, and every subscription over it, in both directions.
    ///
    /// The peer receives `code` as the session's close code, and its
    /// [`closed`](Self::closed) reports it as a [`moq_net::Error::Session`]
    /// source, for example `Session(SessionError::App(7))` for `App(7)`.
    pub fn close(&self, code: moq_net::Error) {
        info!(peer = %self.inner.peer.fmt_short(), %code, "closing session");
        self.inner.closing.store(true, Ordering::Release);
        self.inner.moq.abort(code);
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
