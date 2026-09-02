use iroh::{EndpointAddr, EndpointId, endpoint::ConnectionError};
use iroh_moq::MoqSession;
use moq_media::{net::NetworkSignals, subscribe::RemoteBroadcast};
use n0_error::{AnyError, Result, stack_error};
use tokio::sync::watch;

use crate::{Live, types::DisconnectReason};

/// Errors from call operations.
#[stack_error(derive)]
pub enum CallError {
    #[error("failed to connect")]
    /// Failed to connect to the remote peer.
    ConnectionFailed(#[error(source, std_err)] iroh_moq::Error),
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
/// 1. Connects to the remote peer, or accepts an incoming session
/// 2. Subscribes to the peer's broadcast -> [`RemoteBroadcast`]
/// 3. Wires the stats recorder and the signal producer onto the connection
///
/// The local side is not part of this. Publishing is node-wide, so a broadcast
/// created with `live.publish(Call::path(own_id))` is announced on every
/// session this node has, including the call's own, and it outlives any number
/// of calls. Keep it wherever the application keeps its capture; the call only
/// reads the peer's side, at `Call::path(remote_id)`.
///
/// Everything this does can be done directly with [`Live::publish`],
/// [`Live::transport`], and [`RemoteBroadcast`]; it is 1:1 sugar, not a layer.
#[derive(Debug)]
pub struct Call {
    session: MoqSession,
    remote: RemoteBroadcast,
    signals: watch::Receiver<NetworkSignals>,
}

/// The path prefix a call publishes under.
///
/// The full path is `calls/<publisher endpoint id>`, so two peers in a call
/// each publish under their own id and subscribe to the other's. The old scheme
/// used the bare name "call" on a per-session origin; publishing is node-wide
/// now, and a per-peer path keeps two concurrent calls from colliding on one
/// name.
const CALL_PREFIX: &str = "calls";

/// The path a peer publishes its side of a call on.
fn call_path(publisher: EndpointId) -> String {
    format!("{CALL_PREFIX}/{publisher}")
}

impl Call {
    /// Dials a remote peer and subscribes to its side of the call.
    ///
    /// Publish this node's own side with
    /// `live.publish(Call::path(live.endpoint().id()))` first, so the peer has
    /// something to subscribe to when it answers.
    pub async fn dial(live: &Live, remote: impl Into<EndpointAddr>) -> Result<Self, CallError> {
        let session = live
            .transport()
            .connect(remote)
            .await
            .map_err(CallError::ConnectionFailed)?;
        Self::setup(session).await
    }

    /// The path this node publishes its side of a call on.
    ///
    /// Build the broadcast with `live.publish(Call::path(live.endpoint().id()))`
    /// before dialing or accepting.
    pub fn path(publisher: EndpointId) -> String {
        call_path(publisher)
    }

    /// Accepts an incoming session as a call.
    ///
    /// Wants the local side published the same way [`dial`](Self::dial)
    /// describes.
    pub async fn accept(session: MoqSession) -> Result<Self, CallError> {
        Self::setup(session).await
    }

    /// Subscribes to the peer's side of the call. Shared by
    /// [`dial`](Self::dial) and [`accept`](Self::accept).
    ///
    /// Nothing is published here: the local broadcast lives on the node origin
    /// and is already announced on this session.
    ///
    /// Auto-wires stats recording and network signal production on the
    /// connection, so callers do not need to do this manually.
    async fn setup(session: MoqSession) -> Result<Self, CallError> {
        let path = call_path(session.remote_id());
        let consumer = session
            .subscribe(&path)
            .await
            .map_err(|err| CallError::Rejected(err.into()))?;
        let remote = RemoteBroadcast::new(&path, consumer)
            .await
            .map_err(|err| CallError::Rejected(err.into()))?;

        crate::util::spawn_stats_recorder(
            session.conn(),
            remote.stats().net.clone(),
            remote.shutdown_token(),
        );
        let signals = crate::util::spawn_signal_producer(session.conn(), remote.shutdown_token());

        Ok(Self {
            session,
            remote,
            signals,
        })
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
    /// Signals are produced automatically when the call is established, so
    /// callers do not need to call `spawn_signal_producer` themselves.
    pub fn signals(&self) -> &watch::Receiver<NetworkSignals> {
        &self.signals
    }

    /// Closes the call, ending the session.
    pub fn close(&self) {
        self.session.close(moq_net::Error::Cancel);
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
