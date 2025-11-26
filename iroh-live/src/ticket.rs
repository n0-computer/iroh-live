use iroh::EndpointId;
use n0_error::{Result, StdResultExt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display, Serialize, Deserialize)]
#[display("{}", self.serialize())]
pub struct LiveTicket {
    pub endpoint_id: EndpointId,
    pub broadcast_name: String,
}

impl LiveTicket {
    pub fn new(endpoint_id: EndpointId, broadcast_name: impl ToString) -> Self {
        Self {
            endpoint_id,
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
        out.push_str("@");
        data_encoding::BASE32_NOPAD.encode_append(self.endpoint_id.as_bytes(), &mut out);
        out.to_ascii_lowercase()
    }

    /// Deserialize from a string.
    pub fn deserialize(str: &str) -> Result<Self> {
        let (broadcast_name, endpoint_id) = str
            .split_once("@")
            .std_context("invalid ticket: missing @")?;
        let endpoint_id: EndpointId = endpoint_id.parse()?;
        Ok(Self {
            broadcast_name: broadcast_name.to_string(),
            endpoint_id,
        })
    }
}
