//! The facade's error type.

use n0_error::stack_error;

/// An error from the node, rooms, calls, or the media.
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// A peer was unreachable, a path could not be resolved or published, or
    /// the endpoint could not be bound.
    #[error(transparent)]
    Node(iroh_live_node::Error),
    /// A source, an encoder, a decoder or a recording failed.
    #[cfg(feature = "media")]
    #[error(transparent)]
    Media(moq_ext_player::Error),
    /// A room failed.
    #[cfg(feature = "rooms")]
    #[error(transparent)]
    Rooms(iroh_live_rooms::Error),
    /// A call could not be placed or answered.
    #[error(transparent)]
    Calls(crate::calls::Error),
}
