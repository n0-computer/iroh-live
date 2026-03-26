use iroh::{EndpointAddr, EndpointId, endpoint::ConnectionError};
use iroh_moq::MoqSession;
use moq_media::{net::NetworkSignals, publish::LocalBroadcast, subscribe::RemoteBroadcast};
use n0_error::{AnyError, Result, stack_error};
use tokio::sync::watch;

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
    signals: watch::Receiver<NetworkSignals>,
}

const CALL_BROADCAST_NAME: &str = "call";

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
    ///
    /// Auto-wires stats recording and network signal production on the
    /// connection, so callers do not need to do this manually.
    async fn setup(mut session: MoqSession, local: LocalBroadcast) -> Result<Self, CallError> {
        session.publish(CALL_BROADCAST_NAME, local.consume());

        let consumer = session
            .subscribe(CALL_BROADCAST_NAME)
            .await
            .map_err(|err| CallError::Rejected(err.into()))?;
        let remote = RemoteBroadcast::new(CALL_BROADCAST_NAME, consumer)
            .await
            .map_err(CallError::Rejected)?;

        crate::util::spawn_stats_recorder(
            session.conn(),
            remote.stats().net.clone(),
            remote.shutdown_token(),
        );
        let signals = crate::util::spawn_signal_producer(session.conn(), remote.shutdown_token());

        Ok(Self {
            session,
            local,
            remote,
            signals,
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

    /// Returns the network signals receiver for adaptive rendition selection.
    ///
    /// Signals are produced automatically when the call is established —
    /// callers do not need to call `spawn_signal_producer` manually.
    pub fn signals(&self) -> &watch::Receiver<NetworkSignals> {
        &self.signals
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
