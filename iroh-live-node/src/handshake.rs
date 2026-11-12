//! The MoQ handshake of a direct session, dialed or accepted.

use std::{num::NonZeroU64, sync::Arc};

use iroh::{EndpointAddr, EndpointId, endpoint::Connection};
use moq_net::{
    auth,
    origin::{self, Source},
};
use n0_error::{AnyError, e};
use tracing::{debug, info, warn};

use crate::{
    ConnectOptions, Error, Grant, LinkKind, SessionRequest, admission::authorize, node::Shared,
    policy::Authorization, state::served, transport,
};

/// The moq hop id the routes of one link carry.
///
/// Never zero: moq reserves zero for a route whose announcer named no hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, derive_more::Display)]
pub(crate) struct Hop(NonZeroU64);

impl Hop {
    /// Returns the hop of the node with endpoint id `id`.
    ///
    /// Derived from the id, so a relay recognizes this node's routes after a
    /// restart. The first eight bytes of an ed25519 key are already uniform.
    /// Masked below 2^53, since JavaScript clients read hop ids as numbers.
    pub(crate) fn of(id: &EndpointId) -> Self {
        let bytes: [u8; 8] = id.as_bytes()[..8]
            .try_into()
            .expect("an endpoint id is 32 bytes");
        let value = u64::from_le_bytes(bytes) & ((1u64 << 53) - 1);
        Self(NonZeroU64::new(value.max(1)).expect("at least one"))
    }

    /// Returns the hop a route entered through.
    ///
    /// Returns `None` for a route this node published, or one announced with
    /// no hop.
    pub(crate) fn of_route(source: Source) -> Option<Self> {
        match source {
            Source::Peer(id) => NonZeroU64::new(id).map(Self),
            Source::Local => None,
        }
    }

    /// Returns the id moq takes.
    pub(crate) fn id(self) -> NonZeroU64 {
        self.0
    }
}

/// Authorizes `auth` for what a direct session with `peer` may see now.
///
/// Called before the session starts, so its first announcements already keep
/// to the grant. The node re-authorizes it as its publications change.
pub(crate) fn authorize_direct(
    shared: &Shared,
    auth: &auth::Handle,
    peer: EndpointId,
    grant: &Grant,
) {
    let view =
        shared
            .state
            .lock()
            .expect("poisoned")
            .link_view(LinkKind::Direct, Some(&peer), grant);
    auth.authorize(&view);
}

/// Returns where a session writes what its peer announces.
///
/// That is the route table, tagged [`origin::Link::Peer`] and scoped to
/// everything: the session's authorization is the only gate, so a grant that
/// widens later, as a ticket claim does, takes effect on the live session.
pub(crate) fn subscriber(shared: &Shared) -> origin::Producer {
    shared.table.clone().with_link(origin::Link::Peer)
}

/// Everything a session needs to run, produced by a dial or an admission.
pub(crate) struct SessionParts {
    pub(crate) peer: EndpointId,
    pub(crate) connection: Connection,
    pub(crate) dialed: bool,
    /// What the policy decided for the session before the handshake.
    pub(crate) authorization: Authorization,
    /// The count of `Node::reauthorize` calls before that decision started.
    pub(crate) reauthorized: u64,
    pub(crate) request: SessionRequest,
    pub(crate) moq: moq_net::Session,
    pub(crate) driver: moq_net::Driver,
    /// The session's authorization, which the node keeps up to date.
    pub(crate) auth: auth::Handle,
}

/// Returns the node error for a dial that iroh failed to connect.
fn connect_error(err: impl std::error::Error + Send + Sync + 'static) -> Error {
    e!(Error::Transport {
        source: e!(transport::Error::Connect {
            source: Arc::new(AnyError::from_std(err))
        })
    })
}

/// Dials `peer` and completes the MoQ handshake as the client.
///
/// Asks the node's policy first. The session's authorization is in place
/// before the handshake, so the dialed peer sees only what its grant and the
/// audiences let it see, as an accepted one does.
pub(crate) async fn dial_session(
    shared: Arc<Shared>,
    peer: EndpointAddr,
    options: ConnectOptions,
) -> Result<SessionParts, Error> {
    let peer_id = peer.id;
    let request = SessionRequest::new(options.path.as_deref().unwrap_or_default());
    let (authorization, reauthorized) = authorize(&shared, peer_id, &request, true).await.map_err(
        |source| {
            info!(reason = source.reason(), code = %source.code(), "the policy rejected the dial");
            e!(Error::Rejected { source })
        },
    )?;
    if let Some(congestion) = options.congestion {
        debug!(%congestion, "dialing with its own congestion controller");
    }
    let connection = shared
        .endpoint
        .connect_with_opts(peer, crate::ALPN, options.quic())
        .await
        .map_err(connect_error)?
        .await
        .map_err(connect_error)?;
    let transport = transport::Transport::new(connection)?;
    let connection = transport.connection().clone();
    let auth = auth::Handle::new();
    authorize_direct(&shared, &auth, peer_id, &authorization.grant);
    let (moq, driver) = client_handshake(
        transport,
        &options,
        peer_id,
        &served(&shared.table),
        Some(subscriber(&shared)),
        auth.clone(),
    )
    .await
    .map_err(|source| match source {
        moq_net::Error::Unauthorized | moq_net::Error::App(_) => e!(Error::Refused { source }),
        source => e!(Error::Moq { source }),
    })?;
    Ok(SessionParts {
        peer: peer_id,
        connection,
        dialed: true,
        authorization,
        reauthorized,
        request,
        moq,
        driver,
        auth,
    })
}

/// Completes the MoQ handshake as the client, over a transport this node dialed.
///
/// Offers `publish` to the peer within what `auth` authorizes and, if given,
/// writes what the peer announces to `subscriber`. Fails with
/// [`moq_net::Error::Timeout`] after [`transport::HANDSHAKE_TIMEOUT`], as the
/// accepting side does.
pub(crate) async fn client_handshake(
    transport: transport::Transport,
    options: &ConnectOptions,
    peer: EndpointId,
    publish: &origin::Consumer,
    subscriber: Option<origin::Producer>,
    auth: auth::Handle,
) -> Result<(moq_net::Session, moq_net::Driver), moq_net::Error> {
    let mut client = moq_net::Client::new()
        .with_publisher(publish.clone())
        .with_peer_hop(Hop::of(&peer).id())
        .with_auth(auth);
    if let Some(subscriber) = subscriber {
        client = client.with_subscriber(subscriber);
    }
    if let Some(cost) = options.cost {
        client = client.with_cost(cost);
    }
    if let Some(token) = options
        .token
        .as_deref()
        .map(moq_ext_access::token::setup_token)
    {
        client = client.with_token(token);
    }
    // The SETUP of moq-lite-06, the newest version iroh peers negotiate, has no
    // token field, so the token also rides the setup path.
    if options.path.is_some() || options.token.is_some() {
        let target = moq_ext_access::token::setup_url(
            options.path.as_deref().unwrap_or_default(),
            options.token.as_deref(),
        );
        match url::Url::parse(&format!("moqt://iroh{target}")) {
            Ok(url) => client = client.with_url(url),
            Err(err) => {
                warn!(error = %err, %target, "setup target is no valid URL, requesting the root")
            }
        }
    }
    // moq-lite-06 starts without a round trip, but a version that exchanges
    // SETUP on a stream waits for the peer's reply. A peer that never sends it
    // would hold the dial, and every connect queued behind it, for good.
    tokio::time::timeout(
        transport::HANDSHAKE_TIMEOUT,
        client.connect(moq_net::time::Instant::now(), transport),
    )
    .await
    .map_err(|_| moq_net::Error::Timeout)?
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn the_hop_is_stable_and_in_range() {
        let id = SecretKey::generate().public();
        let hop = Hop::of(&id);
        assert_eq!(hop, Hop::of(&id), "the same id gives the same hop");
        assert!(hop.id().get() < 1 << 53, "{hop}");
        assert_ne!(
            hop,
            Hop::of(&SecretKey::generate().public()),
            "two ids give two hops"
        );
    }
}
