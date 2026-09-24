//! The facade's error type.

use iroh_live_media::{publish::PublishError, subscribe::SubscribeError};
use n0_error::stack_error;

/// What can go wrong in the facade: the transport, or the media on top of it.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum Error {
    /// The transport failed.
    ///
    /// A peer could not be reached, or a path could not be resolved or
    /// published.
    #[error(transparent)]
    Transport(iroh_moq::Error),
    /// A broadcast could not be set up for publishing.
    #[error(transparent)]
    Publish(PublishError),
    /// A broadcast's catalog could not be read.
    #[error(transparent)]
    Subscribe(SubscribeError),
}
