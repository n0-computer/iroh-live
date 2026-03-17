use std::str::FromStr;

use iroh::EndpointAddr;
use n0_error::{Result, StdResultExt};
use serde::{Deserialize, Serialize};

/// Ticket for subscribing to a live broadcast.
///
/// Contains the publisher's endpoint address, the broadcast name, and
/// optional relay URLs for reaching the publisher through a relay when
/// direct P2P is unavailable.
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

    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let ticket = postcard::from_bytes(bytes).std_context("failed to deserialize")?;
        Ok(ticket)
    }

    /// Serialize to string.
    pub fn serialize(&self) -> String {
        let mut out = self.broadcast_name.clone();
        out.push('@');
        data_encoding::BASE32_NOPAD
            .encode_append(&postcard::to_stdvec(&self.endpoint).unwrap(), &mut out);
        out.to_ascii_lowercase()
    }

    /// Deserialize from a string.
    pub fn deserialize(str: &str) -> Result<Self> {
        let (broadcast_name, encoded_addr) = str
            .split_once("@")
            .std_context("invalid ticket: missing @")?;
        let endpoint_addr: EndpointAddr = postcard::from_bytes(
            &(data_encoding::BASE32_NOPAD_NOCASE
                .decode(encoded_addr.as_bytes())
                .std_context("invalid base32")?),
        )
        .std_context("failed to parse")?;
        Ok(Self {
            broadcast_name: broadcast_name.to_string(),
            endpoint: endpoint_addr,
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
