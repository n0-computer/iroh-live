//! The facade's error type.

use n0_error::stack_error;

/// An error from the transport or from the media on top of it.
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// A peer was unreachable, or a path could not be resolved or published.
    #[error(transparent)]
    Transport(iroh_moq::Error),
    /// A source, an encoder, a decoder or a recording failed.
    #[error(transparent)]
    Media(iroh_live_media::Error),
}
