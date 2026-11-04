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
use n0_error::e;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::{
    Error, LinkKind, Session,
    node::Shared,
    session::{SessionParts, driver_now, hop_for, subscriber},
    state::served,
    transport::{self, HANDSHAKE_TIMEOUT},
};

/// A function that gives a peer its grant, from its endpoint id.
pub type GrantFn = Arc<dyn Fn(EndpointId) -> Grant + Send + Sync>;

/// How many incoming sessions may wait for [`Node::accept`] at once.
///
/// Past this, new sessions wait up to [`ADMISSION_TIMEOUT`] for room.
pub(crate) const INCOMING_QUEUE: usize = 16;

/// How long an incoming session waits for room in the admission queue.
///
/// Keeps a stalled accept loop from piling up connections whose peers think
/// they are connected.
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
    Manual,
}

/// How to dial a peer with [`Node::connect_with`](crate::Node::connect_with).
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// A token to present, sent in the MoQ SETUP and as `?jwt=` in the setup path.
    ///
    /// The accepting node reads it as [`SessionRequest::token`].
    pub token: Option<String>,
    /// The cost added to every route learned over this link.
    ///
    /// `None` keeps moq's default of one.
    pub cost: Option<u64>,
    /// What the dialed peer may do on this node.
    ///
    /// `None` takes it from [`NodeConfig::grant`](crate::NodeConfig::grant).
    /// A new session is authorized for it before its handshake, so nothing
    /// outside it ever reaches the route table or the peer. A session that
    /// already exists keeps its grant: change that with
    /// [`Session::set_grant`](crate::Session::set_grant).
    pub grant: Option<Grant>,
}

impl ConnectOptions {
    /// Returns the token as the MoQ SETUP carries it, if there is one.
    pub(crate) fn setup_token(&self) -> Option<Token> {
        let token = self.token.as_ref()?;
        Some(Token {
            kind: Token::OUT_OF_BAND,
            value: token.clone().into_bytes(),
        })
    }

    /// Returns the setup path that carries the token, if there is one.
    pub(crate) fn setup_path(&self) -> Option<String> {
        let token = self.token.as_ref()?;
        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("jwt", token)
            .finish();
        Some(format!("/?{query}"))
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
    /// Cancellation safe: dropping the future before the handshake completes
    /// rejects the session, and dropping it after admits the session.
    ///
    /// # Errors
    ///
    /// Fails if the MoQ handshake fails or takes longer than
    /// [`HANDSHAKE_TIMEOUT`], and with [`Error::ShutDown`] once the node has
    /// shut down, which refuses the peer.
    pub async fn admit(self, grant: Grant) -> Result<Session, Error> {
        let Some(shared) = self
            .shared
            .upgrade()
            .filter(|shared| !shared.shutdown.is_cancelled())
        else {
            self.handshake.close(moq_net::Error::Cancel);
            return Err(e!(Error::ShutDown));
        };
        info!(remote = %self.remote.fmt_short(), ?grant, "admitting session");
        // Authorized before the handshake completes, so the session's first
        // announcements already keep to what the peer may see.
        let auth = self.handshake.auth();
        auth.authorize(&shared.state.lock().expect("poisoned").grant_for(
            LinkKind::Direct,
            Some(&self.remote),
            &grant,
            true,
        ));
        let mut handshake = self
            .handshake
            .with_publisher(served(&shared.table))
            .with_peer_hop(hop_for(&self.remote));
        if let Some(subscriber) = subscriber(&shared, self.remote, &grant) {
            handshake = handshake.with_subscriber(subscriber);
        }
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
            grant,
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
        moq_net::Server::new().accept_request(driver_now(), transport),
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
            incoming.admit(shared.grant_for(remote)).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_rides_the_setup_path() {
        let options = ConnectOptions {
            token: Some("a+b".into()),
            ..Default::default()
        };
        let path = options.setup_path().expect("a path");
        let request = SessionRequest::new(&path);
        assert_eq!(request.token(), Some("a+b"));
        assert_eq!(ConnectOptions::default().setup_path(), None);
        let token = options.setup_token().expect("a token");
        assert_eq!(token.value, b"a+b");
        assert_eq!(ConnectOptions::default().setup_token(), None);
    }
}
