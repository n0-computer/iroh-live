//! The one error type of this crate.

use std::sync::Arc;

use moq_net::PathOwned;
use n0_error::{AnyError, stack_error};

/// Everything that can go wrong in the transport.
///
/// Sources from the crates underneath (iroh's connection errors,
/// web-transport-iroh, moq-tokio) are boxed into an [`AnyError`], so a bump of
/// one of them is not a breaking change here. moq-net's own error is kept typed,
/// because it is the protocol this crate speaks and callers match on it.
#[stack_error(derive, add_meta)]
#[non_exhaustive]
pub enum Error {
    /// The peer could not be dialed.
    ///
    /// No address was found, the QUIC or WebTransport handshake failed, or the
    /// peer refused the connection. Shared, because several callers can wait on
    /// one dial and each of them wants the reason.
    #[error("failed to connect to the peer")]
    Connect {
        /// Why the dial failed.
        #[error(source, std_err)]
        source: Arc<AnyError>,
    },
    /// The peer negotiated an ALPN this build does not speak.
    #[error("the peer negotiated an ALPN this build does not speak: {alpn}")]
    UnsupportedAlpn {
        /// What the peer chose.
        alpn: String,
    },
    /// The MoQ handshake or the session failed.
    #[error("the MoQ session failed")]
    Moq {
        /// What moq-net reported.
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// The peer refused the session this node opened.
    #[error("the peer refused the session")]
    Refused {
        /// The reason the peer gave.
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// The session ended.
    #[error("the session closed")]
    SessionClosed {
        /// Why it ended.
        ///
        /// [`moq_net::Error::Cancel`] for a close either side asked for.
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// A publication already exists at this path.
    #[error("a broadcast is already published at {path}")]
    Duplicate {
        /// The path that is taken.
        path: PathOwned,
    },
    /// The path cannot be published or subscribed.
    ///
    /// It is empty, or holds a segment only a pattern can spell.
    #[error("invalid path {path:?}")]
    InvalidPath {
        /// The path as given.
        path: String,
    },
    /// The session's grant does not cover the path.
    #[error("the session's grant does not cover {path}")]
    NotGranted {
        /// The path the grant does not cover.
        path: PathOwned,
    },
    /// The path was not announced before its only link went away.
    #[error("{path} was not announced before the session ended")]
    NotAnnounced {
        /// The path that was asked for.
        path: PathOwned,
    },
    /// No link can serve the path.
    ///
    /// The reach asks for relays and none is attached, or names this node as
    /// the publisher of a path it does not publish.
    #[error("no link can reach {path}")]
    NoRoute {
        /// The path that was asked for.
        path: PathOwned,
    },
    /// The path could not be resolved.
    ///
    /// For a reason other than the link ending first: the peer refused it, or
    /// it lies outside what this node may see.
    #[error("{path} could not be resolved")]
    Unresolved {
        /// The path that was asked for.
        path: PathOwned,
        /// What moq-net reported.
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// A ticket did not parse.
    #[error("invalid ticket: {reason}")]
    InvalidTicket {
        /// What was wrong with it.
        reason: String,
    },
    /// The endpoint could not be bound.
    #[error("failed to bind the endpoint")]
    Bind {
        /// What iroh reported.
        #[error(source, std_err)]
        source: AnyError,
    },
    /// A relay link could not be set up.
    #[error("the relay link failed")]
    Relay {
        /// What went wrong.
        #[error(source, std_err)]
        source: AnyError,
    },
    /// The transport has shut down.
    #[error("the transport has shut down")]
    ShutDown,
}
