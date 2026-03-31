//! Linux capture backends.

#[cfg(feature = "libcamera")]
pub(crate) mod libcamera;

#[cfg(feature = "libcamera-h264")]
pub(crate) mod libcamera_h264;

#[cfg(feature = "pipewire")]
pub(crate) mod pipewire;

#[cfg(feature = "v4l2")]
pub(crate) mod v4l2;

#[cfg(feature = "x11")]
pub(crate) mod x11;
