//! Tickets that name a broadcast by its publisher and its path.
//!
//! A ticket holds no socket addresses. Subscribers look them up from the
//! endpoint id over pkarr and mDNS, which keeps the ticket short enough for a
//! QR code on a small screen.

use std::{fmt, str::FromStr};

use iroh::EndpointId;
use moq_net::Path;
use n0_error::e;
use serde::{Deserialize, Serialize};

use crate::{Error, publish::check_path};

/// The scheme of a ticket string, with its version.
const SCHEME: &str = "ilv1:";

/// The segment that stands for the publisher's own id.
const SELF_SEGMENT: &str = "~";

/// A publisher's endpoint id and the path of one of its broadcasts.
///
/// The string form is `ilv1:<peer>/<path>`. `<peer>` is the endpoint id in
/// lowercase base32. Any segment of the path that equals the peer's own id is
/// written as `~`, and every other byte outside `[A-Za-z0-9._-]` is
/// percent-encoded. Parsing accepts the id in base32 of either case, or in lowercase hex.
/// Serde uses the string form.
///
/// A ticket says nothing about layouts: the path is whatever the publisher
/// published at.
///
/// # Examples
///
/// ```
/// use iroh_live_node::BroadcastTicket;
/// use moq_net::Path;
///
/// let peer = iroh::SecretKey::generate().public();
/// let path = Path::new(&format!("live/{peer}/studio"))?;
/// let ticket = BroadcastTicket::at(peer, path);
/// assert!(ticket.to_string().ends_with("/live/~/studio"));
/// let parsed: BroadcastTicket = ticket.to_string().parse()?;
/// assert_eq!(parsed, ticket);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BroadcastTicket {
    peer: EndpointId,
    path: Path,
}

impl BroadcastTicket {
    /// The most bytes a ticket string may have.
    ///
    /// A relay mounts the string as one path, and moq allows 4096 bytes in one.
    pub const MAX_LEN: usize = 4000;

    /// The most path segments a ticket may have.
    ///
    /// A relay mounts the string as one more segment, and moq allows 32.
    pub const MAX_PARTS: usize = 31;

    /// Creates a ticket for the broadcast at `path` of `peer`.
    ///
    /// A path that is too long for the string form still displays, but does
    /// not parse back.
    #[must_use]
    pub fn at(peer: EndpointId, path: Path) -> Self {
        Self { peer, path }
    }

    /// Returns the publisher's endpoint id.
    #[must_use]
    pub fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Returns the path the broadcast is published at.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn invalid(reason: &'static str) -> Error {
    e!(Error::InvalidTicket { reason })
}

/// Reports whether `byte` is written as itself in a ticket path.
fn is_plain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

/// Appends `segment` to `out`, percent-encoding what is not plain.
fn push_encoded(out: &mut String, segment: &str) {
    for byte in segment.bytes() {
        if is_plain(byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
}

/// Reads one percent-encoded segment, exactly once.
fn decode_segment(segment: &str) -> Result<String, Error> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = segment
                .get(at + 1..at + 3)
                .ok_or_else(|| invalid("truncated percent escape"))?;
            let byte =
                u8::from_str_radix(hex, 16).map_err(|_| invalid("invalid percent escape"))?;
            out.push(byte);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).map_err(|_| invalid("path is not UTF-8"))
}

impl fmt::Display for BroadcastTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = data_encoding::BASE32_NOPAD
            .encode(self.peer.as_bytes())
            .to_ascii_lowercase();
        let own = self.peer.to_string();
        let mut path = String::new();
        for (index, segment) in self.path.as_str().split('/').enumerate() {
            if index > 0 {
                path.push('/');
            }
            if segment == own {
                path.push_str(SELF_SEGMENT);
            } else {
                push_encoded(&mut path, segment);
            }
        }
        write!(f, "{SCHEME}{id}/{path}")
    }
}

impl FromStr for BroadcastTicket {
    type Err = Error;

    /// Parses the string form, with a case-insensitive scheme and peer id.
    ///
    /// Fails with [`Error::InvalidTicket`] if the string is not a ticket, has
    /// more than 31 path segments or 4000 bytes, or its path is not valid.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() > Self::MAX_LEN {
            return Err(invalid("ticket is too long"));
        }
        let rest = s
            .get(..SCHEME.len())
            .filter(|scheme| scheme.eq_ignore_ascii_case(SCHEME))
            .map(|_| &s[SCHEME.len()..])
            .ok_or_else(|| invalid("missing ilv1: scheme"))?;
        let (id, path) = rest
            .split_once('/')
            .ok_or_else(|| invalid("missing / separator"))?;
        let peer = EndpointId::from_str(id).map_err(|_| invalid("invalid endpoint id"))?;
        let own = peer.to_string();
        let segments = path
            .split('/')
            .map(|segment| {
                if segment.is_empty() {
                    Err(invalid("empty path segment"))
                } else if segment == SELF_SEGMENT {
                    Ok(own.clone())
                } else {
                    decode_segment(segment)
                }
            })
            .collect::<Result<Vec<_>, Error>>()?;
        if segments.len() > Self::MAX_PARTS {
            return Err(invalid("too many path segments"));
        }
        let path = Path::new(&segments.join("/")).map_err(|_| invalid("invalid path"))?;
        check_path(&path).map_err(|_| invalid("invalid path"))?;
        Ok(Self { peer, path })
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
    use serde::de::{IntoDeserializer, value::Error};

    use super::*;

    fn ticket(peer: EndpointId, path: &str) -> BroadcastTicket {
        BroadcastTicket::at(peer, Path::new(path).expect("a path"))
    }

    fn id() -> EndpointId {
        SecretKey::generate().public()
    }

    #[test]
    fn a_ticket_round_trips() {
        let peer = id();
        let original = ticket(peer, &format!("live/{peer}/studio"));
        let text = original.to_string();
        assert!(text.starts_with("ilv1:"), "{text}");
        assert!(text.ends_with("/live/~/studio"), "{text}");
        assert_eq!(text.len(), 5 + 52 + "/live/~/studio".len());
        assert_eq!(text.parse::<BroadcastTicket>().expect("parse"), original);
    }

    #[test]
    fn reserved_characters_round_trip() {
        let peer = id();
        let other = id();
        for path in [
            "a/b c/d%e".to_string(),
            "tilde/~/x".to_string(),
            "uni/caf\u{e9}/\u{1f600}".to_string(),
            format!("rooms/{other}/{peer}/cam"),
            "q/a?b=c#d/e+f".to_string(),
        ] {
            let original = ticket(peer, &path);
            let text = original.to_string();
            assert!(text.is_ascii(), "{text}");
            let parsed = text.parse::<BroadcastTicket>().expect(&text);
            assert_eq!(parsed, original, "{text}");
        }
        let tilde = ticket(peer, "tilde/~/x").to_string();
        assert!(tilde.ends_with("/tilde/%7E/x"), "{tilde}");
    }

    #[test]
    fn the_peer_reads_in_base32_of_either_case_and_in_hex() {
        let peer = id();
        let canonical = ticket(peer, "a/b").to_string();
        let (_, path) = canonical.split_once('/').expect("separator");
        for spelling in [
            peer.to_string(),
            data_encoding::BASE32_NOPAD
                .encode(peer.as_bytes())
                .to_ascii_lowercase(),
            data_encoding::BASE32_NOPAD.encode(peer.as_bytes()),
        ] {
            let text = format!("ILV1:{spelling}/{path}");
            assert_eq!(text.parse::<BroadcastTicket>().expect(&text).peer(), peer);
        }
    }

    #[test]
    fn a_decoded_escape_stays_a_literal() {
        let peer = id();
        let text = format!("ilv1:{}/a/%7E/b", &ticket(peer, "x").to_string()[5..57]);
        let parsed = text.parse::<BroadcastTicket>().expect("parse");
        assert_eq!(parsed.path().as_str(), "a/~/b");
    }

    #[test]
    fn garbage_is_rejected() {
        let peer = id();
        let good = ticket(peer, "a/b").to_string();
        let id_part = &good[5..57];
        for bad in [
            "not-a-ticket".to_string(),
            "iroh-live:abc/x".to_string(),
            format!("ilv1:{id_part}"),
            format!("ilv1:{id_part}/"),
            format!("ilv1:{id_part}/a//b"),
            format!("ilv1:{id_part}/a/%zz"),
            format!("ilv1:{id_part}/a/%4"),
            format!("ilv1:{id_part}/a/*/b"),
            format!("ilv1:{id_part}/%FF"),
            "ilv1:short/a".to_string(),
        ] {
            assert!(bad.parse::<BroadcastTicket>().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_ticket_over_the_cap_displays_but_does_not_parse() {
        let peer = id();
        let deep = vec!["s"; 32].join("/");
        let long = ticket(peer, &deep);
        assert!(long.to_string().parse::<BroadcastTicket>().is_err());
        let ok = ticket(peer, &vec!["s"; 31].join("/"));
        assert_eq!(
            ok.to_string().parse::<BroadcastTicket>().expect("parse"),
            ok
        );
        let wide = ticket(peer, &vec!["\u{e9}".repeat(100); 10].join("/"));
        assert!(wide.to_string().len() > BroadcastTicket::MAX_LEN);
        assert!(wide.to_string().parse::<BroadcastTicket>().is_err());
    }

    #[test]
    fn the_peers_own_segment_is_a_tilde() {
        let peer = SecretKey::from_bytes(&[7; 32]).public();
        let text = ticket(peer, &format!("live/{peer}/studio")).to_string();
        let id = data_encoding::BASE32_NOPAD
            .encode(peer.as_bytes())
            .to_ascii_lowercase();
        assert_eq!(text, format!("ilv1:{id}/live/~/studio"));
    }

    #[test]
    fn serde_reads_the_string_form() {
        let original = ticket(id(), "cam");
        let text = original.to_string();
        let back = BroadcastTicket::deserialize(IntoDeserializer::<Error>::into_deserializer(
            text.as_str(),
        ))
        .expect("deserialize");
        assert_eq!(back, original);
    }
}
