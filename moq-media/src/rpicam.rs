//! Raspberry Pi camera capture through `rpicam-vid`.
//!
//! On Raspberry Pi OS the CSI camera is only reachable through the libcamera
//! stack: `/dev/video0` hands back raw Bayer data from the Unicam sensor, which
//! is unusable without the ISP. `rpicam-vid` drives that pipeline, so this
//! module runs it as a subprocess and reads whatever it writes to stdout.
//!
//! It writes one of two things, and [`Output`] picks which:
//!
//! - [`Output::H264`] is the Annex-B stream the Pi's hardware encoder produced.
//!   It is the cheapest thing a Pi Zero can publish, because it avoids both the
//!   raw-YUV pipe (about 10 MB/s at 640x360) and a second encode. It arrives as
//!   a [`VideoSource::AnnexB`]; `moq_mux` splits the stream and derives the
//!   catalog rendition from its first SPS.
//! - [`Output::I420`] is raw pictures, which arrive as a
//!   [`VideoSource::Frames`] of [`Surface::I420`]. It costs the pipe and an
//!   encode, and it is the only way anything that needs pixels can see this
//!   camera: a preview, a QR scanner, a software encode, or a simulcast ladder
//!   that has to produce the same picture at several sizes.
//!
//! Shelling out to a camera app is an application concern rather than a
//! `moq-video` one, which is why this lives here.

use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use n0_error::{Result, stack_error};
use n0_future::{
    boxed::BoxStream,
    task::{AbortOnDropHandle, spawn},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing::{debug, info, warn};

use crate::{
    publish::VideoSource,
    video::{Frame, I420, Surface},
};

/// The subprocess we drive. Named here so a caller can substitute a wrapper.
const RPICAM_VID: &str = "rpicam-vid";

/// How much stdout to take per read. One H.264 access unit at 500 kbps and
/// 30 fps is about 2 KB, so this is a handful of frames per wakeup without the
/// syscall rate of a tiny buffer. A raw picture is far larger than this and
/// takes several reads, which costs nothing next to the copy each one avoids.
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

/// How long the raw reader watches before judging the picture rate.
///
/// Long enough that a camera still starting up, which delivers its first
/// pictures in a burst, does not read as a fault.
const RATE_CHECK_AFTER: Duration = Duration::from_secs(3);

/// How far over the requested rate the observed one may go before the picture
/// size is the only explanation.
///
/// A camera undershoots its rate all the time and overshoots it never. The
/// smallest padding libcamera applies at these widths is 64 pixels against 640,
/// which is ten percent, so this sits well inside the gap between the two.
const RATE_TOLERANCE: f64 = 1.2;

/// Pixel alignment libcamera gives each row of a raw picture.
///
/// The raw stream carries no strides, so a picture can only be split off it if
/// the rows are tightly packed. They are not always: libcamera rounds the luma
/// row up to a multiple of this and the chroma rows to half of it, and the
/// padding goes down the pipe with everything else.
///
/// Measured on a Pi 4 (rpicam-apps 2024-06-17, libcamera v0.3.0, IMX708): a
/// 1500 ms capture at 320x240, 640x360 and 1280x720 divides exactly into
/// `width * height * 3 / 2` byte pictures, while 642, 656, 672, 688 and 700 all
/// produce the same byte count as 704, 800 produces the same as 832, and 96
/// produces the same as 128. Heights are not padded: 358 and 362 both come out
/// exact.
///
/// So [`Config::raw`] rounds the width up to this rather than accepting a
/// geometry whose padding we would have to reconstruct. Asking for a picture we
/// know the layout of is worth more than honouring a width to the pixel, and it
/// keeps the read path a plain split with no per-row copy.
const RAW_WIDTH_ALIGN: u32 = 64;

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
    /// The raw geometry cannot hold I420 pictures.
    #[error(
        "{width}x{height} cannot be captured as I420: both dimensions must be even and non-zero"
    )]
    Geometry {
        /// The requested width.
        width: u32,
        /// The requested height.
        height: u32,
    },
    /// Pictures are coming out faster than the camera was asked to produce
    /// them, so the size they are being split at is too small.
    #[error(
        "{RPICAM_VID} is producing {observed:.1} pictures a second against the {requested} \
         requested, so {width}x{height} is not the size it is writing: its rows are longer \
         than this geometry implies"
    )]
    PictureRate {
        /// Pictures a second actually split off the stream.
        observed: f64,
        /// The rate the camera was asked for.
        requested: u32,
        /// The width the pictures were split at.
        width: u32,
        /// The height the pictures were split at.
        height: u32,
    },
    /// The raw stream ended part way through a picture.
    ///
    /// The byte count did not divide by the picture size, so the rows were not
    /// the length computed from the geometry. On this camera that means
    /// libcamera padded them, which [`RAW_WIDTH_ALIGN`] is meant to rule out.
    #[error(
        "{RPICAM_VID} wrote {trailing} bytes that are not a whole {width}x{height} \
         picture of {frame} bytes; its rows are not the length this geometry implies"
    )]
    PartialFrame {
        /// Bytes left over when the stream ended.
        trailing: usize,
        /// Bytes one tightly-packed picture should take.
        frame: usize,
        /// The width the pictures were split at.
        width: u32,
        /// The height the pictures were split at.
        height: u32,
    },
}

/// What `rpicam-vid` writes to its stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Annex-B H.264 from the Pi's hardware encoder.
    H264 {
        /// Target bitrate in bits per second.
        bitrate: u32,
        /// Keyframe interval, in frames.
        ///
        /// A subscriber cannot start decoding until the next keyframe, so this
        /// is join latency far more than it is bitrate.
        keyframe_interval: u32,
    },
    /// Raw planar I420 pictures, tightly packed.
    I420,
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
    /// What the camera app hands us.
    pub output: Output,
}

impl Config {
    /// Creates a configuration for the hardware H.264 encoder.
    ///
    /// The bitrate defaults to 500 kbps and the keyframe interval to one
    /// second, both of which [`with_bitrate`](Self::with_bitrate) and
    /// [`with_keyframe_interval`](Self::with_keyframe_interval) replace.
    pub fn new(width: u32, height: u32, framerate: u32) -> Self {
        Self {
            width,
            height,
            framerate,
            output: Output::H264 {
                bitrate: 500_000,
                keyframe_interval: framerate,
            },
        }
    }

    /// Creates a configuration for raw I420 pictures.
    ///
    /// The geometry is rounded up to one whose rows libcamera leaves tightly
    /// packed, because the raw stream carries no strides and a padded picture
    /// cannot be split off it. See [`RAW_WIDTH_ALIGN`] for the measurements.
    /// An encoder downstream scales to whatever the renditions asked for, so
    /// the rounding costs a few columns of capture rather than the geometry the
    /// caller publishes.
    pub fn raw(width: u32, height: u32, framerate: u32) -> Self {
        let capture_width = align_up(width.max(1), RAW_WIDTH_ALIGN);
        let capture_height = even(height.max(1));
        if (capture_width, capture_height) != (width, height) {
            info!(
                requested = format_args!("{width}x{height}"),
                capturing = format_args!("{capture_width}x{capture_height}"),
                align = RAW_WIDTH_ALIGN,
                "rounding the raw capture up to a geometry libcamera does not pad",
            );
        }
        Self {
            width: capture_width,
            height: capture_height,
            framerate,
            output: Output::I420,
        }
    }

    /// Returns the configuration with a different target bitrate.
    ///
    /// Does nothing under [`Output::I420`], where nothing encodes.
    #[must_use]
    pub fn with_bitrate(mut self, bitrate: u32) -> Self {
        if let Output::H264 {
            bitrate: current, ..
        } = &mut self.output
        {
            *current = bitrate;
        }
        self
    }

    /// Returns the configuration with a different keyframe interval.
    ///
    /// Does nothing under [`Output::I420`], where nothing encodes.
    #[must_use]
    pub fn with_keyframe_interval(mut self, frames: u32) -> Self {
        if let Output::H264 {
            keyframe_interval, ..
        } = &mut self.output
        {
            *keyframe_interval = frames;
        }
        self
    }

    /// The command line this configuration runs.
    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--nopreview".to_string(),
            // Run until killed; the process dies when the source is dropped.
            "--timeout".to_string(),
            "0".to_string(),
            "--width".to_string(),
            self.width.to_string(),
            "--height".to_string(),
            self.height.to_string(),
            "--framerate".to_string(),
            self.framerate.to_string(),
            "--output".to_string(),
            "-".to_string(),
        ];
        match self.output {
            Output::H264 {
                bitrate,
                keyframe_interval,
            } => args.extend([
                "--codec".to_string(),
                "h264".to_string(),
                // Repeat the parameter sets before every keyframe, so a
                // subscriber that joined late can start decoding.
                "--inline".to_string(),
                "--bitrate".to_string(),
                bitrate.to_string(),
                "--intra".to_string(),
                keyframe_interval.to_string(),
            ]),
            Output::I420 => args.extend(["--codec".to_string(), "yuv420".to_string()]),
        }
        args
    }
}

/// Starts `rpicam-vid` and returns what it writes as a video source.
///
/// [`Output::H264`] gives a [`VideoSource::AnnexB`] and [`Output::I420`] a
/// [`VideoSource::Frames`]. The subprocess is killed when the returned stream
/// is dropped, because `tokio::process::Child` is configured to kill on drop.
///
/// Raw pictures are stamped on a clock this call starts, which is a few
/// milliseconds behind the broadcast's own. Use [`frames`] with
/// `LocalBroadcast::clock` where audio has to line up with the video exactly.
///
/// # Errors
///
/// Fails if `rpicam-vid` is not installed or cannot open the camera, or if a
/// raw geometry cannot hold I420 pictures.
pub fn open(config: Config) -> Result<VideoSource, RpicamError> {
    match config.output {
        Output::H264 { .. } => Ok(VideoSource::AnnexB(annexb(config)?)),
        Output::I420 => Ok(VideoSource::Frames(frames(config, moq_mux::Clock::new())?)),
    }
}

/// Starts `rpicam-vid` and returns the raw pictures it writes, stamped on
/// `clock`.
///
/// Pass the clock the rest of the broadcast is stamped from, so the video lands
/// on the same timeline as the audio. The stream carries [`Surface::I420`], so
/// anything that reads pixels can take it: an encoder, a preview, or a QR
/// scanner.
///
/// The configuration's [`Output`] is ignored; only its geometry is used, and it
/// has to be one libcamera leaves tightly packed. [`Config::raw`] produces one.
///
/// # Errors
///
/// Fails if `rpicam-vid` is not installed or cannot open the camera, or if the
/// geometry is odd or zero in either dimension.
pub fn frames(config: Config, clock: moq_mux::Clock) -> Result<BoxStream<Frame>, RpicamError> {
    let pictures = Pictures::new(config.width, config.height, config.framerate)?;
    let mut config = config;
    config.output = Output::I420;
    let process = Process::spawn(&config)?;

    let state = Raw {
        process,
        pictures,
        clock,
    };
    Ok(Box::pin(n0_future::stream::unfold(
        state,
        |mut state| async move {
            loop {
                if let Some(picture) = state.pictures.take() {
                    // Checked here rather than at end of stream: `--timeout 0`
                    // means a working camera never closes its output, so the
                    // leftover-bytes check below only ever runs on a stream
                    // that has already stopped.
                    if let Err(err) = state.pictures.check_rate() {
                        warn!(error = %err, "stopping the raw camera stream");
                        return None;
                    }
                    let frame = Frame::new(Surface::I420(picture), timestamp(&state.clock));
                    return Some((frame, state));
                }
                match state
                    .process
                    .stdout
                    .read_buf(state.pictures.buffer_mut())
                    .await
                {
                    Ok(0) => {
                        debug!("{RPICAM_VID} closed its output");
                        // Before the exit report, because a stream that ended
                        // mid-picture says something the exit status does not:
                        // the rows were not the length we split at, so every
                        // picture handed on was a shear of two.
                        if let Err(err) = state.pictures.finish() {
                            warn!(error = %err, "the raw camera stream does not divide into pictures");
                        }
                        state.process.report_exit().await;
                        return None;
                    }
                    Ok(_) => continue,
                    Err(err) => {
                        warn!(error = %err, "{RPICAM_VID} read failed");
                        state.process.report_exit().await;
                        return None;
                    }
                }
            }
        },
    )))
}

/// Starts `rpicam-vid` and returns the Annex-B bytes it writes.
fn annexb(config: Config) -> Result<BoxStream<Bytes>, RpicamError> {
    let process = Process::spawn(&config)?;
    let state = AnnexB {
        process,
        buffer: BytesMut::with_capacity(READ_CHUNK),
    };
    Ok(Box::pin(n0_future::stream::unfold(
        state,
        |mut state| async move {
            // No clear first: `split` below hands the whole buffer on and
            // leaves this one empty, so every read starts from zero length.
            match state.process.stdout.read_buf(&mut state.buffer).await {
                Ok(0) => {
                    debug!("{RPICAM_VID} closed its output");
                    state.process.report_exit().await;
                    None
                }
                Ok(_) => {
                    let chunk = state.buffer.split().freeze();
                    Some((chunk, state))
                }
                Err(err) => {
                    warn!(error = %err, "{RPICAM_VID} read failed");
                    state.process.report_exit().await;
                    None
                }
            }
        },
    )))
}

/// The Annex-B stream's state: the running process and the buffer each read
/// fills.
struct AnnexB {
    process: Process,
    buffer: BytesMut,
}

/// The raw stream's state: the running process, the split, and the clock its
/// pictures are stamped from.
struct Raw {
    process: Process,
    pictures: Pictures,
    clock: moq_mux::Clock,
}

/// Splits `rpicam-vid`'s raw output into tightly-packed I420 pictures.
///
/// The stream is a run of fixed-size pictures with nothing framing them, so the
/// only thing that says where one ends is the geometry. That makes a wrong
/// geometry silent rather than loud, which is why [`finish`](Self::finish)
/// exists: leftover bytes at the end are the one signal that the size we split
/// at was not the size the camera wrote.
struct Pictures {
    width: u32,
    height: u32,
    /// Bytes of one picture: Y, then U, then V, with no row padding.
    frame: usize,
    buffer: BytesMut,
    /// The rate the camera was asked for, and what has been split off so far,
    /// so a picture size that is wrong can be caught while the camera runs.
    framerate: u32,
    started: Instant,
    taken: u64,
    reported: bool,
}

impl Pictures {
    /// Creates a split for pictures of the given geometry.
    ///
    /// # Errors
    ///
    /// Fails if either dimension is odd or zero, which 4:2:0 chroma cannot
    /// describe.
    fn new(width: u32, height: u32, framerate: u32) -> Result<Self, RpicamError> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(n0_error::e!(RpicamError::Geometry { width, height }));
        }
        let frame = I420::len(width, height);
        Ok(Self {
            width,
            height,
            frame,
            buffer: BytesMut::with_capacity(frame),
            framerate,
            started: Instant::now(),
            taken: 0,
            reported: false,
        })
    }

    /// Reports whether pictures are coming out faster than the camera can be
    /// producing them, which is what a picture size that is too small looks
    /// like from here.
    ///
    /// [`finish`](Self::finish) cannot catch that case. `--timeout 0` means a
    /// working camera never closes its output, so the leftover-bytes check runs
    /// only when the stream has already ended, and by then a whole session has
    /// been handed on sheared. Splitting a padded stream at the unpadded size
    /// does not fail, it drifts: each picture starts a little further into the
    /// one the camera actually wrote, and more of them come out than went in.
    /// So the tell is the rate, and it is a large one. A 640-wide request that
    /// libcamera pads to a 704 stride yields about ten percent more pictures a
    /// second, and nothing else makes a camera exceed the rate it was asked
    /// for.
    fn check_rate(&mut self) -> Result<(), RpicamError> {
        if self.reported || self.framerate == 0 {
            return Ok(());
        }
        let elapsed = self.started.elapsed();
        if elapsed < RATE_CHECK_AFTER {
            return Ok(());
        }
        self.reported = true;
        let observed = self.taken as f64 / elapsed.as_secs_f64();
        let allowed = f64::from(self.framerate) * RATE_TOLERANCE;
        if observed <= allowed {
            return Ok(());
        }
        Err(n0_error::e!(RpicamError::PictureRate {
            observed,
            requested: self.framerate,
            width: self.width,
            height: self.height,
        }))
    }

    /// The buffer to read into, with room for at least one more chunk.
    fn buffer_mut(&mut self) -> &mut BytesMut {
        self.buffer.reserve(READ_CHUNK);
        &mut self.buffer
    }

    /// Takes the next whole picture, if the buffer holds one.
    fn take(&mut self) -> Option<I420> {
        if self.buffer.len() < self.frame {
            return None;
        }
        let data: Vec<u8> = self.buffer.split_to(self.frame).into();
        // `I420::new` rejects only an odd or zero dimension and a buffer of the
        // wrong length. `new` checked the geometry, and `frame` is `I420::len`
        // of it, so this split is exactly the length it wants.
        let picture = I420::new(self.width, self.height, data)
            .expect("the geometry and the length were both checked");
        self.taken += 1;
        Some(picture)
    }

    /// Checks that the stream ended on a picture boundary.
    ///
    /// # Errors
    ///
    /// Fails naming the leftover byte count if it did not. That is what a row
    /// stride other than the one this geometry implies looks like from here,
    /// and it means the pictures already handed on were sheared.
    fn finish(&self) -> Result<(), RpicamError> {
        match self.buffer.len() {
            0 => Ok(()),
            trailing => Err(n0_error::e!(RpicamError::PartialFrame {
                trailing,
                frame: self.frame,
                width: self.width,
                height: self.height,
            })),
        }
    }
}

/// A running `rpicam-vid`, its output pipe, and the stderr that says why it
/// stopped.
struct Process {
    /// Killed on drop, which is what stops the camera when the source ends.
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    /// The last few lines the subprocess wrote to stderr, which is where it
    /// says why it stopped.
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// Held so the forwarding task stops with the stream rather than outliving
    /// it. `None` only if the child gave us no stderr pipe.
    #[allow(dead_code, reason = "owned for its drop")]
    stderr_reader: Option<AbortOnDropHandle<()>>,
}

impl Process {
    /// Starts `rpicam-vid` with the command line `config` describes.
    ///
    /// # Errors
    ///
    /// Fails if the subprocess cannot be started, or starts without a stdout
    /// pipe.
    fn spawn(config: &Config) -> Result<Self, RpicamError> {
        let args = config.args();
        info!(
            width = config.width,
            height = config.height,
            framerate = config.framerate,
            output = ?config.output,
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

        Ok(Self {
            child,
            stdout,
            stderr_tail,
            stderr_reader,
        })
    }

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
                warn!(error = %err, "could not collect {RPICAM_VID}'s exit status");
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
        // Wait for the forwarding task before reading what it collected. The
        // child has exited, so its stderr is at end of file and the task is
        // about to finish, but "about to" is not "has": a camera app that fails
        // on startup writes its reason and exits within a millisecond or two,
        // and reading the ring first reports `reason=` empty on exactly the
        // failure the reason was wanted for. Seen doing it, on a second
        // publisher finding the camera busy.
        if let Some(task) = self.stderr_reader.take()
            && tokio::time::timeout(EXIT_TIMEOUT, task).await.is_err()
        {
            debug!("{RPICAM_VID}'s stderr did not end with it");
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

impl Drop for Process {
    fn drop(&mut self) {
        debug!("stopping {RPICAM_VID}");
        // `kill_on_drop` handles the signal; this only makes the intent visible
        // in a log, since a camera that stays on is the failure people notice.
        let _ = self.child.start_kill();
    }
}

/// Stamps a picture with the time since `clock` started.
///
/// The same reading `moq-media`'s capture path takes, so a raw camera and a
/// microphone handed the same clock land on one timeline.
fn timestamp(clock: &moq_mux::Clock) -> moq_net::Timestamp {
    // u64 microseconds only overflows a Timestamp after ~584,000 years of
    // uptime, so there is no failure to report here.
    moq_net::Timestamp::from_micros(clock.micros()).expect("clock micros out of range")
}

/// Rounds `value` up to the next multiple of `align`.
fn align_up(value: u32, align: u32) -> u32 {
    value.div_ceil(align) * align
}

/// Rounds `value` up to the next even number.
fn even(value: u32) -> u32 {
    value + (value % 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A geometry the Pi 4 was measured to leave tightly packed.
    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 360;

    /// Bytes one 640x360 I420 picture takes: 345,600.
    const FRAME: usize = (WIDTH * HEIGHT) as usize * 3 / 2;

    /// Feeds `bytes` to a split in chunks that do not line up with picture
    /// boundaries, which is what reading a pipe gives.
    fn split(pictures: &mut Pictures, bytes: &[u8], chunk: usize) -> Vec<I420> {
        let mut taken = Vec::new();
        for part in bytes.chunks(chunk) {
            pictures.buffer_mut().extend_from_slice(part);
            while let Some(picture) = pictures.take() {
                taken.push(picture);
            }
        }
        taken
    }

    #[test]
    fn a_whole_number_of_pictures_splits_into_that_many() {
        let mut pictures = Pictures::new(WIDTH, HEIGHT, 30).expect("640x360 is even");
        assert_eq!(pictures.frame, FRAME);

        let stream = vec![0u8; FRAME * 3];
        let taken = split(&mut pictures, &stream, READ_CHUNK);

        assert_eq!(taken.len(), 3);
        for picture in &taken {
            assert_eq!(picture.width(), WIDTH);
            assert_eq!(picture.height(), HEIGHT);
            // Y is `width * height`, U and V a quarter of that each.
            assert_eq!(picture.data().len(), FRAME);
        }
        pictures.finish().expect("the stream ended on a boundary");
    }

    /// Each picture is handed on whole and in order, rather than sheared across
    /// the reads that carried it.
    #[test]
    fn a_picture_is_not_sheared_by_the_reads_that_carried_it() {
        let mut pictures = Pictures::new(WIDTH, HEIGHT, 30).expect("640x360 is even");
        let mut stream = Vec::with_capacity(FRAME * 4);
        for index in 0..4u8 {
            stream.extend(std::iter::repeat_n(index, FRAME));
        }

        // A chunk size that is coprime with the picture size, so no read ends
        // where a picture does.
        let taken = split(&mut pictures, &stream, 7_777);

        assert_eq!(taken.len(), 4);
        for (index, picture) in taken.iter().enumerate() {
            let expected = u8::try_from(index).expect("four pictures");
            assert!(
                picture.data().iter().all(|byte| *byte == expected),
                "picture {index} carries bytes from another",
            );
        }
    }

    /// A padded row stride shows up here as a stream that does not divide, and
    /// the leftover count is what says by how much.
    #[test]
    fn a_stream_that_does_not_divide_is_reported() {
        let mut pictures = Pictures::new(WIDTH, HEIGHT, 30).expect("640x360 is even");

        // What a 704-byte row stride at a 640 pixel width would write: two
        // pictures' worth of padded rows, which is more than two of ours.
        let padded = (704 * HEIGHT) as usize * 3 / 2;
        let stream = vec![0u8; padded * 2];
        let taken = split(&mut pictures, &stream, READ_CHUNK);

        assert_eq!(taken.len(), stream.len() / FRAME);
        let err = pictures
            .finish()
            .expect_err("the leftover is not a picture");
        assert!(
            format!("{err}").contains(&(stream.len() % FRAME).to_string()),
            "{err}",
        );
    }

    #[test]
    fn an_odd_geometry_is_refused() {
        assert!(Pictures::new(641, 360, 30).is_err());
        assert!(Pictures::new(640, 361, 30).is_err());
        assert!(Pictures::new(0, 360, 30).is_err());
    }

    /// The raw geometry is rounded up to rows libcamera does not pad, so the
    /// split above always has a whole number of pictures to find.
    #[test]
    fn raw_capture_rounds_up_to_an_unpadded_geometry() {
        assert_eq!(Config::raw(640, 360, 30).width, 640);
        assert_eq!(Config::raw(1280, 720, 30).width, 1280);
        assert_eq!(Config::raw(854, 480, 30).width, 896);
        assert_eq!(Config::raw(700, 360, 30).width, 704);
        assert_eq!(Config::raw(96, 64, 30).width, 128);
        assert_eq!(Config::raw(640, 361, 30).height, 362);
        assert_eq!(Config::raw(0, 0, 30).width, RAW_WIDTH_ALIGN);
    }

    #[test]
    fn the_output_picks_the_codec_flag() {
        let h264 = Config::new(640, 360, 30).args().join(" ");
        assert!(h264.contains("--codec h264"), "{h264}");
        assert!(h264.contains("--bitrate 500000"), "{h264}");
        assert!(h264.contains("--intra 30"), "{h264}");

        let raw = Config::raw(640, 360, 30).args().join(" ");
        assert!(raw.contains("--codec yuv420"), "{raw}");
        assert!(!raw.contains("--bitrate"), "{raw}");
        assert!(!raw.contains("--intra"), "{raw}");
    }

    /// The encoder settings belong to the H.264 output, so a raw configuration
    /// has nothing for them to change.
    #[test]
    fn encoder_settings_only_apply_to_the_encoded_output() {
        assert_eq!(
            Config::new(640, 360, 30)
                .with_bitrate(2_000_000)
                .with_keyframe_interval(60)
                .output,
            Output::H264 {
                bitrate: 2_000_000,
                keyframe_interval: 60,
            }
        );
        assert_eq!(
            Config::raw(640, 360, 30).with_bitrate(2_000_000).output,
            Output::I420
        );
    }
}

#[cfg(test)]
mod rate_check_tests {
    use super::{Pictures, RATE_CHECK_AFTER};

    /// A stream split at the right size stays under the rate limit, so the
    /// check does not fire on a camera that is working.
    #[test]
    fn a_correct_split_passes() {
        let mut pictures = Pictures::new(64, 64, 30).expect("64x64 is even");
        pictures.started = std::time::Instant::now() - RATE_CHECK_AFTER;
        pictures.taken = 30 * RATE_CHECK_AFTER.as_secs();
        assert!(pictures.check_rate().is_ok());
    }

    /// Splitting a padded stream at the unpadded size yields more pictures
    /// than the camera wrote, which is the only way to notice while it runs.
    #[test]
    fn too_many_pictures_a_second_is_reported() {
        let mut pictures = Pictures::new(64, 64, 30).expect("64x64 is even");
        pictures.started = std::time::Instant::now() - RATE_CHECK_AFTER;
        // A 704 stride against a 640 request is ten percent more pictures;
        // this is that, with room to spare.
        pictures.taken = 45 * RATE_CHECK_AFTER.as_secs();
        let err = pictures
            .check_rate()
            .expect_err("45fps against 30 requested");
        assert!(format!("{err}").contains("pictures a second"), "{err}");
    }

    /// Reported once. A stream that trips the check and is somehow kept
    /// running should not log on every picture.
    #[test]
    fn the_rate_is_reported_once() {
        let mut pictures = Pictures::new(64, 64, 30).expect("64x64 is even");
        pictures.started = std::time::Instant::now() - RATE_CHECK_AFTER;
        pictures.taken = 45 * RATE_CHECK_AFTER.as_secs();
        assert!(pictures.check_rate().is_err());
        assert!(pictures.check_rate().is_ok());
    }
}
