//! The path layout: where a node's broadcasts live, and who publishes a path.
//!
//! A broadcast's path names its publisher, so the same broadcast has the same
//! path over every link and a route table can see several routes to it:
//!
//! ```text
//! live/<publisher endpoint id>/<name>            a node's own broadcasts
//! rooms/<topic>/<publisher endpoint id>/<name>   broadcasts published into a room
//! ```

use iroh::EndpointId;
use moq_net::{Hop, Path, PathOwned};

/// The namespace a node's own broadcasts are published under.
pub(crate) const LIVE: &str = "live";

/// The namespace room broadcasts are published under.
const ROOMS: &str = "rooms";

/// Returns the path `live/<publisher>/<name>` a broadcast is published at.
pub fn live_path(publisher: EndpointId, name: &str) -> PathOwned {
    Path::new(&format!("{LIVE}/{publisher}/{name}")).to_owned()
}

/// Returns the endpoint that publishes `path`, if the path follows the layout.
///
/// Recognizes `live/<id>/...` and `rooms/<topic>/<id>/...`. Any other path names
/// no publisher, and resolving it takes a route that already exists or an
/// explicit session.
pub fn publisher_of(path: &Path<'_>) -> Option<EndpointId> {
    let mut parts = path.parts();
    let id = match parts.next()? {
        LIVE => parts.next()?,
        ROOMS => {
            parts.next()?;
            parts.next()?
        }
        _ => return None,
    };
    // Only a name beneath the id makes it a broadcast path.
    parts.next()?;
    id.parse().ok()
}

/// Returns the moq hop id of the node with endpoint id `id`.
///
/// Derived rather than drawn at random per start, so a relay that saw this node
/// before recognizes its routes after a restart. An endpoint id is an ed25519
/// public key, whose bytes are already spread evenly, so the first eight are
/// used as they are. Truncated below 2^53, the bound moq documents for the
/// JavaScript clients, which read hop ids as numbers. Zero names nobody in moq,
/// so it maps to one.
pub(crate) fn hop_for(id: &EndpointId) -> Hop {
    let bytes: [u8; 8] = id.as_bytes()[..8]
        .try_into()
        .expect("an endpoint id is 32 bytes");
    let value = u64::from_le_bytes(bytes) & ((1u64 << 53) - 1);
    Hop::new(value.max(1)).expect("non-zero and below 2^62")
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    fn id() -> EndpointId {
        SecretKey::generate().public()
    }

    #[test]
    fn a_live_path_names_its_publisher() {
        let publisher = id();
        let path = live_path(publisher, "cam");
        assert_eq!(path.as_str(), format!("live/{publisher}/cam"));
        assert_eq!(publisher_of(&path), Some(publisher));
    }

    #[test]
    fn a_room_path_names_its_publisher() {
        let publisher = id();
        let path = Path::new(&format!("rooms/topic/{publisher}/cam")).to_owned();
        assert_eq!(publisher_of(&path), Some(publisher));
    }

    #[test]
    fn other_paths_name_no_publisher() {
        let publisher = id();
        for path in [
            "cam".to_owned(),
            format!("live/{publisher}"),
            "live/not-an-id/cam".to_owned(),
            format!("calls/{publisher}"),
        ] {
            assert_eq!(publisher_of(&Path::new(&path)), None, "{path}");
        }
    }

    #[test]
    fn the_hop_is_stable_and_in_range() {
        let publisher = id();
        let hop = hop_for(&publisher);
        assert_eq!(hop, hop_for(&publisher), "the same id gives the same hop");
        assert!(hop.id() > 0 && hop.id() < 1 << 53, "{}", hop.id());
        assert_ne!(hop, hop_for(&id()), "two ids give two hops");
    }
}
