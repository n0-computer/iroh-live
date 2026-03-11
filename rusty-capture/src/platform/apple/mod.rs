//! macOS and iOS capture backends.

#[cfg(feature = "camera-apple")]
pub(crate) mod camera;

#[cfg(all(target_os = "macos", feature = "screen-apple"))]
pub(crate) mod screen;
