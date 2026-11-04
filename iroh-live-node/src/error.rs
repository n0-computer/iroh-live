//! The crate's error type.

use std::sync::Arc;

use moq_net::Path;
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
    Node {
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
    Duplicate { path: Path },
    /// The path cannot be published or subscribed.
    ///
    /// It is empty, or holds a segment only a pattern can spell.
    #[error("invalid path {path:?}")]
    InvalidPath { path: String },
    /// The session's grant does not cover the path.
    ///
    /// For a subscribe, the peer may not publish the path.
    #[error("the session's grant does not cover {path}")]
    NotGranted { path: Path },
    /// The path was not announced before its only link went away.
    #[error("{path} was not announced before the session ended")]
    NotAnnounced { path: Path },
    /// No link can serve the path.
    ///
    /// The reach asks for relays and none is attached, or names this node as
    /// the publisher of a path it does not publish.
    #[error("no link can reach {path}")]
    NoRoute { path: Path },
    /// The path could not be resolved.
    ///
    /// The peer refused it, or it lies outside what this node may see.
    #[error("{path} could not be resolved")]
    Unresolved {
        path: Path,
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

impl From<moq_transport_iroh::Error> for Error {
    fn from(err: moq_transport_iroh::Error) -> Self {
        match err {
            moq_transport_iroh::Error::Connect { source, .. } => {
                n0_error::e!(Self::Connect { source })
            }
            moq_transport_iroh::Error::UnsupportedAlpn { alpn, .. } => {
                n0_error::e!(Self::UnsupportedAlpn { alpn })
            }
        }
    }
}

/// Converts a caller's path, failing like moq-net's own entry points do.
pub(crate) fn to_path(
    path: impl TryInto<moq_net::Path, Error: Into<moq_net::Error>>,
) -> Result<moq_net::Path, Error> {
    path.try_into().map_err(|source| {
        n0_error::e!(Error::Node {
            source: source.into()
        })
    })
}
