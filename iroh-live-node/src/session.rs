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
    Error, Grant, LinkId, LinkSample, SessionRequest, Subscription, handshake::Hop,
    link::LinkState, node::Shared, route,
};

/// A direct session with a peer.
///
/// Cheap to clone. Dropping the handles does not close it: it ends when either
/// side closes it, the connection fails, or the node shuts down.
#[derive(Clone, derive_more::Debug)]
#[debug("Session({}, {}, dialed {})", inner.remote.fmt_short(), inner.link, inner.dialed)]
pub struct Session {
    pub(crate) inner: Arc<SessionInner>,
}

pub(crate) struct SessionInner {
    pub(crate) link: LinkId,
    pub(crate) remote: EndpointId,
    pub(crate) dialed: bool,
    pub(crate) grant: std::sync::Mutex<Grant>,
    pub(crate) request: SessionRequest,
    pub(crate) connection: Connection,
    pub(crate) moq: moq_net::Session,
    /// The identity the peer's routes are attributed to in the route table.
    pub(crate) hop: Hop,
    pub(crate) link_state: LinkState,
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
    pub fn remote_id(&self) -> EndpointId {
        self.inner.remote
    }

    /// Reports whether this node dialed the session.
    pub fn dialed(&self) -> bool {
        self.inner.dialed
    }

    /// Returns what the peer may do on this node.
    ///
    /// That is the base grant, widened by the paths that live ticket
    /// subscriptions claimed for the peer (see [`Node::claim_path`](crate::Node::claim_path)).
    pub fn grant(&self) -> Grant {
        let Some(shared) = self.inner.shared.upgrade() else {
            return self.inner.grant.lock().expect("poisoned").clone();
        };
        let state = shared.state.lock().expect("poisoned");
        let base = self.inner.grant.lock().expect("poisoned");
        state.effective_grant(Some(&self.inner.remote), &base)
    }

    /// Returns the base grant: what [`set_grant`](Self::set_grant) last set.
    ///
    /// Without live ticket claims this equals [`grant`](Self::grant).
    pub fn base_grant(&self) -> Grant {
        self.inner.grant.lock().expect("poisoned").clone()
    }

    /// Changes the base grant of the peer, on the live session.
    ///
    /// moq enforces it at once, in both directions. A narrower subscribe half
    /// takes paths from the peer and resets what it reads there, and a
    /// narrower publish half takes the peer's broadcasts outside it out of the
    /// route table. A wider one brings them back, with no bound from the grant
    /// the session started with. Paths that live ticket claims added stay until
    /// their claims drop.
    pub fn set_grant(&self, grant: Grant) {
        let Some(shared) = self.inner.shared.upgrade() else {
            return;
        };
        let mut state = shared.state.lock().expect("poisoned");
        *self.inner.grant.lock().expect("poisoned") = grant.clone();
        state.set_grant(self.inner.link, grant);
    }

    /// Returns what the peer asked for when it opened the session.
    ///
    /// Empty for a session this node dialed.
    pub fn request(&self) -> &SessionRequest {
        &self.inner.request
    }

    /// Resolves `path` through this session's peer, and not through a relay.
    ///
    /// Waits for the peer to announce the path, then pins the subscription to
    /// the stream the peer announced: moq may move it to another route of
    /// that stream, but never to another stream at the path. A relay or a
    /// third peer that serves the same path under another instance is never
    /// resolved.
    ///
    /// The session is told from other links by the hop its routes carry: the
    /// one derived from the peer's id, or, for a peer that declares a hop of
    /// its own on the wire (as moq-relay does), one no other link carries.
    /// moq lets a declaration win over the hop this node assigns, and this
    /// node cannot read the declaration on a session it dialed. So the
    /// guarantee is only as strong as the grants: a peer whose grant lets it
    /// publish the path could declare this peer's hop, or a fresh one, and be
    /// resolved here. It holds when grants do not overlap, as the default
    /// grants ensure by letting each peer publish only under its own id. Two
    /// sessions with one peer share the hop.
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
            return Err(Error::not_granted(path, self.inner.remote, &grant));
        }
        let Some(shared) = self.inner.shared.upgrade() else {
            return Err(e!(Error::ShutDown));
        };
        let view = route::direct_view(&shared.table);
        // This peer's routes carry its derived hop, unless the peer declares
        // one of its own, as moq-relay does. A hop no other link carries is
        // taken to be such a declaration.
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

    /// Returns the connection monitor's latest reading of this session.
    pub fn link_sample(&self) -> LinkSample {
        self.inner.link_state.get()
    }

    /// Returns the QUIC connection under the session.
    pub fn connection(&self) -> &Connection {
        &self.inner.connection
    }

    /// Closes the session, and every subscription over it, in both directions.
    ///
    /// Logs `reason` locally. The peer sees a clean close.
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
