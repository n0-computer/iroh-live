//! The crate's error types.

use n0_error::{AnyError, stack_error};

/// Errors raised while opening or running a source.
#[stack_error(derive, add_meta)]
pub enum Error {
    /// A capture device or file would not open, or failed while running.
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
    /// The source was closed.
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
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        n0_error::e!(Self::Io { source })
    }
}

/// The source a [`FrameSender`](crate::FrameSender) feeds has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
#[display("the source has closed")]
pub struct Closed;

impl std::error::Error for Closed {}
