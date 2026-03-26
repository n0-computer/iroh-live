use std::fmt;

/// Disconnect reason for calls and room participants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DisconnectReason {
    /// Closed by the local end.
    LocalClose,
    /// Closed by the remote end.
    RemoteClose,
    /// Transport-level error.
    TransportError,
}

impl fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalClose => write!(f, "local close"),
            Self::RemoteClose => write!(f, "remote close"),
            Self::TransportError => write!(f, "transport error"),
        }
    }
}
