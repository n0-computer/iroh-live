#[cfg(feature = "capture-camera")]
pub mod camera;
#[cfg(feature = "capture-screen")]
pub mod screen;

#[cfg(feature = "capture-camera")]
pub use camera::*;
#[cfg(feature = "capture-screen")]
pub use screen::*;
