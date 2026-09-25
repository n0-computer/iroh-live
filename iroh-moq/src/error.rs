//! The crate's error type.

use std::sync::Arc;

use moq_net::PathOwned;
use n0_error::{AnyError, stack_error};

/// Errors of the transport.
///
/// moq-net's errors stay typed, and the rest carry an [`AnyError`].
#[stack_error(derive, add_meta)]
pub enum Error {
    /// The peer could not be dialed.
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
    /// The MoQ handshake or the session failed.
    #[error("the MoQ session failed")]
    Moq {
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// The peer refused the session this node opened.
    #[error("the peer refused the session")]
    Refused {
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// The session ended.
    #[error("the session closed")]
    SessionClosed {
        /// Is [`moq_net::Error::Cancel`] if either side closed it.
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// A publication already exists at this path.
    #[error("a broadcast is already published at {path}")]
    Duplicate { path: PathOwned },
    /// The path cannot be published or subscribed.
    ///
    /// It is empty, or holds a segment only a pattern can spell.
    #[error("invalid path {path:?}")]
    InvalidPath { path: String },
    /// The session's grant does not cover the path.
    #[error("the session's grant does not cover {path}")]
    NotGranted { path: PathOwned },
    /// The path was not announced before its only link went away.
    #[error("{path} was not announced before the session ended")]
    NotAnnounced { path: PathOwned },
    /// No link can serve the path.
    ///
    /// The reach asks for relays and none is attached, or names this node as
    /// the publisher of a path it does not publish.
    #[error("no link can reach {path}")]
    NoRoute { path: PathOwned },
    /// The path could not be resolved.
    ///
    /// The peer refused it, or it lies outside what this node may see.
    #[error("{path} could not be resolved")]
    Unresolved {
        path: PathOwned,
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// A relay link could not be set up.
    #[error("the relay link failed")]
    Relay {
        #[error(source, std_err)]
        source: AnyError,
    },
    /// The transport has shut down.
    #[error("the transport has shut down")]
    ShutDown,
}
