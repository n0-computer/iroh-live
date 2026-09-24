//! Tickets that name a broadcast.
//!
//! A [`BroadcastTicket`] is a publisher's endpoint id and the name of one of its
//! broadcasts, in a form that survives a chat message or a QR code. It maps to
//! the path `live/<publisher>/<name>`.
//!
//! Socket addresses are deliberately absent. A publisher announces its
//! addresses to pkarr and over mDNS, and a subscriber looks them up from the id
//! alone, so a ticket that listed them as well repeated what two lookup services
//! already do. On a host with several interfaces that list was most of the
//! payload, and what it cost was a denser QR code, the kind that will not scan
//! off a small screen.

use std::{fmt, str::FromStr};

use iroh::EndpointId;
use moq_net::PathOwned;
use n0_error::e;
use serde::{Deserialize, Serialize};

use crate::{Error, path::live_path};

/// URI scheme prefix of a broadcast ticket.
const SCHEME: &str = "iroh-live:";

/// The length of the raw endpoint id a ticket encodes.
const ENDPOINT_ID_LEN: usize = 32;

/// A peer and the name of one of its broadcasts.
///
/// Carries the endpoint id and no addresses; iroh's address lookup resolves
/// them. The string form is `iroh-live:<base64url(endpoint id)>/<name>`, and
/// parsing also accepts it without the scheme. Serde goes through the string
/// form.
///
/// # Examples
///
/// ```
/// use iroh_moq::BroadcastTicket;
///
/// let peer = iroh::SecretKey::generate().public();
/// let ticket = BroadcastTicket::new(peer, "studio");
/// let parsed: BroadcastTicket = ticket.to_string().parse()?;
/// assert_eq!(parsed, ticket);
/// assert_eq!(ticket.path().as_str(), format!("live/{peer}/studio"));
/// # Ok::<(), iroh_moq::Error>(())
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
        live_path(self.peer, &self.name)
    }

    fn parse_uri(rest: &str) -> Result<Self, Error> {
        let (id, name) = rest
            .split_once('/')
            .ok_or_else(|| invalid("missing / separator"))?;
        let bytes = data_encoding::BASE64URL_NOPAD
            .decode(id.as_bytes())
            .map_err(|_| invalid("invalid base64url"))?;
        let id = <&[u8; ENDPOINT_ID_LEN]>::try_from(bytes.as_slice())
            .map_err(|_| invalid("not an endpoint id"))?;
        let peer = EndpointId::from_bytes(id).map_err(|_| invalid("invalid endpoint id"))?;
        Ok(Self::new(peer, name))
    }
}

fn invalid(reason: &str) -> Error {
    e!(Error::InvalidTicket {
        reason: reason.to_owned()
    })
}

impl fmt::Display for BroadcastTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = data_encoding::BASE64URL_NOPAD.encode(self.peer.as_bytes());
        write!(f, "{SCHEME}{id}/{}", self.name)
    }
}

impl FromStr for BroadcastTicket {
    type Err = Error;

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

impl iroh_tickets::Ticket for BroadcastTicket {
    const KIND: &'static str = "broadcast";

    /// Encodes the 32 bytes of the endpoint id followed by the name.
    fn encode_bytes(&self) -> Vec<u8> {
        let mut bytes = self.peer.as_bytes().to_vec();
        bytes.extend_from_slice(self.name.as_bytes());
        bytes
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, iroh_tickets::ParseError> {
        let too_short = || iroh_tickets::ParseError::verification_failed("ticket too short");
        let id: &[u8; ENDPOINT_ID_LEN] = bytes
            .get(..ENDPOINT_ID_LEN)
            .and_then(|id| id.try_into().ok())
            .ok_or_else(too_short)?;
        let peer = EndpointId::from_bytes(id)
            .map_err(|_| iroh_tickets::ParseError::verification_failed("invalid endpoint id"))?;
        let name = std::str::from_utf8(&bytes[ENDPOINT_ID_LEN..])
            .map_err(|_| iroh_tickets::ParseError::verification_failed("name is not UTF-8"))?;
        Ok(Self::new(peer, name))
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;
    use iroh_tickets::Ticket;

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
    }

    #[test]
    fn a_ticket_qr_stays_sparse() {
        // A QR code holds 84 bytes in 37 modules at the default error
        // correction level, three pixels each on the 122 px e-paper panel the
        // Pi Zero demo draws on.
        let ticket = BroadcastTicket::new(test_endpoint_id(), "my-stream-name");
        assert!(ticket.to_string().len() <= 84);
    }

    #[test]
    fn serde_goes_through_the_string() {
        let ticket = BroadcastTicket::new(test_endpoint_id(), "cam");
        let json = serde_json_like(&ticket);
        assert_eq!(json, format!("\"{ticket}\""));
    }

    /// Round-trips `ticket` through postcard and returns its string form, quoted.
    ///
    /// Postcard is where a derive would have leaked the struct shape.
    fn serde_json_like(ticket: &BroadcastTicket) -> String {
        let bytes = postcard::to_stdvec(ticket).expect("serialize");
        let decoded: String = postcard::from_bytes(&bytes).expect("a string");
        let back: BroadcastTicket = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(&back, ticket);
        format!("\"{decoded}\"")
    }

    #[test]
    fn the_iroh_ticket_form_round_trips() {
        let ticket = BroadcastTicket::new(test_endpoint_id(), "studio/main");
        let encoded = ticket.encode_string();
        assert!(encoded.starts_with("broadcast"), "{encoded}");
        assert_eq!(
            BroadcastTicket::decode_string(&encoded).expect("decode"),
            ticket
        );
    }

    #[test]
    fn the_path_names_the_publisher() {
        let peer = test_endpoint_id();
        let ticket = BroadcastTicket::new(peer, "cam");
        assert_eq!(ticket.path().as_str(), format!("live/{peer}/cam"));
    }
}
