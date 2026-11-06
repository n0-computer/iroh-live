//! The MoQ handshake of a direct session, dialed or accepted.

use std::sync::Arc;

use iroh::{EndpointAddr, EndpointId, endpoint::Connection};
use moq_net::{auth, origin};
use n0_error::e;
use tracing::{debug, warn};

use crate::{
    ConnectOptions, Error, Grant, LinkKind, SessionRequest, node::Shared, state::served, transport,
};

/// Returns the moq hop id of the node with endpoint id `id`.
///
/// Derived from the id, so a relay recognizes this node's routes after a
/// restart. The first eight bytes of an ed25519 key are already uniform. Masked
/// below 2^53, since JavaScript clients read hop ids as numbers, and never
/// zero, which moq reserves.
pub(crate) fn hop_for(id: &EndpointId) -> moq_net::Hop {
    let bytes: [u8; 8] = id.as_bytes()[..8]
        .try_into()
        .expect("an endpoint id is 32 bytes");
    let value = u64::from_le_bytes(bytes);
    moq_net::Hop::new((value & ((1u64 << 53) - 1)).max(1)).expect("non-zero and below 2^62")
}

/// Returns the instant a session's driver starts from.
///
/// Read from tokio's clock, which `moq_net::time::run` polls the driver with.
/// Under `tokio::time::pause` std's clock runs ahead, and a driver refuses
/// time that moves backwards.
pub(crate) fn driver_now() -> moq_net::time::Instant {
    moq_net::time::Instant::now()
}

/// Returns an auth handle authorized for what a direct session with `remote` may see now.
///
/// Authorized before the session starts, so its first announcements already
/// keep to the grant. The node re-authorizes it as its publications change.
pub(crate) fn authorized(shared: &Shared, remote: EndpointId, grant: &Grant) -> auth::Handle {
    let auth = auth::Handle::new();
    let scope = shared.state.lock().expect("poisoned").grant_for(
        LinkKind::Direct,
        Some(&remote),
        grant,
        true,
    );
    auth.authorize(&scope);
    auth
}

/// Returns where a session with `remote` writes what its peer announces.
///
/// That is the route table, tagged [`origin::Link::Peer`] and scoped to what
/// `grant` or the node's own grant for `remote` lets the peer publish. The
/// scope bounds the session for good, so a session that starts with a grant
/// narrower than the node's can still be widened up to it with
/// [`Session::set_grant`]. The session's authorization keeps the peer to
/// `grant` meanwhile. `None` if neither grant lets the peer publish anything.
pub(crate) fn subscriber(
    shared: &Shared,
    remote: EndpointId,
    grant: &Grant,
) -> Option<origin::Producer> {
    let mut publish = grant.publish.clone();
    publish.extend(shared.grant_for(remote).publish.iter().cloned());
    if publish.is_empty() {
        return None;
    }
    match shared
        .table
        .clone()
        .with_link(origin::Link::Peer)
        .scope("", &publish)
    {
        Ok(scoped) => Some(scoped),
        Err(err) => {
            warn!(
                remote = %remote.fmt_short(),
                %err,
                "the grant's publish patterns fit no path, so nothing the peer announces is ingested"
            );
            None
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
    pub(crate) driver: moq_net::Driver,
    /// The session's authorization, which the node keeps up to date.
    pub(crate) auth: auth::Handle,
}

/// Dials `remote` and completes the MoQ handshake as the client.
///
/// The session's authorization is in place before the handshake, so the dialed
/// peer sees only what its grant and the audiences let it see, as an accepted
/// one does.
pub(crate) async fn dial_session(
    shared: Arc<Shared>,
    remote: EndpointAddr,
    options: ConnectOptions,
) -> Result<SessionParts, Error> {
    let remote_id = remote.id;
    let grant = options
        .grant
        .clone()
        .unwrap_or_else(|| shared.grant_for(remote_id));
    if let Some(congestion) = options.congestion {
        debug!(%congestion, "dialing with its own congestion controller");
    }
    let transport = transport::dial_with(&shared.endpoint, remote, options.quic()).await?;
    let connection = transport.conn().clone();
    let auth = authorized(&shared, remote_id, &grant);
    let (moq, driver) = client_handshake(
        transport,
        &options,
        remote_id,
        &served(&shared.table),
        subscriber(&shared, remote_id, &grant),
        auth.clone(),
    )
    .await
    .map_err(|source| match source {
        moq_net::Error::Unauthorized | moq_net::Error::App(_) => e!(Error::Refused { source }),
        source => e!(Error::Moq { source }),
    })?;
    Ok(SessionParts {
        remote: remote_id,
        connection,
        dialed: true,
        grant,
        request: SessionRequest::default(),
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
    remote: EndpointId,
    publish: &origin::Consumer,
    subscriber: Option<origin::Producer>,
    auth: auth::Handle,
) -> Result<(moq_net::Session, moq_net::Driver), moq_net::Error> {
    let mut client = moq_net::Client::new()
        .with_publisher(publish.clone())
        .with_peer_hop(hop_for(&remote))
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
    if let Some(path) = options
        .token
        .as_deref()
        .map(moq_ext_access::token::setup_path)
        && let Ok(url) = url::Url::parse(&format!("moqt://iroh{path}"))
    {
        client = client.with_url(url);
    }
    // moq-lite-06 starts without a round trip, but a version that exchanges
    // SETUP on a stream waits for the peer's reply. A peer that never sends it
    // would hold the dial, and every connect queued behind it, for good.
    tokio::time::timeout(
        transport::HANDSHAKE_TIMEOUT,
        client.connect(driver_now(), transport),
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
        let hop = hop_for(&id);
        assert_eq!(hop, hop_for(&id), "the same id gives the same hop");
        assert!(hop.id() > 0 && hop.id() < 1 << 53, "{}", hop.id());
        assert_ne!(
            hop,
            hop_for(&SecretKey::generate().public()),
            "two ids give two hops"
        );
    }
}
