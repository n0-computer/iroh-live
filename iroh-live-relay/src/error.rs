//! The library's error type.

use std::{path::PathBuf, sync::Arc, time::Duration};

use iroh::EndpointId;
use moq_net::Path;
use n0_error::{AnyError, stack_error};

/// An error from running the relay, pulling a broadcast, or loading a key.
///
/// It is cheap to clone, so every session that waits on one pull gets the
/// same failure. The node's errors sit in a [`Box`], since they are large.
#[stack_error(derive, add_meta)]
#[derive(Clone)]
pub enum Error {
    /// The ticket's peer may not publish the ticket's path.
    ///
    /// The peer does not own the path, already holds too many claims, or the
    /// relay's node has shut down.
    #[error("cannot claim {path} for its publisher")]
    Claim {
        path: Path,
        #[error(source, std_err)]
        source: Box<iroh_live_node::Error>,
    },
    /// The relay could not reach the ticket's publisher.
    #[error("failed to reach the publisher {}", peer.fmt_short())]
    Connect {
        peer: EndpointId,
        #[error(source, std_err)]
        source: Box<iroh_live_node::Error>,
    },
    /// The publisher's session did not resolve the ticket's path.
    #[error("failed to subscribe to {path}")]
    Subscribe {
        path: Path,
        #[error(source, std_err)]
        source: Box<iroh_live_node::Error>,
    },
    /// The publisher did not announce the path in time.
    #[error("the publisher did not announce {path} within {timeout:?}")]
    Timeout { path: Path, timeout: Duration },
    /// The pull ended before its dial reported an outcome.
    #[error("the pull was dropped before it connected")]
    Dropped,
    /// The platform has no data directory to keep the relay's identity in.
    #[error("no platform data directory, set IROH_LIVE_RELAY_DATA")]
    NoDataDir,
    /// The relay's identity could not be loaded or stored.
    #[error(
        "cannot load the relay's identity. Deleting {} starts over, with a new \
         endpoint id that invalidates every ticket naming the old one",
        path.display()
    )]
    Identity {
        path: PathBuf,
        #[error(source, std_err)]
        source: Arc<std::io::Error>,
    },
    /// The relay's iroh endpoint could not bind.
    #[error("failed to bind the relay's iroh endpoint")]
    Bind {
        #[error(source, std_err)]
        source: Arc<iroh_live::Error>,
    },
    /// The JWT keys could not be read.
    #[error("cannot read the JWT keys from {}", path.display())]
    Keys {
        path: PathBuf,
        #[error(source, std_err)]
        source: Arc<moq_auth::Error>,
    },
    /// The moq relay failed to start or stopped with an error.
    #[error("the moq relay failed")]
    Relay {
        #[error(source, std_err)]
        source: Arc<AnyError>,
    },
}
