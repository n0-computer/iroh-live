//! Platform-specific capture backends.

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod apple;

#[cfg(target_os = "windows")]
pub(crate) mod windows;

#[cfg(target_os = "android")]
pub(crate) mod android;
