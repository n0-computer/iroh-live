//! The crate's error type.

use std::sync::Arc;

use iroh::EndpointId;
use moq_net::Path;
use n0_error::{AnyError, stack_error};

/// An error from a [`Node`](crate::Node) or one of its handles.
///
/// moq-net's and the transport's errors stay typed, and the rest carry an
/// [`AnyError`]. The error is cheap to clone, so the callers waiting on one
/// dial each get the same failure.
#[stack_error(derive, add_meta)]
#[derive(Clone)]
pub enum Error {
    /// The QUIC connection could not be set up.
    ///
    /// The peer could not be dialed, or it negotiated an ALPN this build does
    /// not speak.
    #[error("the transport failed")]
    Transport {
        #[error(source, std_err)]
        source: moq_transport_iroh::Error,
    },
    /// moq-net refused an operation.
    ///
    /// The MoQ handshake or the session failed, a path did not parse, or the
    /// route table refused to create or announce a broadcast.
    #[error("moq-net failed")]
    Moq {
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// The fill passed to [`Node::publish_with`](crate::Node::publish_with) failed.
    ///
    /// The broadcast was closed and never announced.
    #[error("failed to fill the broadcast")]
    Fill {
        #[error(source, std_err)]
        source: Arc<AnyError>,
    },
    /// The peer refused the session this node opened.
    #[error("the peer refused the session")]
    Refused {
        #[error(source, std_err)]
        source: moq_net::Error,
    },
    /// This node's policy rejected the peer.
    ///
    /// A dial fails with it before it reaches the peer.
    #[error("the policy rejected the session")]
    Rejected {
        #[error(source, std_err)]
        source: crate::policy::Rejection,
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
    /// It is empty, holds a `*`, or has more segments than a pattern takes.
    #[error("invalid path {path:?}")]
    InvalidPath { path: String },
    /// The session's grant does not cover the path.
    ///
    /// For a subscribe, the peer may not publish the path. `publish_patterns`
    /// are the patterns the grant lets the peer publish, as text.
    #[error(
        "the grant of {} does not let it publish {path}, it may publish [{}]",
        peer.fmt_short(),
        list_patterns(publish_patterns)
    )]
    NotGranted {
        path: Path,
        peer: EndpointId,
        publish_patterns: moq_net::Patterns,
    },
    /// The peer does not own the path, so it cannot be claimed for the peer.
    #[error("{} does not own {path}", peer.fmt_short())]
    NotOwner { path: Path, peer: EndpointId },
    /// The peer already holds as many claimed paths as the node allows.
    #[error("too many claimed paths for {}", peer.fmt_short())]
    TooManyClaims { peer: EndpointId },
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
        source: Arc<AnyError>,
    },
    /// A string is not a [`BroadcastTicket`](crate::BroadcastTicket).
    #[error("invalid broadcast ticket: {reason}")]
    InvalidTicket { reason: &'static str },
    /// The node has shut down.
    #[error("the node has shut down")]
    ShutDown,
}

impl From<moq_transport_iroh::Error> for Error {
    fn from(source: moq_transport_iroh::Error) -> Self {
        n0_error::e!(Self::Transport { source })
    }
}

/// How many patterns [`Error::NotGranted`] lists.
const LISTED_PATTERNS: usize = 8;

/// Renders the first few of `patterns` for an error message.
fn list_patterns(patterns: &moq_net::Patterns) -> String {
    let mut text = patterns
        .iter()
        .take(LISTED_PATTERNS)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if patterns.len() > LISTED_PATTERNS {
        text.push_str(&format!(", and {} more", patterns.len() - LISTED_PATTERNS));
    }
    text
}

impl Error {
    /// Returns the error for `peer` publishing `path` outside `grant`.
    pub(crate) fn not_granted(path: Path, peer: EndpointId, grant: &crate::Grant) -> Self {
        n0_error::e!(Self::NotGranted {
            path,
            peer,
            publish_patterns: grant.publish.clone(),
        })
    }
}

/// Converts a caller's path, failing like moq-net's own entry points do.
pub(crate) fn to_path(
    path: impl TryInto<moq_net::Path, Error: Into<moq_net::Error>>,
) -> Result<moq_net::Path, Error> {
    path.try_into().map_err(|source| {
        n0_error::e!(Error::Moq {
            source: source.into()
        })
    })
}

#[cfg(test)]
mod tests {
    use moq_net::{Pattern, Patterns};

    use super::*;

    #[test]
    fn not_granted_lists_at_most_eight_patterns() {
        let patterns: Patterns = (0..12)
            .map(|n| Pattern::literal(&format!("live/p{n}/cam")).expect("pattern"))
            .collect();
        let listed = list_patterns(&patterns);
        assert_eq!(listed.matches("/cam").count(), 8, "{listed}");
        assert!(listed.ends_with("and 4 more"), "{listed}");
        assert_eq!(list_patterns(&Patterns::new()), "");
    }
}
