//! Admission: who gets a session, and what it may do once it has one.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

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
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    Error, LinkKind, Session,
    handshake::{Hop, SessionParts, subscriber},
    node::Shared,
    policy::{Authorization, DynPolicy, Event, PeerRequest, Rejection},
    state::served,
    transport::{self, HANDSHAKE_TIMEOUT},
};

/// A function that gives a peer its grant, from its endpoint id.
pub type GrantFn = Arc<dyn Fn(EndpointId) -> Grant + Send + Sync>;

/// How many incoming sessions may wait for [`Node::accept`] at once.
///
/// Past this, new sessions wait up to [`ADMISSION_TIMEOUT`] for room.
pub(crate) const INCOMING_QUEUE: usize = 16;

/// How long an incoming session waits for room in the admission queue, and a session for its policy.
///
/// Keeps a stalled accept loop or policy from piling up connections whose
/// peers think they are connected.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);

/// How a node treats incoming sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Admission {
    /// Admits every session with the grant [`NodeConfig::grant`] gives its peer.
    ///
    /// [`NodeConfig::grant`]: crate::NodeConfig::grant
    #[default]
    Open,
    /// Hands every incoming session to [`Node::accept`](crate::Node::accept) to decide.
    ///
    /// The node's policy then decides only the sessions it dials.
    Manual,
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

/// An incoming session waiting for admission.
///
/// Yielded by [`Node::accept`](crate::Node::accept) under
/// [`Admission::Manual`](crate::Admission::Manual). Dropping it without
/// admitting rejects the session.
#[derive(derive_more::Debug)]
pub struct Incoming {
    #[debug("{}", remote.fmt_short())]
    pub(crate) remote: EndpointId,
    pub(crate) request: SessionRequest,
    #[debug(skip)]
    pub(crate) connection: Connection,
    #[debug(skip)]
    pub(crate) handshake: Handshake,
    #[debug(skip)]
    pub(crate) shared: Weak<Shared>,
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
    /// The grant is the peer's: it replaces the grant of every other live
    /// session with the peer too.
    ///
    /// Cancellation safe: dropping the future before the handshake completes
    /// rejects the session, and dropping it after admits the session.
    ///
    /// # Errors
    ///
    /// Fails if the MoQ handshake fails or takes longer than
    /// [`HANDSHAKE_TIMEOUT`], and with [`Error::ShutDown`] once the node has
    /// shut down, which refuses the peer.
    pub async fn admit(self, grant: Grant) -> Result<Session, Error> {
        self.admit_with(grant.into()).await
    }

    /// Admits the session with `authorization`, as [`admit`](Self::admit) does.
    async fn admit_with(self, authorization: Authorization) -> Result<Session, Error> {
        let Some(shared) = self
            .shared
            .upgrade()
            .filter(|shared| !shared.shutdown.is_cancelled())
        else {
            self.handshake.close(moq_net::Error::Cancel);
            return Err(e!(Error::ShutDown));
        };
        info!(remote = %self.remote.fmt_short(), grant = ?authorization.grant, "admitting session");
        // Authorized before the handshake completes, so the session's first
        // announcements already keep to what the peer may see.
        let auth = self.handshake.auth();
        auth.authorize(&shared.state.lock().expect("poisoned").grant_for(
            LinkKind::Direct,
            Some(&self.remote),
            &authorization.grant,
            true,
        ));
        let mut handshake = self
            .handshake
            .with_publisher(served(&shared.table))
            .with_peer_hop(Hop::of(&self.remote).id());
        handshake = handshake.with_subscriber(subscriber(&shared));
        let (moq, driver) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake.ok())
            .await
            .map_err(|_| {
                e!(Error::Moq {
                    source: moq_net::Error::Timeout
                })
            })?
            .map_err(|source| e!(Error::Moq { source }))?;
        let parts = SessionParts {
            remote: self.remote,
            connection: self.connection,
            dialed: false,
            authorization,
            request: self.request,
            moq,
            driver,
            auth,
        };
        shared.register(parts).await
    }

    /// Rejects the session with `reason`, such as [`moq_net::Error::Unauthorized`].
    pub fn reject(self, reason: moq_net::Error) {
        info!(remote = %self.remote.fmt_short(), %reason, "rejecting session");
        self.handshake.close(reason);
    }
}

/// Admits or queues one incoming connection, for the protocol handler.
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
    let incoming = Incoming {
        remote,
        request,
        connection,
        handshake,
        shared: Arc::downgrade(shared),
    };
    match shared.admission {
        Admission::Open => {
            let authorization = accept_authorization(shared, &incoming).await;
            let authorization = match authorization {
                Ok(authorization) => authorization,
                Err(rejection) => {
                    info!(
                        remote = %remote.fmt_short(),
                        reason = rejection.reason(),
                        code = %rejection.code(),
                        "the policy rejected the session",
                    );
                    incoming.handshake.close(rejection.code().clone());
                    return Err(e!(Error::Rejected { source: rejection }));
                }
            };
            incoming.admit_with(authorization).await?;
        }
        Admission::Manual => {
            let room = tokio::select! {
                room = tokio::time::timeout(
                    ADMISSION_TIMEOUT,
                    shared.incoming_tx.reserve(),
                ) => room,
                _ = shared.shutdown.cancelled() => Ok(Err(mpsc::error::SendError(()))),
            };
            match room {
                Ok(Ok(permit)) => permit.send(incoming),
                Ok(Err(_)) => {
                    incoming.handshake.close(moq_net::Error::Cancel);
                    return Err(e!(Error::ShutDown));
                }
                Err(_) => {
                    info!(remote = %remote.fmt_short(), "admission queue full, rejecting");
                    incoming.handshake.close(moq_net::Error::Timeout);
                    return Err(e!(Error::Moq {
                        source: moq_net::Error::Timeout
                    }));
                }
            }
        }
    }
    Ok(())
}

/// Returns the authorization of an incoming session.
///
/// A peer that already holds a live session keeps that session's
/// authorization, so a second session is not decided again. Otherwise the
/// policy decides.
async fn accept_authorization(
    shared: &Shared,
    incoming: &Incoming,
) -> Result<Authorization, Rejection> {
    let stored = shared
        .state
        .lock()
        .expect("poisoned")
        .peer_authorization(&incoming.remote)
        .cloned();
    if let Some(authorization) = stored {
        debug!(remote = %incoming.remote.fmt_short(), "the peer already holds an authorization");
        return Ok(authorization);
    }
    let request = PeerRequest::new(
        incoming.remote,
        incoming.request.clone(),
        false,
        Event::Connect,
    );
    decide(shared, &request).await
}

/// Asks the node's policy about `request`, within [`ADMISSION_TIMEOUT`].
///
/// A policy that takes longer is a rejection with
/// [`moq_net::Error::Timeout`]. So is an authorization whose transit is not
/// the default, since the node does not forward yet.
pub(crate) async fn decide(
    shared: &Shared,
    request: &PeerRequest,
) -> Result<Authorization, Rejection> {
    let decided = tokio::time::timeout(
        ADMISSION_TIMEOUT,
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

/// Waits for the next session that needs admission.
pub(crate) async fn next(shared: &Shared) -> Option<Incoming> {
    let mut queue = shared.incoming_rx.lock().await;
    tokio::select! {
        // Never hand out a session after the shutdown, even a queued one.
        biased;
        _ = shared.shutdown.cancelled() => None,
        incoming = queue.recv() => incoming,
    }
}
