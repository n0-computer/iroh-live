//! Tickets that name a broadcast.
//!
//! A ticket holds no socket addresses. Subscribers look them up from the
//! endpoint id over pkarr and mDNS, which keeps the ticket short enough for a
//! QR code on a small screen.

use std::{fmt, str::FromStr};

use iroh::EndpointId;
use iroh_tickets::ParseError;
use moq_net::{Path, PathOwned};
use serde::{Deserialize, Serialize};

/// The broadcast name of a call, as `irl call` and the Android demo use it.
///
/// Each side publishes `live/<its id>/call` to the other peer only, and
/// subscribes to the other's. A node learns it is called when a peer's `call`
/// path appears in its route table.
pub const CALL: &str = "call";

/// URI scheme prefix of a broadcast ticket.
const SCHEME: &str = "iroh-live:";

/// The length of the raw endpoint id a ticket encodes.
const ENDPOINT_ID_LEN: usize = 32;

/// A publisher's endpoint id and the name of one of its broadcasts.
///
/// The string form is `iroh-live:<base64url(endpoint id)>/<name>`. Parsing
/// also accepts it without the scheme. Serde uses the string form.
///
/// # Examples
///
/// ```
/// use iroh_live::BroadcastTicket;
///
/// let peer = iroh::SecretKey::generate().public();
/// let ticket = BroadcastTicket::new(peer, "studio");
/// let parsed: BroadcastTicket = ticket.to_string().parse()?;
/// assert_eq!(parsed, ticket);
/// assert_eq!(ticket.path().as_str(), format!("live/{peer}/studio"));
/// # Ok::<(), iroh_tickets::ParseError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BroadcastTicket {
    peer: EndpointId,
    name: String,
}

impl BroadcastTicket {
    /// Creates a ticket for broadcast `name` of `peer`.
    pub fn new(peer: EndpointId, name: impl Into<String>) -> Self {
        Self {
            peer,
            name: name.into(),
        }
    }

    /// Returns the publisher's endpoint id.
    pub fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Returns the broadcast's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the path the broadcast is published at: `live/<peer>/<name>`.
    pub fn path(&self) -> PathOwned {
        Path::new(&format!("live/{}/{}", self.peer, self.name)).to_owned()
    }

    /// Returns the ticket of the broadcast at `path`, if it is a `live/<peer>/<name>` path.
    pub fn from_path(path: &str) -> Option<Self> {
        let mut parts = path.split('/');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("live"), Some(peer), Some(name), None) if !name.is_empty() => {
                Some(Self::new(peer.parse().ok()?, name))
            }
            _ => None,
        }
    }

    fn parse_uri(rest: &str) -> Result<Self, ParseError> {
        let (id, name) = rest
            .split_once('/')
            .ok_or_else(|| invalid("missing / separator"))?;
        if name.is_empty() {
            return Err(invalid("empty broadcast name"));
        }
        let bytes = data_encoding::BASE64URL_NOPAD
            .decode(id.as_bytes())
            .map_err(|_| invalid("invalid base64url"))?;
        let id = <&[u8; ENDPOINT_ID_LEN]>::try_from(bytes.as_slice())
            .map_err(|_| invalid("not an endpoint id"))?;
        let peer = EndpointId::from_bytes(id).map_err(|_| invalid("invalid endpoint id"))?;
        Ok(Self::new(peer, name))
    }
}

fn invalid(reason: &'static str) -> ParseError {
    ParseError::verification_failed(reason)
}

impl fmt::Display for BroadcastTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = data_encoding::BASE64URL_NOPAD.encode(self.peer.as_bytes());
        write!(f, "{SCHEME}{id}/{}", self.name)
    }
}

impl FromStr for BroadcastTicket {
    type Err = ParseError;

    /// Parses the `iroh-live:` URI, with or without its scheme.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        Self::parse_uri(s.strip_prefix(SCHEME).unwrap_or(s))
    }
}

impl Serialize for BroadcastTicket {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BroadcastTicket {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    fn test_endpoint_id() -> EndpointId {
        SecretKey::generate().public()
    }

    #[test]
    fn round_trip() {
        let ticket = BroadcastTicket::new(test_endpoint_id(), "my-stream");
        let s = ticket.to_string();
        assert!(s.starts_with("iroh-live:"), "should start with scheme: {s}");
        assert!(s.ends_with("/my-stream"), "should end with /name: {s}");
        assert_eq!(s.parse::<BroadcastTicket>().expect("parse"), ticket);
    }

    #[test]
    fn a_ticket_carries_no_addresses() {
        let ticket = BroadcastTicket::new(test_endpoint_id(), "my-stream");
        // 10 for the scheme, 43 for a base64url endpoint id, one separator.
        assert_eq!(ticket.to_string().len(), 10 + 43 + 1 + "my-stream".len());
    }

    #[test]
    fn rejects_garbage() {
        assert!("not-a-ticket".parse::<BroadcastTicket>().is_err());
        assert!("hello".parse::<BroadcastTicket>().is_err());
        assert!("my-stream-360p".parse::<BroadcastTicket>().is_err());
        let nameless = BroadcastTicket::new(test_endpoint_id(), "");
        assert!(nameless.to_string().parse::<BroadcastTicket>().is_err());
    }

    #[test]
    fn serde_reads_the_string_form() {
        use serde::de::{IntoDeserializer, value::Error};

        let ticket = BroadcastTicket::new(test_endpoint_id(), "cam");
        let string = ticket.to_string();
        let back = BroadcastTicket::deserialize(IntoDeserializer::<Error>::into_deserializer(
            string.as_str(),
        ))
        .expect("deserialize");
        assert_eq!(back, ticket);
    }

    #[test]
    fn the_path_names_the_publisher() {
        let peer = test_endpoint_id();
        let ticket = BroadcastTicket::new(peer, "cam");
        assert_eq!(ticket.path().as_str(), format!("live/{peer}/cam"));
        assert_eq!(
            BroadcastTicket::from_path(ticket.path().as_str()),
            Some(ticket)
        );
        assert_eq!(
            BroadcastTicket::from_path(&format!("live/{peer}/cam/hd")),
            None
        );
        assert_eq!(
            BroadcastTicket::from_path(&format!("rooms/{peer}/cam")),
            None
        );
        assert_eq!(BroadcastTicket::from_path("live/nobody/cam"), None);
    }
}
