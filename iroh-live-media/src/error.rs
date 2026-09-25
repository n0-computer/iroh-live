//! The crate's error types.
//!
//! One [`Error`] covers everything the crate does. Its variants group failures
//! by what a caller can act on.

use std::sync::Arc;

use n0_error::{AnyError, stack_error};

/// Errors raised by `iroh-live-media`.
///
/// A watched status holds it as `Arc<Error>`, as in
/// [`SlotState::Failed`](crate::SlotState::Failed).
#[stack_error(derive, add_meta)]
pub enum Error {
    /// A capture or playback device would not open, or failed while running.
    #[error("device failed")]
    Device {
        /// What the device reported.
        #[error(source)]
        source: AnyError,
    },
    /// A configuration cannot be used as given.
    #[error("invalid configuration: {reason}")]
    InvalidConfig {
        /// What is wrong with it.
        reason: String,
    },
    /// No encoder compiled into this build can produce the codec asked for.
    #[error("no encoder for {codec} is available in this build")]
    NoEncoder {
        /// The codec that was asked for.
        codec: String,
    },
    /// No decoder compiled into this build can read the codec.
    #[error("no decoder for {codec} is available in this build")]
    NoDecoder {
        /// The codec the broadcast carries.
        codec: String,
    },
    /// An encoder failed to open or to encode.
    #[error("encoder failed")]
    Encoder {
        /// What the encoder reported.
        #[error(source)]
        source: AnyError,
    },
    /// A decoder failed to open or to decode.
    #[error("decoder failed")]
    Decoder {
        /// What the decoder reported.
        #[error(source)]
        source: AnyError,
    },
    /// The broadcast's catalog has no video rendition of that name.
    #[error("no video rendition named {name}, the broadcast has [{}]", offered.join(", "))]
    UnknownRendition {
        /// The name that was asked for.
        name: String,
        /// The names the catalog has, largest first.
        offered: Vec<String>,
    },
    /// The catalog could not be read or written.
    #[error("catalog failed")]
    Catalog {
        /// What went wrong with it.
        #[error(source)]
        source: AnyError,
    },
    /// The broadcast layer refused an operation or reset a track being read.
    ///
    /// The error comes from moq-net and looks the same whatever transport
    /// carries the broadcast. Failures of the transport itself, such as an
    /// unreachable peer, belong to the transport's own error, `iroh_moq::Error`
    /// for iroh.
    #[error("broadcast failed")]
    Broadcast {
        /// What moq-net reported.
        #[error(source)]
        source: AnyError,
    },
    /// The broadcast, source or player was closed.
    #[error("closed")]
    Closed,
    /// Reading or writing a file or pipe failed.
    #[error("I/O failed")]
    Io {
        /// What the operating system reported.
        #[error(source, std_err)]
        source: std::io::Error,
    },
}

impl Error {
    /// Creates an [`Error::InvalidConfig`] with `reason`.
    pub(crate) fn invalid(reason: impl Into<String>) -> Self {
        n0_error::e!(Self::InvalidConfig {
            reason: reason.into()
        })
    }

    /// Creates an [`Error::Device`] from an upstream error.
    #[cfg(any(feature = "capture", feature = "playback"))]
    pub(crate) fn device(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Device {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Device`] from a message.
    pub(crate) fn device_msg(message: impl std::fmt::Display) -> Self {
        n0_error::e!(Self::Device {
            source: AnyError::from_display(message)
        })
    }

    /// Creates an [`Error::Encoder`] from an upstream error.
    pub(crate) fn encoder(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Encoder {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Encoder`] from a message.
    pub(crate) fn encoder_msg(message: impl std::fmt::Display) -> Self {
        n0_error::e!(Self::Encoder {
            source: AnyError::from_display(message)
        })
    }

    /// Creates an [`Error::Decoder`] from an upstream error.
    pub(crate) fn decoder(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Decoder {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Decoder`] from a message.
    pub(crate) fn decoder_msg(message: impl std::fmt::Display) -> Self {
        n0_error::e!(Self::Decoder {
            source: AnyError::from_display(message)
        })
    }

    /// Creates an [`Error::Catalog`] from an upstream error.
    pub(crate) fn catalog(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Catalog {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Broadcast`] from an upstream error.
    pub(crate) fn broadcast(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Broadcast {
            source: AnyError::from_std(source)
        })
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        n0_error::e!(Self::Io { source })
    }
}

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
    /// The decoder did not open, the track ended, or the switch timed out.
    #[error("the switch to {rendition} failed")]
    Failed {
        /// The rendition the switch was for.
        rendition: String,
        /// Why it failed.
        #[error(source, std_err)]
        source: Arc<Error>,
    },
    /// The broadcast's catalog has no video rendition of that name.
    #[error("no video rendition named {rendition}, the broadcast has [{}]", offered.join(", "))]
    UnknownRendition {
        /// The name that was asked for.
        rendition: String,
        /// The names the catalog has, largest first.
        offered: Vec<String>,
    },
    /// The player's video ended or was turned off.
    #[error("the video ended")]
    Ended,
}

/// The source a [`FrameSender`](crate::FrameSender) feeds has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("the source has closed")]
pub struct Closed;

impl std::error::Error for Closed {}
