use iroh::EndpointAddr;
use n0_error::{Result, StdResultExt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display, Serialize, Deserialize)]
#[display("{}", self.serialize())]
pub struct LiveTicket {
    pub endpoint: EndpointAddr,
    pub broadcast_name: String,
}

impl LiveTicket {
    pub fn new(endpoint: impl Into<EndpointAddr>, broadcast_name: impl ToString) -> Self {
        Self {
            endpoint: endpoint.into(),
            broadcast_name: broadcast_name.to_string(),
        }
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
        })
    }
}

impl std::str::FromStr for LiveTicket {
    type Err = n0_error::AnyError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::deserialize(s)
    }
}
