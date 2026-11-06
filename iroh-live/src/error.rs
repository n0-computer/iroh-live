//! The facade's error type.

use n0_error::stack_error;

/// An error from the transport, the media, or binding the endpoint.
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// A peer was unreachable, or a path could not be resolved or published.
    #[error(transparent)]
    Transport(iroh_live_node::Error),
    /// A source, an encoder, a decoder or a recording failed.
    #[cfg(feature = "media")]
    #[error(transparent)]
    Media(moq_ext_player::Error),
    /// `IROH_SECRET` does not hold a secret key.
    #[error("IROH_SECRET does not hold a secret key")]
    SecretKey { source: iroh::KeyParsingError },
    /// The endpoint could not be bound.
    #[error("failed to bind the endpoint")]
    Bind { source: iroh::endpoint::BindError },
}
