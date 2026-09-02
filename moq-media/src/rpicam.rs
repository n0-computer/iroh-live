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

use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use n0_error::{Result, stack_error};
use n0_future::{
    boxed::BoxStream,
    task::{AbortOnDropHandle, spawn},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing::{debug, info, warn};

use crate::publish::VideoSource;

/// The subprocess we drive. Named here so a caller can substitute a wrapper.
const RPICAM_VID: &str = "rpicam-vid";

/// How much stdout to take per read. One access unit at 500 kbps and 30 fps is
/// about 2 KB, so this is a handful of frames per wakeup without the syscall
/// rate of a tiny buffer.
const READ_CHUNK: usize = 32 * 1024;

/// How many lines of the subprocess's stderr to keep for the exit report.
///
/// `rpicam-vid` says what went wrong in its last line or two, after a banner
/// from libcamera. Ten is enough to carry the reason without holding a log.
const STDERR_TAIL_LINES: usize = 10;

/// How long to wait for the subprocess's exit status once it closes stdout.
///
/// A camera app that has stopped writing is on its way out, so this only
/// exists so that one which is not cannot hang the publish task.
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);

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
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| n0_error::e!(RpicamError::Spawn { source }))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| n0_error::e!(RpicamError::NoOutput))?;

    // Everything that can go wrong with a camera is reported on stderr and
    // nowhere else: a ribbon cable nobody seated reads as "no cameras
    // available" there, and as an empty stdout here. Discarding it turns a
    // one-line diagnosis into a stream that simply never carries a picture.
    let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let stderr_reader = child.stderr.take().map(|stderr| {
        let tail = Arc::clone(&stderr_tail);
        AbortOnDropHandle::new(spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                debug!(line = %line, "{RPICAM_VID} stderr");
                let mut tail = tail.lock().expect("poisoned");
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        }))
    });

    // The child rides along in the stream state so the process is killed
    // exactly when the source is dropped, not when this function returns.
    let state = Reader {
        child,
        stdout,
        buffer: BytesMut::with_capacity(READ_CHUNK),
        stderr_tail,
        stderr_reader,
    };
    let stream: BoxStream<Bytes> =
        Box::pin(n0_future::stream::unfold(state, |mut state| async move {
            // No clear first: `split` below hands the whole buffer on and
            // leaves this one empty, so every read starts from zero length.
            match state.stdout.read_buf(&mut state.buffer).await {
                Ok(0) => {
                    debug!("{RPICAM_VID} closed its output");
                    state.report_exit().await;
                    None
                }
                Ok(_) => {
                    let chunk = state.buffer.split().freeze();
                    Some((chunk, state))
                }
                Err(err) => {
                    warn!(error = %err, "{RPICAM_VID} read failed");
                    state.report_exit().await;
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
    /// The last few lines the subprocess wrote to stderr, which is where it
    /// says why it stopped.
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// Held so the forwarding task stops with the stream rather than outliving
    /// it. `None` only if the child gave us no stderr pipe.
    #[allow(dead_code, reason = "owned for its drop")]
    stderr_reader: Option<AbortOnDropHandle<()>>,
}

impl Reader {
    /// Reports how `rpicam-vid` exited, once it has closed its output.
    ///
    /// A camera app that fails leaves a healthy-looking publisher behind: the
    /// broadcast is announced, the catalog is never written because no SPS
    /// ever arrived, and a subscriber waits on a picture that is not coming.
    /// So a non-zero exit is logged at `warn` with whatever the subprocess
    /// gave as its reason.
    async fn report_exit(&mut self) {
        let status = match tokio::time::timeout(EXIT_TIMEOUT, self.child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(err)) => {
                warn!(error = %err, "could not reap {RPICAM_VID}");
                return;
            }
            Err(_) => {
                warn!(
                    timeout = ?EXIT_TIMEOUT,
                    "{RPICAM_VID} closed its output but is still running",
                );
                return;
            }
        };
        if status.success() {
            debug!(%status, "{RPICAM_VID} exited");
            return;
        }
        let reason = self
            .stderr_tail
            .lock()
            .expect("poisoned")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        warn!(%status, reason = %reason, "{RPICAM_VID} failed");
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        debug!("stopping {RPICAM_VID}");
        // `kill_on_drop` handles the signal; this only makes the intent visible
        // in a log, since a camera that stays on is the failure people notice.
        let _ = self.child.start_kill();
    }
}
