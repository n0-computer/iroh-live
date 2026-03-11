//! macOS and iOS capture backends.

#[cfg(feature = "apple")]
pub(crate) mod camera;

#[cfg(all(target_os = "macos", feature = "apple-screen"))]
pub(crate) mod screen;
