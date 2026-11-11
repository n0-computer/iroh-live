//! Admission: who gets a session, and what it may do once it has one.

use std::sync::{Arc, Weak};

use iroh::{EndpointId, endpoint::Connection};
/// What a session may subscribe to from this node, and publish into it.
///
/// See [`moq_ext_access::Grant`].
pub use moq_ext_access::Grant;
/// What a peer asked for when it opened a session.
pub use moq_ext_access::SessionRequest;
use moq_transport_iroh::Congestion;
use n0_error::e;
use tracing::{debug, info};

use crate::{
    Error,
    handshake::{Hop, SessionParts, authorize_direct, subscriber},
    node::Shared,
    policy::{Authorization, Event, PeerRequest, Rejection},
    state::{Registered, State, served},
    transport::{self, HANDSHAKE_TIMEOUT},
};

/// How to dial a peer with [`Node::connect_with`](crate::Node::connect_with).
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// A token to present, sent in the MoQ SETUP and as `?jwt=` in the setup path.
    ///
    /// The accepting node reads it as [`SessionRequest::token`].
    pub token: Option<String>,
    /// The path the SETUP requests, without a query.
    ///
    /// The accepting node reads it as [`SessionRequest::path`]. `None` requests
    /// the root.
    pub path: Option<String>,
    /// The cost added to every route learned over this link.
    ///
    /// `None` keeps moq's default of one.
    pub cost: Option<u64>,
    /// The congestion controller of this connection.
    ///
    /// `None` keeps the endpoint's, which is BBR3 under
    /// [`MoqPreset`](crate::MoqPreset). It applies to a direct dial only: a
    /// session that already exists keeps its controller, so close it first to
    /// dial again with another. A relay link ignores it.
    pub congestion: Option<Congestion>,
}

impl ConnectOptions {
    /// Returns iroh's options for the QUIC dial.
    pub(crate) fn quic(&self) -> iroh::endpoint::ConnectOptions {
        let options = iroh::endpoint::ConnectOptions::new();
        match self.congestion {
            Some(congestion) => options.with_transport_config(congestion.transport_config()),
            None => options,
        }
    }
}

/// Admits one incoming connection if the policy does, for the protocol handler.
pub(crate) async fn accept(shared: &Arc<Shared>, connection: Connection) -> Result<(), Error> {
    if shared.shutdown.is_cancelled() {
        return Err(e!(Error::ShutDown));
    }
    let peer = connection.remote_id();
    let transport = transport::Transport::new(connection.clone())?;
    let opened = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        moq_net::Server::new().accept_request(moq_net::time::Instant::now(), transport),
    )
    .await;
    let handshake = match opened {
        Ok(opened) => opened.map_err(|source| e!(Error::Moq { source }))?,
        Err(_) => {
            debug!(peer = %peer.fmt_short(), "no session setup in time");
            return Err(e!(Error::Moq {
                source: moq_net::Error::Timeout
            }));
        }
    };
    let request = SessionRequest::from_handshake(&handshake);
    debug!(peer = %peer.fmt_short(), path = request.path(), "session requested");
    let decided = tokio::select! {
        decided = authorize(shared, peer, &request, false) => decided,
        _ = shared.shutdown.cancelled() => {
            handshake.close(moq_net::Error::Cancel);
            return Err(e!(Error::ShutDown));
        }
    };
    let admitted = match decided {
        Ok(admitted) => admitted,
        Err(rejection) => {
            info!(
                peer = %peer.fmt_short(),
                reason = rejection.reason(),
                code = %rejection.code(),
                "the policy rejected the session",
            );
            handshake.close(rejection.code().clone());
            return Err(e!(Error::Rejected { source: rejection }));
        }
    };
    if shared.shutdown.is_cancelled() {
        handshake.close(moq_net::Error::Cancel);
        return Err(e!(Error::ShutDown));
    }
    info!(peer = %peer.fmt_short(), grant = ?admitted.authorization.grant, "admitting session");
    let auth = handshake.auth();
    authorize_direct(shared, &auth, peer, &admitted.authorization.grant);
    let handshake = handshake
        .with_publisher(served(&shared.table))
        .with_peer_hop(Hop::of(&peer).id())
        .with_subscriber(subscriber(shared));
    let (moq, driver) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake.ok())
        .await
        .map_err(|_| {
            e!(Error::Moq {
                source: moq_net::Error::Timeout
            })
        })?
        .map_err(|source| e!(Error::Moq { source }))?;
    let parts = SessionParts {
        peer,
        connection,
        dialed: false,
        admitted,
        request,
        moq,
        driver,
        auth,
    };
    shared.register(parts).await?;
    Ok(())
}

/// A session's authorization from before its handshake, until the session registers.
pub(crate) struct Admitted {
    /// The authorization the session starts with.
    pub(crate) authorization: Authorization,
    /// The epoch at which the decision behind `authorization` started.
    pub(crate) decided: u64,
    /// Whether the policy decided for this session, rather than it reusing the peer's.
    pub(crate) fresh: bool,
    /// Counts the session as pending on its peer until it registers.
    pub(crate) pending: Pending,
}

/// Counts a session as pending on its peer, so triggers reach a peer whose first session has not registered.
///
/// Dropping it, when the session fails before it registers, stops the count.
pub(crate) struct Pending {
    shared: Weak<Shared>,
    peer: EndpointId,
    armed: bool,
}

impl Pending {
    /// Registers the session under the state lock, and stops counting it as pending.
    pub(crate) fn register(
        mut self,
        state: &mut State,
        authorization: Authorization,
        decided: u64,
    ) -> Registered {
        self.armed = false;
        state.register(self.peer, authorization, decided)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        // Never panic in a drop, even after a panic poisoned the lock.
        if let Ok(mut state) = shared.state.lock() {
            state.abandon(&self.peer);
        }
    }
}

/// Returns the authorization a session with `peer` starts with.
///
/// A peer that already holds an authorization keeps it, so a second
/// session is not decided again. Otherwise the policy decides, with `dialed`
/// set when this node dialed. Either way the result is a snapshot: the
/// session reconciles it with the peer's authorization when it registers.
pub(crate) async fn authorize(
    shared: &Arc<Shared>,
    peer: EndpointId,
    request: &SessionRequest,
    dialed: bool,
) -> Result<Admitted, Rejection> {
    let snapshot = shared.state.lock().expect("poisoned").begin(peer);
    let pending = Pending {
        shared: Arc::downgrade(shared),
        peer,
        armed: true,
    };
    if let Some(authorization) = snapshot.stored {
        debug!(peer = %peer.fmt_short(), "the peer already holds an authorization");
        return Ok(Admitted {
            authorization,
            decided: snapshot.epoch,
            fresh: false,
            pending,
        });
    }
    let mut request = PeerRequest::new(peer, request.clone(), Event::Connect);
    if dialed {
        request = request.with_dialed();
    }
    let authorization = decide(shared, &request).await?;
    Ok(Admitted {
        authorization,
        decided: snapshot.epoch,
        fresh: true,
        pending,
    })
}

/// Asks the node's policy about `request`, within [`NodeConfig::decide_timeout`].
///
/// See [`decide_within`](moq_ext_access::policy::decide_within) for what it
/// rejects on its own.
///
/// [`NodeConfig::decide_timeout`]: crate::NodeConfig::decide_timeout
pub(crate) async fn decide(
    shared: &Shared,
    request: &PeerRequest,
) -> Result<Authorization, Rejection> {
    moq_ext_access::policy::decide_within(&*shared.policy, request, shared.decide_timeout).await
}
