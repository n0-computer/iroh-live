//! Raspberry Pi camera capture through `rpicam-vid`.
//!
//! On Raspberry Pi OS the CSI camera is only reachable through the libcamera
//! stack: `/dev/video0` hands back raw Bayer data from the Unicam sensor, which
//! is unusable without the ISP. `rpicam-vid` drives that pipeline and its
//! hardware H.264 encoder, so the cheapest thing a Pi Zero can do is read the
//! Annex-B bytes it writes to stdout and publish them unchanged. That avoids
//! both the raw-YUV pipe (about 10 MB/s at 640x360) and a second encode.
//!
//! Shelling out to a camera app is an application concern rather than a
//! `moq-video` one, which is why this lives here. It produces a
//! [`VideoSource::AnnexB`]; `moq_mux`
//! splits the stream and derives the catalog rendition from its first SPS.

use std::process::Stdio;

use bytes::{Bytes, BytesMut};
use n0_error::{Result, stack_error};
use n0_future::boxed::BoxStream;
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

use crate::publish::VideoSource;

/// The subprocess we drive. Named here so a caller can substitute a wrapper.
const RPICAM_VID: &str = "rpicam-vid";

/// How much stdout to take per read. One access unit at 500 kbps and 30 fps is
/// about 2 KB, so this is a handful of frames per wakeup without the syscall
/// rate of a tiny buffer.
const READ_CHUNK: usize = 32 * 1024;

/// Errors raised while running `rpicam-vid`.
#[stack_error(derive, add_meta, from_sources)]
#[non_exhaustive]
pub enum RpicamError {
    /// The subprocess could not be started, usually because it is not installed.
    #[error("failed to start {RPICAM_VID}")]
    Spawn {
        /// The spawn failure.
        #[error(source, std_err)]
        source: std::io::Error,
    },
    /// The subprocess started but gave us no stdout to read.
    #[error("{RPICAM_VID} produced no output pipe")]
    NoOutput,
}

/// How to run `rpicam-vid`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
    /// Capture width in pixels.
    pub width: u32,
    /// Capture height in pixels.
    pub height: u32,
    /// Capture and encode framerate.
    pub framerate: u32,
    /// Target bitrate in bits per second.
    pub bitrate: u32,
    /// Keyframe interval, in frames.
    ///
    /// Defaults to one second. A subscriber cannot start decoding until the
    /// next keyframe, so this is join latency far more than it is bitrate.
    pub keyframe_interval: u32,
}

impl Config {
    /// Creates a configuration for the given capture mode.
    pub fn new(width: u32, height: u32, framerate: u32) -> Self {
        Self {
            width,
            height,
            framerate,
            bitrate: 500_000,
            keyframe_interval: framerate,
        }
    }

    /// Returns the configuration with a different target bitrate.
    #[must_use]
    pub fn with_bitrate(mut self, bitrate: u32) -> Self {
        self.bitrate = bitrate;
        self
    }

    /// Returns the configuration with a different keyframe interval.
    #[must_use]
    pub fn with_keyframe_interval(mut self, frames: u32) -> Self {
        self.keyframe_interval = frames;
        self
    }

    /// The command line this configuration runs.
    fn args(&self) -> Vec<String> {
        vec![
            "--codec".into(),
            "h264".into(),
            "--inline".into(),
            "--nopreview".into(),
            // Run until killed; the process dies when the source is dropped.
            "--timeout".into(),
            "0".into(),
            "--width".into(),
            self.width.to_string(),
            "--height".into(),
            self.height.to_string(),
            "--framerate".into(),
            self.framerate.to_string(),
            "--bitrate".into(),
            self.bitrate.to_string(),
            "--intra".into(),
            self.keyframe_interval.to_string(),
            "--output".into(),
            "-".into(),
        ]
    }
}

/// Starts `rpicam-vid` and returns its H.264 stream as a video source.
///
/// The subprocess is killed when the returned stream is dropped, because
/// `tokio::process::Child` is configured to kill on drop.
///
/// # Errors
///
/// Fails if `rpicam-vid` is not installed or cannot open the camera.
pub fn open(config: Config) -> Result<VideoSource, RpicamError> {
    let args = config.args();
    info!(
        width = config.width,
        height = config.height,
        framerate = config.framerate,
        bitrate = config.bitrate,
        "starting {RPICAM_VID}",
    );

    let mut child = tokio::process::Command::new(RPICAM_VID)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| n0_error::e!(RpicamError::Spawn { source }))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| n0_error::e!(RpicamError::NoOutput))?;

    // The child rides along in the stream state so the process is killed
    // exactly when the source is dropped, not when this function returns.
    let state = Reader {
        child,
        stdout,
        buffer: BytesMut::with_capacity(READ_CHUNK),
    };
    let stream: BoxStream<Bytes> =
        Box::pin(n0_future::stream::unfold(state, |mut state| async move {
            state.buffer.clear();
            match state.stdout.read_buf(&mut state.buffer).await {
                Ok(0) => {
                    debug!("{RPICAM_VID} closed its output");
                    None
                }
                Ok(_) => {
                    let chunk = state.buffer.split().freeze();
                    Some((chunk, state))
                }
                Err(err) => {
                    warn!(error = %err, "{RPICAM_VID} read failed");
                    None
                }
            }
        }));

    Ok(VideoSource::AnnexB(stream))
}

/// The stream's state: the running process, its output pipe, and the buffer
/// each read fills.
struct Reader {
    /// Killed on drop, which is what stops the camera when the source ends.
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    buffer: BytesMut,
}

impl Drop for Reader {
    fn drop(&mut self) {
        debug!("stopping {RPICAM_VID}");
        // `kill_on_drop` handles the signal; this only makes the intent visible
        // in a log, since a camera that stays on is the failure people notice.
        let _ = self.child.start_kill();
    }
}
