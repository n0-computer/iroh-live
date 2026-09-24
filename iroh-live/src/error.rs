//! The facade's error type.

use n0_error::stack_error;

/// What can go wrong in the facade: the transport, or the media on top of it.
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// The transport failed.
    ///
    /// A peer could not be reached, or a path could not be resolved or
    /// published.
    #[error(transparent)]
    Transport(iroh_moq::Error),
    /// The media on top of it failed.
    ///
    /// A source could not be opened, a broadcast set up, or a broadcast
    /// played or recorded.
    #[error(transparent)]
    Media(iroh_live_media::Error),
}
