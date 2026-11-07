//! The player's own error type, beside the shared [`moq_ext_media_core::Error`].
//!
//! Also tells a session that closed from a track that failed. A peer hanging
//! up ends every track read over the session with a cancel code, and a
//! publisher in the same process that goes away ends its tracks as dropped.
//! That is how playback normally ends, so it is logged at debug and never
//! reported as a failed decoder.

use std::sync::Arc;

use moq_ext_media_core::Error;
use moq_net::{SessionError, StreamError};
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
    /// The switch could not finish.
    ///
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

/// An upstream error that may only say the session or the track was closed.
pub(crate) trait CleanClose {
    /// Reports whether this error is a clean close rather than a failure.
    ///
    /// A clean close is a cancel from either side: the session closed with no
    /// error, or the publisher or this side stopped the track. A producer
    /// dropped without a finish or an abort also counts: its publisher went
    /// away, and the rendition is not to blame.
    fn is_clean_close(&self) -> bool;
}

impl CleanClose for moq_net::Error {
    fn is_clean_close(&self) -> bool {
        matches!(
            self,
            Self::Cancel
                | Self::Dropped
                | Self::Session(SessionError::Cancel)
                | Self::Stream(StreamError::Cancel | StreamError::Session(SessionError::Cancel))
        )
    }
}

impl CleanClose for moq_mux::Error {
    fn is_clean_close(&self) -> bool {
        matches!(self, Self::Moq(err) if err.is_clean_close())
    }
}

impl CleanClose for moq_video::Error {
    fn is_clean_close(&self) -> bool {
        match self {
            Self::Net(err) => err.is_clean_close(),
            Self::Mux(err) => err.is_clean_close(),
            _ => false,
        }
    }
}

impl CleanClose for moq_audio::Error {
    fn is_clean_close(&self) -> bool {
        match self {
            Self::Net(err) => err.is_clean_close(),
            Self::Mux(err) => err.is_clean_close(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_closed_without_error_is_a_clean_close() {
        let closed = moq_net::Error::Session(SessionError::Cancel);
        assert!(closed.is_clean_close());
        assert!(moq_video::Error::Mux(moq_mux::Error::Moq(closed.clone())).is_clean_close());
        assert!(moq_audio::Error::Net(closed).is_clean_close());
    }

    #[test]
    fn a_cancelled_stream_is_a_clean_close() {
        assert!(moq_net::Error::Stream(StreamError::Cancel).is_clean_close());
        assert!(moq_net::Error::Cancel.is_clean_close());
    }

    /// A publisher that drops its track without finishing it has gone away.
    #[test]
    fn a_dropped_producer_is_a_clean_close() {
        let dropped = moq_net::Error::Dropped;
        assert!(dropped.is_clean_close());
        assert!(moq_video::Error::Net(dropped.clone()).is_clean_close());
        assert!(moq_audio::Error::Mux(moq_mux::Error::Moq(dropped)).is_clean_close());
    }

    #[test]
    fn a_failed_session_is_not_a_clean_close() {
        let failed = moq_net::Error::Session(SessionError::Internal);
        assert!(!failed.is_clean_close());
        assert!(!moq_video::Error::Net(failed).is_clean_close());
        assert!(!moq_net::Error::Timeout.is_clean_close());
    }
}
