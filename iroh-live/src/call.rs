use std::str::FromStr;

use iroh::{EndpointAddr, EndpointId, endpoint::ConnectionError};
use iroh_moq::MoqSession;
use moq_media::{publish::LocalBroadcast, subscribe::RemoteBroadcast};
use n0_error::{AnyError, Result, StdResultExt, stack_error};
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
/// Wraps an [`EndpointAddr`] with a fixed broadcast name (`call`).
/// Serializes to an `iroh-live:` URI via [`LiveTicket`](crate::ticket::LiveTicket).
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

    /// Serializes to an `iroh-live:` URI with broadcast name `call`.
    fn serialize(&self) -> String {
        crate::ticket::LiveTicket::new(self.endpoint.clone(), CALL_BROADCAST_NAME)
            .with_relay_urls(self.relay_urls.clone())
            .serialize()
    }

    /// Deserializes from a string. Accepts `iroh-live:` URIs and the
    /// legacy base32 format.
    pub fn deserialize(s: &str) -> Result<Self> {
        let s = s.trim();
        // Try URI format (iroh-live:...) or legacy LiveTicket format (name@...).
        if s.starts_with(crate::ticket::SCHEME) || s.contains('@') {
            let ticket = crate::ticket::LiveTicket::deserialize(s)?;
            return Ok(Self {
                endpoint: ticket.endpoint,
                relay_urls: ticket.relay_urls,
            });
        }
        // Legacy CallTicket format: plain base32.
        let bytes = data_encoding::BASE32_NOPAD_NOCASE
            .decode(s.as_bytes())
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
        let session = live
            .transport()
            .connect(remote)
            .await
            .map_err(CallError::ConnectionFailed)?;
        Self::setup(session, local).await
    }

    /// Accepts an incoming session as a call.
    ///
    /// The `local` broadcast should have video/audio already configured.
    /// It is published on the session as "call" — do **not** also publish
    /// it via [`Live::publish`].
    pub async fn accept(session: MoqSession, local: LocalBroadcast) -> Result<Self, CallError> {
        Self::setup(session, local).await
    }

    /// Publishes the local broadcast and subscribes to the remote's "call"
    /// broadcast. Shared setup for both [`dial`](Self::dial) and
    /// [`accept`](Self::accept).
    async fn setup(mut session: MoqSession, local: LocalBroadcast) -> Result<Self, CallError> {
        session.publish(CALL_BROADCAST_NAME, local.consume());

        let consumer = session
            .subscribe(CALL_BROADCAST_NAME)
            .await
            .map_err(|err| CallError::Rejected(err.into()))?;
        let remote = RemoteBroadcast::new(CALL_BROADCAST_NAME, consumer)
            .await
            .map_err(CallError::Rejected)?;

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

    /// Waits until the call ends and returns the disconnect reason.
    ///
    /// Inspects the QUIC connection's close reason to distinguish local
    /// close, remote close, and transport errors.
    pub async fn closed(&self) -> DisconnectReason {
        let _ = self.session.closed().await;
        match self.session.conn().close_reason() {
            Some(ConnectionError::LocallyClosed) => DisconnectReason::LocalClose,
            Some(ConnectionError::ApplicationClosed(_) | ConnectionError::ConnectionClosed(_)) => {
                DisconnectReason::RemoteClose
            }
            Some(ConnectionError::Reset) => DisconnectReason::RemoteClose,
            Some(_) => DisconnectReason::TransportError,
            // Session closed but no close reason yet — likely remote.
            None => DisconnectReason::RemoteClose,
        }
    }
}
