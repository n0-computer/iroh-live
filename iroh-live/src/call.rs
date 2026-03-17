use std::str::FromStr;

use iroh::{EndpointAddr, EndpointId};
use iroh_moq::MoqSession;
use moq_media::{publish::LocalBroadcast, subscribe::RemoteBroadcast};
use n0_error::{AnyError, Result, StackErrorExt, StdResultExt, stack_error};
use serde::{Deserialize, Serialize};

use crate::{Live, types::DisconnectReason};

/// Errors from call operations.
#[stack_error(derive)]
pub enum CallError {
    #[error("failed to connect")]
    /// Failed to connect to the remote peer.
    ConnectionFailed(#[error(source)] AnyError),
    /// Remote peer rejected the call or closed before subscribing.
    #[error("call rejected")]
    Rejected(#[error(source)] AnyError),
    /// Call ended.
    #[error("call ended ({_0})")]
    Ended(DisconnectReason),
}

/// Standalone 1:1 call helper. Pure sugar over MoQ primitives.
///
/// What it does internally:
/// 1. Connects to remote peer (or accepts incoming session)
/// 2. Publishes the caller's [`LocalBroadcast`] named "call" on the session
/// 3. Subscribes to the remote's broadcast -> [`RemoteBroadcast`]
/// 4. Wraps both in a handle with state tracking
///
/// The caller provides a [`LocalBroadcast`] with video/audio already
/// configured. Call publishes it on the session — do **not** also call
/// [`Live::publish`] with the same name, as that would cause a
/// double-publish conflict.
///
/// Everything Call does can be done directly with
/// [`Live::transport()`] + [`LocalBroadcast`] + [`RemoteBroadcast`].
#[derive(Debug)]
pub struct Call {
    session: MoqSession,
    local: LocalBroadcast,
    remote: RemoteBroadcast,
}

const CALL_BROADCAST_NAME: &str = "call";

/// Shareable ticket for 1:1 calls.
///
/// Wraps an [`EndpointAddr`] with a fixed broadcast name. Serializes to a
/// compact base32 string suitable for copy-paste or QR codes.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display, Serialize, Deserialize)]
#[display("{}", self.serialize())]
pub struct CallTicket {
    /// Remote peer address.
    pub endpoint: EndpointAddr,
    /// Optional relay URLs for indirect connectivity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relay_urls: Vec<String>,
}

impl CallTicket {
    /// Creates a new call ticket for the given endpoint.
    pub fn new(endpoint: impl Into<EndpointAddr>) -> Self {
        Self {
            endpoint: endpoint.into(),
            relay_urls: Vec::new(),
        }
    }

    /// Adds relay URLs to this ticket.
    pub fn with_relay_urls(mut self, urls: impl IntoIterator<Item = String>) -> Self {
        self.relay_urls = urls.into_iter().collect();
        self
    }

    /// Converts this ticket to a [`crate::ticket::LiveTicket`].
    pub fn into_live_ticket(self) -> crate::ticket::LiveTicket {
        crate::ticket::LiveTicket::new(self.endpoint, CALL_BROADCAST_NAME)
            .with_relay_urls(self.relay_urls)
    }

    /// Serializes to a compact string.
    fn serialize(&self) -> String {
        let bytes = postcard::to_stdvec(&self.endpoint).unwrap();
        data_encoding::BASE32_NOPAD
            .encode(&bytes)
            .to_ascii_lowercase()
    }

    /// Deserializes from a string produced by [`serialize`](Self::serialize).
    pub fn deserialize(s: &str) -> Result<Self> {
        let bytes = data_encoding::BASE32_NOPAD_NOCASE
            .decode(s.trim().as_bytes())
            .std_context("invalid base32")?;
        let endpoint =
            postcard::from_bytes(&bytes).std_context("failed to parse endpoint address")?;
        Ok(Self {
            endpoint,
            relay_urls: Vec::new(),
        })
    }
}

impl From<CallTicket> for EndpointAddr {
    fn from(ticket: CallTicket) -> Self {
        ticket.endpoint
    }
}

impl FromStr for CallTicket {
    type Err = AnyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::deserialize(s)
    }
}

impl Call {
    /// Dials a remote peer. Connects, publishes the local broadcast, subscribes to remote.
    ///
    /// The `local` broadcast should have video/audio already configured.
    /// It is published on the session as "call" — do **not** also publish
    /// it via [`Live::publish`].
    pub async fn dial(
        live: &Live,
        remote: impl Into<EndpointAddr>,
        local: LocalBroadcast,
    ) -> Result<Self, CallError> {
        let mut session = live
            .transport()
            .connect(remote)
            .await
            .map_err(CallError::ConnectionFailed)?;

        session.publish(CALL_BROADCAST_NAME.to_string(), local.consume());

        let consumer = session
            .subscribe(CALL_BROADCAST_NAME)
            .await
            .map_err(|err| CallError::Rejected(err.into()))?;
        let remote = RemoteBroadcast::new(CALL_BROADCAST_NAME.to_string(), consumer)
            .await
            .map_err(CallError::Rejected)?;

        Ok(Self {
            session,
            local,
            remote,
        })
    }

    /// Accepts an incoming session as a call.
    ///
    /// The `local` broadcast should have video/audio already configured.
    /// It is published on the session as "call" — do **not** also publish
    /// it via [`Live::publish`].
    pub async fn accept(mut session: MoqSession, local: LocalBroadcast) -> Result<Self, CallError> {
        session.publish(CALL_BROADCAST_NAME.to_string(), local.consume());

        let consumer = session
            .subscribe(CALL_BROADCAST_NAME)
            .await
            .map_err(|err| CallError::Rejected(err.context("subscribe failed")))?;
        let remote = RemoteBroadcast::new(CALL_BROADCAST_NAME.to_string(), consumer)
            .await
            .map_err(|err| CallError::Rejected(err.context("establish failed")))?;

        Ok(Self {
            session,
            local,
            remote,
        })
    }

    /// Returns the local broadcast (configure video/audio here).
    pub fn local(&self) -> &LocalBroadcast {
        &self.local
    }

    /// Returns the remote broadcast (subscribe to video/audio here).
    pub fn remote(&self) -> &RemoteBroadcast {
        &self.remote
    }

    /// Returns the remote peer's endpoint ID.
    pub fn remote_id(&self) -> EndpointId {
        self.session.remote_id()
    }

    /// Returns a reference to the underlying [`MoqSession`].
    pub fn session(&self) -> &MoqSession {
        &self.session
    }

    /// Closes the call, ending the session.
    pub fn close(&self) {
        self.session.close(0u32, b"call ended");
    }

    /// Waits until the call ends.
    pub async fn closed(&self) -> DisconnectReason {
        let _ = self.session.closed().await;
        DisconnectReason::RemoteClose
    }
}
