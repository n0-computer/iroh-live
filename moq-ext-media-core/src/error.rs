//! The error the player and the publisher share.
//!
//! The variants of [`Error`] group failures by what a caller can act on.

use moq_ext_sources::Error as SourceError;
use n0_error::{AnyError, stack_error};

/// Errors raised by the media crates.
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
    pub fn invalid(reason: impl Into<String>) -> Self {
        n0_error::e!(Self::InvalidConfig {
            reason: reason.into()
        })
    }

    /// Creates an [`Error::Device`] from an upstream error.
    pub fn device(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Device {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Device`] from a message.
    pub fn device_msg(message: impl std::fmt::Display) -> Self {
        n0_error::e!(Self::Device {
            source: AnyError::from_display(message)
        })
    }

    /// Creates an [`Error::Encoder`] from an upstream error.
    pub fn encoder(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Encoder {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Encoder`] from a message.
    pub fn encoder_msg(message: impl std::fmt::Display) -> Self {
        n0_error::e!(Self::Encoder {
            source: AnyError::from_display(message)
        })
    }

    /// Creates an [`Error::Decoder`] from an upstream error.
    pub fn decoder(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Decoder {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Decoder`] from a message.
    pub fn decoder_msg(message: impl std::fmt::Display) -> Self {
        n0_error::e!(Self::Decoder {
            source: AnyError::from_display(message)
        })
    }

    /// Creates an [`Error::Catalog`] from an upstream error.
    pub fn catalog(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Catalog {
            source: AnyError::from_std(source)
        })
    }

    /// Creates an [`Error::Broadcast`] from an upstream error.
    pub fn broadcast(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        n0_error::e!(Self::Broadcast {
            source: AnyError::from_std(source)
        })
    }
}

impl From<moq_ext_sources::Error> for Error {
    fn from(err: moq_ext_sources::Error) -> Self {
        match err {
            SourceError::Device { source, .. } => n0_error::e!(Self::Device { source }),
            SourceError::InvalidConfig { reason, .. } => {
                n0_error::e!(Self::InvalidConfig { reason })
            }
            SourceError::Closed { .. } => n0_error::e!(Self::Closed),
            SourceError::Io { source, .. } => n0_error::e!(Self::Io { source }),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        n0_error::e!(Self::Io { source })
    }
}
