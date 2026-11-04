//! The `live/<endpoint id>/<name>` path layout.
//!
//! This is the default layout of [`Live`](crate::Live): a node publishes under
//! `live/<its id>/`, and a peer may publish only there. Tickets, the default
//! grant and the call helpers all build and read paths through this module, so
//! the layout is in one place. An application with another layout publishes
//! through [`Live::publish_at`](crate::Live::publish_at) and ignores it.
//!
//! Rooms paths (`rooms/…`) belong to `iroh-live-rooms`.

use iroh::EndpointId;
use iroh_moq::net::Pattern;

/// The first segment of every path in this layout.
const ROOT: &str = "live";

/// Returns the path of `peer`'s broadcast `name`: `live/<peer>/<name>`.
///
/// A string, since a name may not make a valid moq path; publishing or
/// subscribing at it says so.
///
/// ```
/// let peer = iroh::SecretKey::generate().public();
/// assert_eq!(iroh_live::paths::broadcast(peer, "cam"), format!("live/{peer}/cam"));
/// ```
pub fn broadcast(peer: EndpointId, name: &str) -> String {
    format!("{ROOT}/{peer}/{name}")
}

/// Returns the path of `peer`'s side of a call: `live/<peer>/call`.
pub fn call(peer: EndpointId) -> String {
    broadcast(peer, crate::CALL)
}

/// Splits a `live/<peer>/<name>` path into the peer and the name.
///
/// The inverse of [`broadcast`]. Returns `None` for a path elsewhere, one
/// whose second segment is not an endpoint id, or one without a name.
pub fn parse(path: &str) -> Option<(EndpointId, &str)> {
    let (peer, name) = path
        .strip_prefix(ROOT)?
        .strip_prefix('/')?
        .split_once('/')?;
    if name.is_empty() {
        return None;
    }
    Some((peer.parse().ok()?, name))
}

/// Returns the paths `peer` publishes its own broadcasts under: `live/<peer>/**`.
pub fn publish_scope(peer: EndpointId) -> Pattern {
    format!("{ROOT}/{peer}/**")
        .parse()
        .expect("an endpoint id is a valid path segment")
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn parse_inverts_broadcast() {
        let peer = SecretKey::generate().public();
        assert_eq!(parse(&broadcast(peer, "cam")), Some((peer, "cam")));
        assert_eq!(parse(&broadcast(peer, "cam/hd")), Some((peer, "cam/hd")));
        assert_eq!(parse(&call(peer)), Some((peer, "call")));
        assert_eq!(parse(&format!("live/{peer}/")), None);
        assert_eq!(parse(&format!("rooms/{peer}/cam")), None);
        assert_eq!(parse(&format!("livestock/{peer}/cam")), None);
        assert_eq!(parse("live/nobody/cam"), None);
    }

    #[test]
    fn the_scope_matches_only_the_peers_own_paths() {
        let (a, b) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let grant = crate::grant(a);
        assert!(grant.allows_publish(&broadcast(a, "cam")));
        assert!(!grant.allows_publish(&broadcast(b, "cam")));
    }
}
