//! The crate's error type.

use std::sync::Arc;

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
