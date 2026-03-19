use std::str::FromStr;

use iroh::EndpointAddr;
use n0_error::{Result, StackResultExt, StdResultExt};
use serde::{Deserialize, Serialize};

/// URI scheme prefix for iroh-live tickets.
pub(crate) const SCHEME: &str = "iroh-live:";

/// Ticket for subscribing to a live broadcast.
///
/// Contains the publisher's endpoint address, the broadcast name, and
/// optional relay URLs for reaching the publisher through a relay when
/// direct P2P is unavailable.
///
/// Serializes to a URI: `iroh-live:<base64url(postcard(EndpointAddr))>/<name>`
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display, Serialize, Deserialize)]
#[display("{}", self.serialize())]
pub struct LiveTicket {
    /// The publisher's endpoint address.
    pub endpoint: EndpointAddr,
    /// The broadcast name to subscribe to.
    pub broadcast_name: String,
    /// Optional relay URLs for indirect connectivity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relay_urls: Vec<String>,
}

impl LiveTicket {
    /// Creates a new ticket with the given endpoint and broadcast name.
    pub fn new(endpoint: impl Into<EndpointAddr>, broadcast_name: impl ToString) -> Self {
        Self {
            endpoint: endpoint.into(),
            broadcast_name: broadcast_name.to_string(),
            relay_urls: Vec::new(),
        }
    }

    /// Adds relay URLs to this ticket.
    pub fn with_relay_urls(mut self, urls: impl IntoIterator<Item = String>) -> Self {
        self.relay_urls = urls.into_iter().collect();
        self
    }

    /// Serializes to raw postcard bytes.
    ///
    /// For a human-readable URI, use [`serialize`](Self::serialize) or [`Display`](std::fmt::Display).
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("LiveTicket serialization is infallible")
    }

    /// Deserializes from raw postcard bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let ticket = postcard::from_bytes(bytes).std_context("failed to deserialize")?;
        Ok(ticket)
    }

    /// Serializes to a URI string: `iroh-live:<addr>/<name>`
    pub fn serialize(&self) -> String {
        let addr_bytes =
            postcard::to_stdvec(&self.endpoint).expect("EndpointAddr serialization is infallible");
        let addr_encoded = data_encoding::BASE64URL_NOPAD.encode(&addr_bytes);
        format!("{SCHEME}{addr_encoded}/{}", self.broadcast_name)
    }

    /// Deserializes from a URI string. Also accepts the legacy `name@base32` format.
    pub fn deserialize(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix(SCHEME) {
            Self::deserialize_url(rest)
        } else if s.contains('@') {
            Self::deserialize_legacy(s)
        } else {
            Self::deserialize_url(s)
                .context("invalid ticket: expected iroh-live: URI or legacy name@addr format")
        }
    }

    fn deserialize_url(rest: &str) -> Result<Self> {
        let (addr_encoded, broadcast_name) = rest
            .split_once('/')
            .std_context("invalid ticket URI: missing / separator")?;

        let addr_bytes = data_encoding::BASE64URL_NOPAD
            .decode(addr_encoded.as_bytes())
            .std_context("invalid base64url in ticket")?;
        let endpoint: EndpointAddr =
            postcard::from_bytes(&addr_bytes).std_context("invalid endpoint address")?;

        Ok(Self {
            endpoint,
            broadcast_name: broadcast_name.to_string(),
            relay_urls: Vec::new(),
        })
    }

    fn deserialize_legacy(s: &str) -> Result<Self> {
        let (broadcast_name, encoded_addr) =
            s.split_once('@').std_context("invalid ticket: missing @")?;
        let endpoint: EndpointAddr = postcard::from_bytes(
            &data_encoding::BASE32_NOPAD_NOCASE
                .decode(encoded_addr.as_bytes())
                .std_context("invalid base32")?,
        )
        .std_context("failed to parse")?;
        Ok(Self {
            broadcast_name: broadcast_name.to_string(),
            endpoint,
            relay_urls: Vec::new(),
        })
    }
}

impl FromStr for LiveTicket {
    type Err = n0_error::AnyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::deserialize(s)
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    fn test_endpoint_addr() -> EndpointAddr {
        let key = SecretKey::generate(&mut rand::rng());
        EndpointAddr::from(key.public())
    }

    #[test]
    fn round_trip() {
        let ticket = LiveTicket::new(test_endpoint_addr(), "my-stream");
        let s = ticket.serialize();
        assert!(s.starts_with("iroh-live:"), "should start with scheme: {s}");
        assert!(s.ends_with("/my-stream"), "should end with /name: {s}");
        let parsed = LiveTicket::deserialize(&s).expect("parse");
        assert_eq!(parsed.endpoint, ticket.endpoint);
        assert_eq!(parsed.broadcast_name, "my-stream");
    }

    #[test]
    fn display_fromstr_round_trip() {
        let ticket = LiveTicket::new(test_endpoint_addr(), "test");
        let s = ticket.to_string();
        let parsed: LiveTicket = s.parse().expect("parse");
        assert_eq!(parsed, ticket);
    }

    #[test]
    fn legacy_format_still_parses() {
        // Generate a ticket in the old format: name@BASE32(postcard(EndpointAddr))
        let addr = test_endpoint_addr();
        let encoded = data_encoding::BASE32_NOPAD
            .encode(&postcard::to_stdvec(&addr).unwrap())
            .to_ascii_lowercase();
        let legacy = format!("hello@{encoded}");

        let parsed = LiveTicket::deserialize(&legacy).expect("parse legacy");
        assert_eq!(parsed.broadcast_name, "hello");
        assert_eq!(parsed.endpoint, addr);
    }

    #[test]
    fn rejects_garbage() {
        assert!(LiveTicket::deserialize("not-a-ticket").is_err());
    }

    #[test]
    fn ticket_string_fits_in_qr_code() {
        // QR codes can hold up to ~4296 alphanumeric characters. Verify
        // that a ticket with a full EndpointAddr serializes to a string
        // short enough for a QR code (important for terminal/e-paper display).
        let ticket = LiveTicket::new(test_endpoint_addr(), "my-stream-name");
        let s = ticket.to_string();
        assert!(
            s.len() < 2000,
            "ticket string too long for QR: {} bytes",
            s.len()
        );
        // Also verify it starts with the expected scheme.
        assert!(s.starts_with("iroh-live:"));
    }
}
