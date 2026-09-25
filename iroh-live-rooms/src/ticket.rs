//! The ticket that identifies a room and bootstraps its gossip topic.

use std::str::FromStr;

use iroh::EndpointId;
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};

/// A room: its gossip topic and the peers to contact first.
///
/// Its string form is an `iroh_tickets` ticket of kind `room`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, derive_more::Display)]
#[display("{}", iroh_tickets::Ticket::encode_string(self))]
pub struct RoomTicket {
    bootstrap: Vec<EndpointId>,
    topic_id: TopicId,
}

impl RoomTicket {
    /// Creates a ticket for `topic_id`, bootstrapping from `bootstrap`.
    pub fn new(topic_id: TopicId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
        Self {
            bootstrap: bootstrap.into_iter().collect(),
            topic_id,
        }
    }

    /// Creates a ticket for a new room with a random topic.
    pub fn generate() -> Self {
        Self::new(TopicId::from_bytes(rand::random()), [])
    }

    /// Returns the gossip topic that identifies the room.
    pub fn topic_id(&self) -> TopicId {
        self.topic_id
    }

    /// Returns the peers a joiner contacts first.
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
    const KIND: &'static str = "room";

    fn encode_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("RoomTicket serialization is infallible")
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, iroh_tickets::ParseError> {
        let ticket = postcard::from_bytes(bytes)?;
        Ok(ticket)
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
        assert!(ticket.to_string().starts_with("room"));
        assert_eq!(parsed.bootstrap(), [peer]);
    }
}
