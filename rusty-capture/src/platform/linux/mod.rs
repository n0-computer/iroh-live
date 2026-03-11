//! Linux capture backends.

#[cfg(feature = "pipewire")]
pub(crate) mod pipewire;

#[cfg(feature = "v4l2")]
pub(crate) mod v4l2;

#[cfg(feature = "x11")]
pub(crate) mod x11;
