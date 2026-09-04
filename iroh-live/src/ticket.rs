//! Tickets that carry everything a subscriber needs to reach a broadcast.
//!
//! A [`LiveTicket`] is a publisher's endpoint id and the name of one of its
//! broadcasts, in a form that survives a chat message or a QR code. The
//! subscriber gets both halves of what [`Live::subscribe`](crate::Live::subscribe)
//! asks for out of one string.
//!
//! Socket addresses are deliberately absent. A publisher announces its
//! addresses to pkarr and over mDNS, and a subscriber looks them up from the id
//! alone, so a ticket that listed them as well was repeating work two lookup
//! services already do. On a host with several interfaces that list was most of
//! the payload, and it bought nothing: what it cost was a denser QR code, which
//! is the kind that will not scan off a small screen.

use std::str::FromStr;

use iroh::{EndpointAddr, EndpointId};
use n0_error::{Result, StackResultExt, StdResultExt};
use serde::{Deserialize, Serialize};

/// URI scheme prefix for iroh-live tickets.
pub(crate) const SCHEME: &str = "iroh-live:";

/// The length of the raw endpoint id a current ticket encodes.
///
/// Also what tells a current ticket from an older one: a ticket minted before
/// the format shrank encodes a postcard [`EndpointAddr`], which spends these
/// same 32 bytes on the id and then at least one more on its address list.
const ENDPOINT_ID_LEN: usize = 32;

/// Ticket for subscribing to a live broadcast.
///
/// Carries the publisher's endpoint id and the broadcast name, and nothing
/// else. The addresses that reach that id come from iroh's address lookup:
/// pkarr and DNS where there is internet, mDNS on a local network.
///
/// Serializes to a URI: `iroh-live:<base64url(endpoint id)>/<name>`
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display, Serialize, Deserialize)]
#[display("{}", self.serialize())]
pub struct LiveTicket {
    /// The publisher's endpoint, holding its id and no addresses.
    ///
    /// An [`EndpointAddr`] rather than a bare [`EndpointId`] because that is
    /// what [`Live::subscribe`](crate::Live::subscribe) and
    /// [`Call::dial`](crate::Call::dial) take, and an address set left empty is
    /// how iroh spells "resolve this one."
    pub endpoint: EndpointAddr,
    /// The broadcast name to subscribe to.
    pub broadcast_name: String,
}

impl LiveTicket {
    /// Creates a new ticket for `broadcast_name` on the endpoint `endpoint_id`.
    pub fn new(endpoint_id: impl Into<EndpointId>, broadcast_name: impl Into<String>) -> Self {
        Self {
            endpoint: EndpointAddr::from(endpoint_id.into()),
            broadcast_name: broadcast_name.into(),
        }
    }

    /// Returns the publisher's endpoint id.
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id
    }

    /// Serializes to a URI string: `iroh-live:<endpoint id>/<name>`
    pub fn serialize(&self) -> String {
        let id_encoded = data_encoding::BASE64URL_NOPAD.encode(self.endpoint.id.as_bytes());
        format!("{SCHEME}{id_encoded}/{}", self.broadcast_name)
    }

    /// Deserializes from a URI string.
    ///
    /// Also accepts the two shapes that came before: a URI whose payload is a
    /// postcard [`EndpointAddr`] rather than a bare id, and the older
    /// `name@base32` form. Neither keeps its addresses.
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
        let (id_encoded, broadcast_name) = rest
            .split_once('/')
            .std_context("invalid ticket URI: missing / separator")?;

        let bytes = data_encoding::BASE64URL_NOPAD
            .decode(id_encoded.as_bytes())
            .std_context("invalid base64url in ticket")?;

        Ok(Self::new(decode_endpoint_id(&bytes)?, broadcast_name))
    }

    fn deserialize_legacy(s: &str) -> Result<Self> {
        let (broadcast_name, encoded_addr) =
            s.split_once('@').std_context("invalid ticket: missing @")?;
        let bytes = data_encoding::BASE32_NOPAD_NOCASE
            .decode(encoded_addr.as_bytes())
            .std_context("invalid base32")?;
        Ok(Self::new(decode_endpoint_id(&bytes)?, broadcast_name))
    }
}

/// Reads the endpoint id out of the bytes a ticket encodes.
///
/// Current tickets hold the 32 raw bytes of the id. Tickets handed out before
/// the format shrank hold a postcard [`EndpointAddr`] instead, and those still
/// parse: their addresses are discarded, because address lookup finds live ones
/// and the ones written into a ticket months ago have moved on.
///
/// # Errors
///
/// Fails if the bytes are neither an endpoint id nor an [`EndpointAddr`].
fn decode_endpoint_id(bytes: &[u8]) -> Result<EndpointId> {
    if let Ok(id) = <&[u8; ENDPOINT_ID_LEN]>::try_from(bytes) {
        return EndpointId::from_bytes(id).std_context("invalid endpoint id in ticket");
    }
    let addr: EndpointAddr =
        postcard::from_bytes(bytes).std_context("invalid endpoint address in ticket")?;
    Ok(addr.id)
}

impl FromStr for LiveTicket {
    type Err = n0_error::AnyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::deserialize(s)
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use iroh::SecretKey;

    use super::*;

    fn test_endpoint_id() -> EndpointId {
        SecretKey::generate().public()
    }

    /// An endpoint address as a publisher on a multi-homed host reports it.
    fn test_endpoint_addr(id: EndpointId) -> EndpointAddr {
        let mut addr = EndpointAddr::from(id);
        for port in 0..10u16 {
            let socket: SocketAddr = format!("172.17.{port}.1:51923").parse().expect("valid");
            addr = addr.with_ip_addr(socket);
        }
        addr
    }

    #[test]
    fn round_trip() {
        let ticket = LiveTicket::new(test_endpoint_id(), "my-stream");
        let s = ticket.serialize();
        assert!(s.starts_with("iroh-live:"), "should start with scheme: {s}");
        assert!(s.ends_with("/my-stream"), "should end with /name: {s}");
        let parsed = LiveTicket::deserialize(&s).expect("parse");
        assert_eq!(parsed, ticket);
    }

    #[test]
    fn display_fromstr_round_trip() {
        let ticket = LiveTicket::new(test_endpoint_id(), "test");
        let s = ticket.to_string();
        let parsed: LiveTicket = s.parse().expect("parse");
        assert_eq!(parsed, ticket);
    }

    #[test]
    fn a_ticket_carries_no_addresses() {
        let ticket = LiveTicket::new(test_endpoint_id(), "my-stream");
        assert!(ticket.endpoint.addrs.is_empty());
        // 10 for the scheme, 43 for a base64url endpoint id, one separator.
        assert_eq!(ticket.to_string().len(), 10 + 43 + 1 + "my-stream".len());
    }

    #[test]
    fn a_ticket_that_still_lists_addresses_parses_without_them() {
        // The format before this one encoded the whole EndpointAddr. Those
        // tickets keep working, minus their addresses: the id is what a
        // subscriber resolves from now.
        let id = test_endpoint_id();
        let encoded = data_encoding::BASE64URL_NOPAD
            .encode(&postcard::to_stdvec(&test_endpoint_addr(id)).expect("encode"));
        let old = format!("iroh-live:{encoded}/my-stream");

        let parsed = LiveTicket::deserialize(&old).expect("parse the older format");
        assert_eq!(parsed.endpoint_id(), id);
        assert!(parsed.endpoint.addrs.is_empty());
        assert!(
            parsed.to_string().len() < old.len() / 2,
            "the point of the change: {} against {}",
            parsed.to_string().len(),
            old.len()
        );
    }

    #[test]
    fn legacy_format_still_parses() {
        // The oldest format of all: name@BASE32(postcard(EndpointAddr)).
        let id = test_endpoint_id();
        let encoded = data_encoding::BASE32_NOPAD
            .encode(&postcard::to_stdvec(&test_endpoint_addr(id)).expect("encode"))
            .to_ascii_lowercase();
        let legacy = format!("hello@{encoded}");

        let parsed = LiveTicket::deserialize(&legacy).expect("parse legacy");
        assert_eq!(parsed.broadcast_name, "hello");
        assert_eq!(parsed.endpoint_id(), id);
    }

    #[test]
    fn rejects_garbage() {
        assert!(LiveTicket::deserialize("not-a-ticket").is_err());
    }

    #[test]
    fn a_ticket_qr_stays_sparse() {
        // A QR code holds 84 bytes in 37 modules at the default error
        // correction level, and 37 modules is three pixels each on the 122 px
        // e-paper panel the Pi Zero demo draws on. The format before this one
        // ran to 184 bytes, which took 57 modules and got one pixel each.
        let ticket = LiveTicket::new(test_endpoint_id(), "my-stream-name");
        let s = ticket.to_string();
        assert!(
            s.len() <= 84,
            "ticket too long for a sparse QR: {}",
            s.len()
        );
    }
}
