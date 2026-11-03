//! The player's error types. Everything else is [`moq_ext_media_core::Error`].

use std::sync::Arc;

use moq_ext_media_core::Error;
use n0_error::stack_error;

/// Why a rendition switch did not land.
///
/// Returned by [`Player::wait_for_rendition`](crate::Player::wait_for_rendition).
#[stack_error(derive, add_meta)]
pub enum SwitchError {
    /// A newer request replaced it before it landed.
    #[error("the switch to {rendition} was superseded")]
    Superseded {
        /// The rendition the switch was for.
        rendition: String,
    },
    /// The request was withdrawn before it landed.
    #[error("the switch to {rendition} was withdrawn")]
    Withdrawn {
        /// The rendition the switch was for.
        rendition: String,
    },
    /// The decoder did not open, the track ended, the switch timed out, or the
    /// catalog has no such rendition ([`Error::UnknownRendition`]).
    #[error("the switch to {rendition} failed")]
    Failed {
        /// The rendition the switch was for.
        rendition: String,
        /// Why it failed.
        #[error(source, std_err)]
        source: Arc<Error>,
    },
    /// The player's video ended or was turned off.
    #[error("the video ended")]
    Ended,
}
