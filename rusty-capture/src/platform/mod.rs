//! Platform-specific and cross-platform capture backends.

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod apple;

#[cfg(target_os = "windows")]
pub(crate) mod windows;

#[cfg(target_os = "android")]
pub(crate) mod android;

#[cfg(feature = "xcap")]
pub(crate) mod xcap_impl;

#[cfg(feature = "nokhwa")]
pub(crate) mod nokhwa_impl;
