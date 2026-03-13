use std::fmt;

/// Well-known track names within a room.
///
/// The room prepends its own namespace, so `TrackName::Camera` becomes
/// e.g. `"room-abc/camera"` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrackName {
    /// Primary camera + mic broadcast.
    Camera,
    /// Screen share broadcast.
    Screen,
    /// Custom named broadcast.
    Other(String),
}

impl fmt::Display for TrackName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Camera => write!(f, "camera"),
            Self::Screen => write!(f, "screen"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Disconnect reason for calls and room participants.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Unique identifier for a participant, derived from their iroh endpoint ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParticipantId(pub iroh::EndpointId);

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.fmt_short())
    }
}

/// Media track kind (used in room events).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// Video track.
    Video,
    /// Audio track.
    Audio,
}
