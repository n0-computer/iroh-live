//! The facade's error type.

use n0_error::stack_error;

/// An error from the node, the endpoint, rooms, calls, or the media.
///
/// Every variant carries the error's metadata after its source, so a match
/// names it:
///
/// ```
/// use iroh_live::{EndpointConfig, EndpointError, Error};
///
/// # async fn run() -> Result<(), Error> {
/// let bound = EndpointConfig::from_env()?.bind().await;
/// match bound.map_err(Error::from) {
///     Ok(endpoint) => endpoint.close().await,
///     Err(Error::Endpoint(EndpointError::Bind { .. }, _)) => eprintln!("cannot bind"),
///     Err(other) => return Err(other),
/// }
/// # Ok(())
/// # }
/// ```
#[stack_error(derive, add_meta, from_sources)]
pub enum Error {
    /// A peer was unreachable, or a path could not be resolved or published.
    #[error(transparent)]
    Node(iroh_live_node::Error),
    /// `IROH_SECRET` held no secret key, or the endpoint could not be bound.
    #[error(transparent)]
    Endpoint(iroh_live_node::EndpointError),
    /// A source, an encoder, a decoder or a recording failed.
    #[cfg(any(feature = "player", feature = "publisher"))]
    #[error(transparent)]
    Media(moq_ext_sources::Error),
    /// A room failed.
    #[cfg(feature = "rooms")]
    #[error(transparent)]
    Rooms(iroh_live_rooms::Error),
    /// A call could not be placed or answered.
    #[error(transparent)]
    Calls(crate::calls::Error),
}
