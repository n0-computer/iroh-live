//! The ticket that identifies a room and bootstraps its gossip topic.

use std::str::FromStr;

use iroh::EndpointId;
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};

/// A room: its gossip topic and the peers to contact first.
///
/// Its string form is an `iroh_tickets` ticket of kind `room2`. The kind
/// names the version of the room's gossip messages, which carry no version of
/// their own, so a ticket of an earlier format fails to parse instead of
/// joining a room whose messages its peers cannot read.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display, Serialize, Deserialize)]
#[display("{}", iroh_tickets::Ticket::encode_string(self))]
pub struct RoomTicket {
    bootstrap: Vec<EndpointId>,
    topic_id: TopicId,
}

impl RoomTicket {
    /// Creates a ticket for `topic_id`, bootstrapping from `bootstrap`.
    #[must_use]
    pub fn new(topic_id: TopicId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
        Self {
            bootstrap: bootstrap.into_iter().collect(),
            topic_id,
        }
    }

    /// Creates a ticket for a new room with a random topic.
    #[must_use]
    pub fn generate() -> Self {
        Self::new(TopicId::from_bytes(rand::random()), [])
    }

    /// Returns the gossip topic that identifies the room.
    #[must_use]
    pub fn topic_id(&self) -> TopicId {
        self.topic_id
    }

    /// Returns the peers a joiner contacts first.
    #[must_use]
    pub fn bootstrap(&self) -> &[EndpointId] {
        &self.bootstrap
    }

    /// Returns this ticket with `peer` as its only bootstrap peer.
    pub(crate) fn with_bootstrap(&self, peer: EndpointId) -> Self {
        Self::new(self.topic_id, [peer])
    }
}

impl FromStr for RoomTicket {
    type Err = iroh_tickets::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        iroh_tickets::Ticket::decode_string(s)
    }
}

impl iroh_tickets::Ticket for RoomTicket {
    /// Bumped from `room` when the gossip announcements changed format.
    const KIND: &'static str = "room2";

    fn encode_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("RoomTicket serialization is infallible")
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, iroh_tickets::ParseError> {
        Ok(postcard::from_bytes(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_round_trips_through_its_string() {
        let peer = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let ticket = RoomTicket::generate().with_bootstrap(peer);
        let parsed: RoomTicket = ticket.to_string().parse().expect("parse");
        assert_eq!(parsed, ticket);
        assert!(ticket.to_string().starts_with("room2"));
        assert_eq!(parsed.bootstrap(), [peer]);
    }

    /// A ticket of the earlier `room` kind fails with a kind error, not a room that cannot talk.
    #[test]
    fn a_ticket_of_the_earlier_format_is_refused_by_kind() {
        let peer = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let ticket = RoomTicket::generate().with_bootstrap(peer);
        // The same bytes under the earlier kind.
        let encoded = ticket.to_string();
        let stale = format!("room{}", encoded.strip_prefix("room2").expect("the kind"));
        let err = stale.parse::<RoomTicket>().expect_err("a stale ticket");
        assert!(
            matches!(
                err,
                iroh_tickets::ParseError::Kind {
                    expected: "room2",
                    ..
                }
            ),
            "{err}"
        );
    }
}
