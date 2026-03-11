mod decoder;
mod encoder;

pub use decoder::V4l2Decoder;
pub use encoder::V4l2Encoder;

use std::path::{Path, PathBuf};

/// Returns the V4L2 encoder device path.
///
/// Checks `V4L2_ENC_DEVICE` env var, then tries common paths.
pub(crate) fn encoder_device_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("V4L2_ENC_DEVICE") {
        return Some(PathBuf::from(path));
    }
    // bcm2835-codec encoder on Pi Zero 2 W / Pi 3 / Pi 4
    find_existing(&["/dev/video11"])
}

/// Returns the V4L2 decoder device path.
///
/// Checks `V4L2_DEC_DEVICE` env var, then tries common paths.
pub(crate) fn decoder_device_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("V4L2_DEC_DEVICE") {
        return Some(PathBuf::from(path));
    }
    // bcm2835-codec decoder on Pi Zero 2 W / Pi 3 / Pi 4
    find_existing(&["/dev/video10"])
}

fn find_existing(paths: &[&str]) -> Option<PathBuf> {
    paths
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .map(Path::to_path_buf)
}
