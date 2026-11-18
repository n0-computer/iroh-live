//! The MoQ handshake of a direct session, dialed or accepted.

use std::{num::NonZeroU64, sync::Arc};

use iroh::{EndpointAddr, EndpointId, endpoint::Connection};
use moq_net::{
    auth,
    origin::{self, Priority, Source},
};
use n0_error::e;
use tracing::{info, warn};

use crate::{
    ConnectOptions, Direction, Error, Grant, SessionRequest,
    admission::authorize,
    node::Shared,
    policy::Authorization,
    state::{SessionKind, served},
    transport,
};

/// The SETUP path query that marks a session its dialer opens as a forwarder.
pub(crate) const FORWARDER_ROLE: (&str, &str) = ("role", "forwarder");

/// The priority of the routes a forwarder session this node dialed lands.
///
/// Below a plain session's, so a site's ingress, which offers the path on a
/// plain session, outranks this node's own forwarder route.
const FORWARDER_PRIORITY: Priority = Priority::new(-10);

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

/// Authorizes `auth` for what a direct session of `kind` with `peer` may see now.
///
/// Called before the session starts, so its first announcements already keep
/// to the grant. The node re-authorizes it as its publications change.
pub(crate) fn authorize_direct(
    shared: &Shared,
    auth: &auth::Handle,
    peer: EndpointId,
    kind: SessionKind,
    grant: &Grant,
) {
    let view = shared
        .state
        .lock()
        .expect("poisoned")
        .session_view(&peer, kind, Some(grant));
    auth.authorize(&view);
}

/// Returns where a session of `kind` writes what its peer announces.
///
/// That is the route table, scoped to everything: the session's
/// authorization is the only gate, so a grant that widens later, as a ticket
/// claim does, takes effect on the live session. Most sessions write tagged
/// [`origin::Link::Peer`], which the node never serves on. A forwarder
/// session this node dialed writes tagged [`origin::Link::Local`] at
/// [`FORWARDER_PRIORITY`], so the node may serve its routes to its site.
// TODO(groups): a live priority setter once moq has one. Until then a later
// feature that needs another priority, such as a planned `via`, reopens the
// session.
pub(crate) fn subscriber(shared: &Shared, kind: SessionKind) -> origin::Producer {
    match kind {
        SessionKind::Forwarding => shared
            .table
            .clone()
            .with_link(origin::Link::Local)
            .with_priority(FORWARDER_PRIORITY),
        SessionKind::Plain | SessionKind::Forwarded => {
            shared.table.clone().with_link(origin::Link::Peer)
        }
    }
}

/// Everything a session needs to run, produced by a dial or an admission.
pub(crate) struct SessionParts {
    pub(crate) peer: EndpointId,
    pub(crate) kind: SessionKind,
    pub(crate) connection: Connection,
    pub(crate) direction: Direction,
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

/// Dials `peer` for a session of `kind` and completes the MoQ handshake as the client.
///
/// Asks the node's policy first. The session's authorization is in place
/// before the handshake, so the dialed peer sees only what its grant and the
/// audiences let it see, as an accepted one does. A forwarder session sends
/// [`FORWARDER_ROLE`] in its SETUP path.
// TODO(groups): short idle timeouts on forwarder sessions (5 s, keep-alive
// 1 s), so a site moves off a dead ingress quickly.
pub(crate) async fn dial_session(
    shared: Arc<Shared>,
    peer: EndpointAddr,
    options: ConnectOptions,
    kind: SessionKind,
) -> Result<SessionParts, Error> {
    let peer_id = peer.id;
    let request = SessionRequest::new(options.path.as_deref().unwrap_or_default());
    let (authorization, reauthorized) = authorize(&shared, peer_id, &request, Direction::Dialed)
        .await
        .map_err(|source| {
            info!(reason = source.reason(), code = %source.code(), "the policy rejected the dial");
            e!(Error::Rejected { source })
        })?;
    let connection = shared
        .endpoint
        .connect_with_opts(peer, crate::ALPN, options.quic.clone())
        .await
        .map_err(transport::Error::connect)?
        .await
        .map_err(transport::Error::connect)?;
    let transport = transport::Transport::new(connection)?;
    let connection = transport.connection().clone();
    let auth = auth::Handle::new();
    authorize_direct(&shared, &auth, peer_id, kind, &authorization.grant);
    let (moq, driver) = client_handshake(
        transport,
        &options,
        kind == SessionKind::Forwarding,
        peer_id,
        &served(&shared.table),
        Some(subscriber(&shared, kind)),
        auth.clone(),
    )
    .await
    .map_err(|source| match source {
        moq_net::Error::Unauthorized | moq_net::Error::App(_) => e!(Error::Refused { source }),
        source => e!(Error::Moq { source }),
    })?;
    Ok(SessionParts {
        peer: peer_id,
        kind,
        connection,
        direction: Direction::Dialed,
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
/// writes what the peer announces to `subscriber`. With `forwarder`, the
/// SETUP path carries [`FORWARDER_ROLE`]. Fails with
/// [`moq_net::Error::Timeout`] after [`transport::HANDSHAKE_TIMEOUT`], as the
/// accepting side does.
pub(crate) async fn client_handshake(
    transport: transport::Transport,
    options: &ConnectOptions,
    forwarder: bool,
    peer: EndpointId,
    publish: &origin::Consumer,
    subscriber: Option<origin::Producer>,
    auth: auth::Handle,
) -> Result<(moq_net::Session, moq_net::Driver), moq_net::Error> {
    let mut client = moq_net::Client::new()
        .with_publisher(publish.clone())
        .with_peer_node(Hop::of(&peer).id())
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
    if options.path.is_some() || options.token.is_some() || forwarder {
        let mut target = moq_ext_access::token::setup_url(
            options.path.as_deref().unwrap_or_default(),
            options.token.as_deref(),
        );
        if forwarder {
            let (key, value) = FORWARDER_ROLE;
            let separator = if target.contains('?') { '&' } else { '?' };
            target.push_str(&format!("{separator}{key}={value}"));
        }
        match url::Url::parse(&format!("moqt://iroh{target}")) {
            Ok(url) => client = client.with_url(url),
            Err(err) => {
                // Logged without the query, which holds the token.
                let path = target
                    .split_once('?')
                    .map_or(target.as_str(), |(path, _)| path);
                warn!(error = %err, %path, "setup target is no valid URL, requesting the root")
            }
        }
    }
    // moq-lite-06 starts without a round trip, but a version that exchanges
    // SETUP on a stream waits for the peer's reply. A peer that never sends it
    // would hold the dial, and every connect queued behind it, until the
    // connection closes.
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
