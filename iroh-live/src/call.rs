use iroh::{EndpointAddr, EndpointId};
use iroh_moq::MoqSession;
use moq_media::{publish::LocalBroadcast, subscribe::RemoteBroadcast};
use n0_error::Result;

use crate::{Live, types::DisconnectReason};

/// Errors from call operations.
#[derive(Debug)]
pub enum CallError {
    /// Failed to connect to the remote peer.
    ConnectionFailed(n0_error::AnyError),
    /// Remote peer rejected the call or closed before subscribing.
    Rejected,
    /// Call ended.
    Ended(DisconnectReason),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(e) => write!(f, "connection failed: {e}"),
            Self::Rejected => write!(f, "call rejected"),
            Self::Ended(r) => write!(f, "call ended: {r}"),
        }
    }
}
impl std::error::Error for CallError {}

/// Standalone 1:1 call helper. Pure sugar over MoQ primitives.
///
/// What it does internally:
/// 1. Connects to remote peer (or accepts incoming session)
/// 2. Creates + publishes a [`LocalBroadcast`] named "call"
/// 3. Subscribes to the remote's broadcast -> [`RemoteBroadcast`]
/// 4. Wraps both in a handle with state tracking
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

impl Call {
    /// Dials a remote peer. Connects, publishes local, subscribes to remote.
    pub async fn dial(live: &Live, remote: impl Into<EndpointAddr>) -> Result<Self, CallError> {
        let mut session = live
            .transport()
            .connect(remote)
            .await
            .map_err(CallError::ConnectionFailed)?;

        let local = LocalBroadcast::new();
        session.publish(CALL_BROADCAST_NAME.to_string(), local.consume());

        let consumer = session
            .subscribe(CALL_BROADCAST_NAME)
            .await
            .map_err(|_| CallError::Rejected)?;
        let remote = RemoteBroadcast::new(CALL_BROADCAST_NAME.to_string(), consumer)
            .await
            .map_err(|_| CallError::Rejected)?;

        Ok(Self {
            session,
            local,
            remote,
        })
    }

    /// Accepts an incoming session as a call.
    pub async fn accept(mut session: MoqSession) -> Result<Self, CallError> {
        let local = LocalBroadcast::new();
        session.publish(CALL_BROADCAST_NAME.to_string(), local.consume());

        let consumer = session
            .subscribe(CALL_BROADCAST_NAME)
            .await
            .map_err(|_| CallError::Rejected)?;
        let remote = RemoteBroadcast::new(CALL_BROADCAST_NAME.to_string(), consumer)
            .await
            .map_err(|_| CallError::Rejected)?;

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
