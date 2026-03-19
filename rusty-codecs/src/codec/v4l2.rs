//! V4L2 stateful H.264 encoder and decoder for Linux.
//!
//! Targets the V4L2 M2M (memory-to-memory) codec interface, which is the
//! standard kernel API for hardware video codecs on ARM SoCs. Tested on
//! the Raspberry Pi's `bcm2835-codec` driver (Pi Zero 2 W, Pi 3, Pi 4).
//!
//! ## Portability across SoCs
//!
//! The V4L2 M2M encoder API (`VIDIOC_S_FMT`, `VIDIOC_REQBUFS`,
//! `VIDIOC_QBUF`/`VIDIOC_DQBUF`) is standardized by the kernel, but
//! individual drivers differ in important ways:
//!
//! - **H.264 level default**: The bcm2835-codec driver defaults to Level
//!   1.0 (128x96 max), so the level MUST be set explicitly via
//!   `V4L2_CID_MPEG_VIDEO_H264_LEVEL` before `VIDIOC_STREAMON` or the
//!   encoder fails for any resolution above SQCIF. Other drivers (e.g.
//!   Rockchip, Samsung Exynos) may default to a higher level or
//!   auto-negotiate based on the format dimensions. There is no way to
//!   know the default without testing each driver.
//!
//! - **SPS/PPS repeat**: bcm2835 uses `V4L2_CID_MPEG_VIDEO_REPEAT_SEQ_HEADER`
//!   to repeat SPS/PPS with each IDR frame. The standard control
//!   `V4L2_CID_MPEG_VIDEO_PREPEND_SPSPPS_TO_IDR` is not supported.
//!   Other drivers may support one, both, or neither.
//!
//! - **Output pixel format**: The bcm2835 decoder outputs YU12 (I420,
//!   planar Y + U + V), not NV12 (semi-planar Y + UV). The decoder
//!   checks the negotiated pixel format and handles both.
//!
//! - **Device paths**: bcm2835 uses `/dev/video11` (encoder) and
//!   `/dev/video10` (decoder). Rockchip and other SoCs use different
//!   paths. Override with `V4L2_ENC_DEVICE` and `V4L2_DEC_DEVICE` env
//!   vars for non-Pi hardware.
//!
//! - **Control timing**: On bcm2835, profile and level controls must be
//!   set before `VIDIOC_S_FMT`. On some drivers, controls can be set at
//!   any time before `VIDIOC_STREAMON`.
//!
//! When porting to a new SoC, the recommended approach is:
//! 1. Query supported controls with `VIDIOC_QUERYCTRL` to check which
//!    controls the driver exposes.
//! 2. Test with `v4l2-ctl --list-ctrls-menus` to see defaults.
//! 3. Use ffmpeg's `h264_v4l2m2m` as a reference; its ioctl sequence is
//!    well-tested across many drivers.

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
