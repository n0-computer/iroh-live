//! The crate's error types.

use std::sync::Arc;

use iroh::endpoint::{
    ClosedStream, ConnectionError, ReadError, ResetError, SendDatagramError, StoppedError, VarInt,
    WriteError,
};
use moq_net::transport::CloseKind;
use n0_error::{AnyError, stack_error};

/// Errors of dialing and accepting.
#[stack_error(derive, add_meta)]
pub enum Error {
    /// The peer could not be dialed, or the handshake failed.
    ///
    /// The source is shared between the callers waiting on one dial.
    #[error("failed to connect to the peer")]
    Connect {
        #[error(source, std_err)]
        source: Arc<AnyError>,
    },
    /// The peer negotiated an ALPN this build does not speak.
    #[error("the peer negotiated an ALPN this build does not speak: {alpn}")]
    UnsupportedAlpn { alpn: String },
}

/// A failure of a [`Transport`](crate::Transport)'s connection or of one of its streams.
///
/// moq-net reads the application codes from it: a session close code from
/// [`Connection`](Self::Connection), a stream code from [`Stopped`](Self::Stopped)
/// and [`Reset`](Self::Reset).
#[stack_error(derive)]
#[derive(Clone)]
pub enum TransportError {
    /// The connection closed or failed.
    #[error("connection lost: {_0}")]
    Connection(#[error(source, std_err)] ConnectionError),
    /// The peer stopped a stream this side sends on.
    #[error("stream stopped by the peer with code {_0}")]
    Stopped(VarInt),
    /// The peer reset a stream this side reads.
    #[error("stream reset by the peer with code {_0}")]
    Reset(VarInt),
    /// The stream was already finished, reset or stopped.
    #[error("stream already closed")]
    ClosedStream,
    /// The connection refused a datagram.
    #[error("datagram refused: {_0}")]
    Datagram(#[error(source, std_err)] SendDatagramError),
}

impl TransportError {
    /// Returns the connection failure behind this error, if there is one.
    fn connection(&self) -> Option<&ConnectionError> {
        match self {
            Self::Connection(err) | Self::Datagram(SendDatagramError::ConnectionLost(err)) => {
                Some(err)
            }
            _ => None,
        }
    }
}

impl moq_net::transport::Error for TransportError {
    fn close_kind(&self) -> CloseKind {
        match self.connection() {
            Some(ConnectionError::TimedOut) => CloseKind::IdleTimeout,
            Some(ConnectionError::LocallyClosed) => CloseKind::Closed,
            _ => CloseKind::Transport,
        }
    }

    fn session_error(&self) -> Option<(u32, String)> {
        let Some(ConnectionError::ApplicationClosed(close)) = self.connection() else {
            return None;
        };
        let code = u32::try_from(close.error_code.into_inner()).ok()?;
        Some((code, String::from_utf8_lossy(&close.reason).into_owned()))
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            Self::Stopped(code) | Self::Reset(code) => u32::try_from(code.into_inner()).ok(),
            _ => None,
        }
    }
}

impl From<ConnectionError> for TransportError {
    fn from(err: ConnectionError) -> Self {
        Self::Connection(err)
    }
}

impl From<SendDatagramError> for TransportError {
    fn from(err: SendDatagramError) -> Self {
        Self::Datagram(err)
    }
}

impl From<ClosedStream> for TransportError {
    fn from(_: ClosedStream) -> Self {
        Self::ClosedStream
    }
}

// The transport never sends 0-RTT data, so the `ZeroRttRejected` variants below
// cannot occur. They map to a closed stream rather than a panic.

impl From<WriteError> for TransportError {
    fn from(err: WriteError) -> Self {
        match err {
            WriteError::Stopped(code) => Self::Stopped(code),
            WriteError::ConnectionLost(err) => Self::Connection(err),
            WriteError::ClosedStream | WriteError::ZeroRttRejected => Self::ClosedStream,
        }
    }
}

impl From<ReadError> for TransportError {
    fn from(err: ReadError) -> Self {
        match err {
            ReadError::Reset(code) => Self::Reset(code),
            ReadError::ConnectionLost(err) => Self::Connection(err),
            ReadError::ClosedStream | ReadError::ZeroRttRejected => Self::ClosedStream,
        }
    }
}

impl From<StoppedError> for TransportError {
    fn from(err: StoppedError) -> Self {
        match err {
            StoppedError::ConnectionLost(err) => Self::Connection(err),
            StoppedError::ZeroRttRejected => Self::ClosedStream,
        }
    }
}

impl From<ResetError> for TransportError {
    fn from(err: ResetError) -> Self {
        match err {
            ResetError::ConnectionLost(err) => Self::Connection(err),
            ResetError::ZeroRttRejected => Self::ClosedStream,
        }
    }
}
