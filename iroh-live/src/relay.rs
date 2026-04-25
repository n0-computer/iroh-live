//! Client-side helpers for talking to an `iroh-live-relay`.
//!
//! The relay accepts both raw MoQ-over-iroh and HTTP/3 WebTransport. We
//! always use the H3 path from iroh-live because it carries URL-level
//! context: the relay can see the broadcast path and (optionally) a JWT
//! passed as a `?jwt=<token>` query parameter. Raw MoQ works too but
//! forfeits that context, which means tokens would have to ride in the
//! handshake itself (a moq-lite-level change we have not yet made).

use iroh::EndpointId;
use iroh_moq::MoqSession;
use n0_error::{AnyError, Result};
use url::Url;

use crate::Live;

/// A resolved target for [`Live::connect_relay`].
///
/// Wraps the remote endpoint id with the path and auth material
/// the client wants the relay to see during the H3 handshake. The
/// `Debug` impl elides `api_key` so JWT material never lands in
/// traces.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RelayTarget {
    endpoint: EndpointId,
    path: String,
    api_key: Option<String>,
}

impl std::fmt::Debug for RelayTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayTarget")
            .field("endpoint", &self.endpoint.fmt_short().to_string())
            .field("path", &self.path)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl RelayTarget {
    /// Creates a target with an empty path and no token.
    pub fn new(endpoint: EndpointId) -> Self {
        Self {
            endpoint,
            path: "/".to_string(),
            api_key: None,
        }
    }

    /// Sets the URL path the relay should observe.
    ///
    /// The leading slash is added when missing. The path is forwarded to
    /// the relay as-is and drives which broadcast namespace the session
    /// operates under.
    pub fn with_path(mut self, path: &str) -> Self {
        let normalized = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        self.path = normalized;
        self
    }

    /// Attaches a JWT api key. The token is carried through the URL as the
    /// `jwt` query parameter when the relay enforces auth.
    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key;
        self
    }

    /// Returns the remote endpoint id.
    pub fn endpoint(&self) -> EndpointId {
        self.endpoint
    }

    /// Returns the URL path this target operates under.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the api key the target carries, if any.
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Returns the URL the client will hand to the relay during the H3
    /// handshake.
    pub fn url(&self) -> Url {
        // We always use `https` because web-transport-iroh requires it and
        // the host is the endpoint id (not a DNS name).
        let mut url = Url::parse(&format!("https://{}", self.endpoint)).expect("valid endpoint id");
        url.set_path(&self.path);
        if let Some(ref jwt) = self.api_key {
            url.query_pairs_mut().append_pair("jwt", jwt);
        }
        url
    }
}

impl Live {
    /// Connects to a remote relay via HTTP/3-over-iroh, returning a
    /// MoQ session that can publish or subscribe under the target's
    /// path scope.
    ///
    /// Each call opens a fresh QUIC connection: distinct URLs (for
    /// example different tokens) must not share session state.
    pub async fn connect_relay(&self, target: &RelayTarget) -> Result<MoqSession, AnyError> {
        let addr = iroh::EndpointAddr::new(target.endpoint);
        self.transport()
            .connect_h3(self.endpoint(), addr, target.url())
            .await
    }
}
