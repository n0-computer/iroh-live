//! Admission: who gets a session, and what it may do once it has one.

use std::sync::Arc;

use iroh::{EndpointId, endpoint::Connection};
/// What a session may subscribe to from this node, and publish into it.
///
/// See [`moq_ext_access::Grant`].
pub use moq_ext_access::Grant;
/// What a peer asked for when it opened a session.
pub use moq_ext_access::SessionRequest;
use n0_error::e;
use tracing::{debug, info};

use crate::{
    Direction, Error,
    handshake::{Hop, SessionParts, authorize_direct, subscriber},
    node::Shared,
    policy::{Authorization, Event, PeerRequest, Rejection},
    state::served,
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
    /// iroh's options for the QUIC dial.
    ///
    /// Set a transport config here to dial with another
    /// [`Congestion`](crate::Congestion) controller. The default keeps the endpoint's transport config, which runs BBR3
    /// under [`MoqPreset`](crate::MoqPreset). It applies to a direct dial
    /// only: a session that already exists keeps its connection, so close it
    /// first to dial again with other options. A relay link ignores it.
    pub quic: iroh::endpoint::ConnectOptions,
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
        decided = authorize(shared, peer, &request, Direction::Accepted) => decided,
        _ = shared.shutdown.cancelled() => {
            handshake.close(moq_net::Error::Cancel);
            return Err(e!(Error::ShutDown));
        }
    };
    let (authorization, reauthorized) = match decided {
        Ok(decided) => decided,
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
    info!(peer = %peer.fmt_short(), grant = ?authorization.grant, "admitting session");
    let auth = handshake.auth();
    authorize_direct(shared, &auth, peer, &authorization.grant);
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
        direction: Direction::Accepted,
        authorization,
        reauthorized,
        request,
        moq,
        driver,
        auth,
    };
    shared.register(parts).await?;
    Ok(())
}

/// Decides a new session with `peer`, opened in `direction`.
///
/// Returns the authorization and the count of `Node::reauthorize` calls
/// before the decision started, which the session compares when it
/// registers.
pub(crate) async fn authorize(
    shared: &Shared,
    peer: EndpointId,
    request: &SessionRequest,
    direction: Direction,
) -> Result<(Authorization, u64), Rejection> {
    let reauthorized = shared.state.lock().expect("poisoned").reauthorized;
    let request = PeerRequest::new(peer, request.clone(), Event::Connect).with_direction(direction);
    Ok((decide(shared, &request).await?, reauthorized))
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
