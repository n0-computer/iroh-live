//! The ticket that identifies a room and bootstraps its gossip topic.

use std::{env, str::FromStr};

use iroh::EndpointId;
use iroh_gossip::TopicId;
use n0_error::{Result, StdResultExt};
use serde::{Deserialize, Serialize};

/// Ticket for joining a room.
///
/// Contains the gossip topic ID and optional bootstrap peer IDs.
/// Serializes to a compact string via the `iroh_tickets` crate.
#[derive(Debug, Serialize, Deserialize, Clone, derive_more::Display)]
#[display("{}", iroh_tickets::Ticket::encode_string(self))]
pub struct RoomTicket {
    /// Peers to contact initially for gossip bootstrap.
    pub bootstrap: Vec<EndpointId>,
    /// The gossip topic that identifies this room.
    pub topic_id: TopicId,
}

impl RoomTicket {
    /// Creates a ticket with the given topic and bootstrap peers.
    pub fn new(topic_id: TopicId, bootstrap: impl IntoIterator<Item = EndpointId>) -> Self {
        Self {
            bootstrap: bootstrap.into_iter().collect(),
            topic_id,
        }
    }

    /// Generates a new room with a random topic ID and no bootstrap peers.
    pub fn generate() -> Self {
        Self {
            bootstrap: vec![],
            topic_id: TopicId::from_bytes(rand::random()),
        }
    }

    /// Creates a ticket from environment variables.
    ///
    /// Reads `IROH_LIVE_ROOM` for a full ticket string, or
    /// `IROH_LIVE_TOPIC` for a hex-encoded topic ID. Generates a
    /// random topic if neither is set.
    pub fn new_from_env() -> Result<Self> {
        if let Ok(value) = env::var("IROH_LIVE_ROOM") {
            value
                .parse()
                .std_context("failed to parse ticket from IROH_LIVE_ROOM environment variable")
        } else {
            let topic_id = match env::var("IROH_LIVE_TOPIC") {
                Ok(topic) => TopicId::from_bytes(
                    data_encoding::HEXLOWER
                        .decode(topic.as_bytes())
                        .std_context("invalid hex")?
                        .as_slice()
                        .try_into()
                        .std_context("invalid length")?,
                ),
                Err(_) => {
                    let topic = TopicId::from_bytes(rand::random());
                    println!(
                        "Created new topic. Reuse with IROH_LIVE_TOPIC={}",
                        data_encoding::HEXLOWER.encode(topic.as_bytes())
                    );
                    topic
                }
            };
            Ok(Self::new(topic_id, vec![]))
        }
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
