//! Admission: who gets a session, and what it may do once it has one.

use std::sync::Arc;

use iroh::{EndpointId, endpoint::Connection};
/// What a session may subscribe to from this node, and publish into it.
///
/// See [`moq_ext_access::Grant`].
pub use moq_ext_access::Grant;
/// What a peer asked for when it opened a session.
pub use moq_ext_access::SessionRequest;
use moq_net::{server::Handshake, setup::Token};
use moq_transport_iroh::Congestion;
use n0_error::e;
use tracing::{debug, info, warn};

use crate::{
    Error, LinkKind,
    handshake::{Hop, SessionParts, subscriber},
    node::Shared,
    policy::{Authorization, DynPolicy, Event, PeerRequest, Rejection},
    state::served,
    transport::{self, HANDSHAKE_TIMEOUT},
};

/// A function that gives a peer its grant, from its endpoint id.
pub type GrantFn = Arc<dyn Fn(EndpointId) -> Grant + Send + Sync>;

/// How a node treats incoming sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Admission {
    /// Admits the sessions [`NodeConfig::policy`] admits.
    ///
    /// [`NodeConfig::policy`]: crate::NodeConfig::policy
    #[default]
    Open,
}

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
    /// What the dialed peer may do on this node.
    ///
    /// `None` takes it from [`NodeConfig::grant`](crate::NodeConfig::grant).
    /// A new session is authorized for it before its handshake, so nothing
    /// outside it ever reaches the route table or the peer. The grant is the
    /// peer's, so it also holds for every other session with the peer. A
    /// session that already exists is returned as it is: change its grant
    /// with [`Session::set_grant`](crate::Session::set_grant).
    pub grant: Option<Grant>,
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
    let remote = connection.remote_id();
    let transport = transport::accept(connection.clone())?;
    let opened = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        moq_net::Server::new().accept_request(moq_net::time::Instant::now(), transport),
    )
    .await;
    let handshake = match opened {
        Ok(opened) => opened.map_err(|source| e!(Error::Moq { source }))?,
        Err(_) => {
            debug!(remote = %remote.fmt_short(), "no session setup in time");
            return Err(e!(Error::Moq {
                source: moq_net::Error::Timeout
            }));
        }
    };
    let request = session_request(&handshake);
    debug!(remote = %remote.fmt_short(), path = request.path(), "session requested");
    let decided = tokio::select! {
        decided = accept_authorization(shared, remote, &request) => decided,
        _ = shared.shutdown.cancelled() => {
            handshake.close(moq_net::Error::Cancel);
            return Err(e!(Error::ShutDown));
        }
    };
    let authorization = match decided {
        Ok(authorization) => authorization,
        Err(rejection) => {
            info!(
                remote = %remote.fmt_short(),
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
    info!(remote = %remote.fmt_short(), grant = ?authorization.grant, "admitting session");
    // Authorized before the handshake completes, so the session's first
    // announcements already keep to what the peer may see.
    let auth = handshake.auth();
    auth.authorize(&shared.state.lock().expect("poisoned").grant_for(
        LinkKind::Direct,
        Some(&remote),
        &authorization.grant,
        true,
    ));
    let handshake = handshake
        .with_publisher(served(&shared.table))
        .with_peer_hop(Hop::of(&remote).id())
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
        remote,
        connection,
        dialed: false,
        authorization,
        request,
        moq,
        driver,
        auth,
    };
    shared.register(parts).await?;
    Ok(())
}

/// Returns the authorization of an incoming session.
///
/// A peer that already holds a live session keeps that session's
/// authorization, so a second session is not decided again. Otherwise the
/// policy decides.
async fn accept_authorization(
    shared: &Shared,
    remote: EndpointId,
    request: &SessionRequest,
) -> Result<Authorization, Rejection> {
    let stored = shared
        .state
        .lock()
        .expect("poisoned")
        .peer_authorization(&remote)
        .cloned();
    if let Some(authorization) = stored {
        debug!(remote = %remote.fmt_short(), "the peer already holds an authorization");
        return Ok(authorization);
    }
    let request = PeerRequest::new(remote, request.clone(), false, Event::Connect);
    decide(shared, &request).await
}

/// Asks the node's policy about `request`, within [`NodeConfig::decide_timeout`].
///
/// A policy that takes longer is a rejection with
/// [`moq_net::Error::Timeout`]. So is an authorization whose transit is not
/// the default, since the node does not forward yet.
///
/// [`NodeConfig::decide_timeout`]: crate::NodeConfig::decide_timeout
pub(crate) async fn decide(
    shared: &Shared,
    request: &PeerRequest,
) -> Result<Authorization, Rejection> {
    let decided = tokio::time::timeout(
        shared.decide_timeout,
        DynPolicy::decide(&*shared.policy, request),
    )
    .await;
    let Ok(decision) = decided else {
        return Err(
            Rejection::new("the policy did not decide in time").with_code(moq_net::Error::Timeout)
        );
    };
    let authorization = decision?;
    if !authorization.transit.is_none() {
        warn!(
            peer = %request.peer().fmt_short(),
            transit = ?authorization.transit,
            "transit is not supported yet, rejecting the session",
        );
        return Err(Rejection::new("transit is not supported yet"));
    }
    Ok(authorization)
}

/// Returns what the peer asked for in its SETUP: a path, and maybe a token.
///
/// The SETUP of moq-lite-06 and older has no token field. On those versions
/// [`SessionRequest::token`] reads the `jwt` query of the path instead.
fn session_request(handshake: &Handshake) -> SessionRequest {
    let request = SessionRequest::new(handshake.path());
    let token = handshake
        .token()
        .filter(|token| token.kind == Token::OUT_OF_BAND)
        .and_then(|token| std::str::from_utf8(&token.value).ok());
    match token {
        Some(token) => request.with_token(token),
        None => request,
    }
}
