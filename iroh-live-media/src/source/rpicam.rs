//! Raspberry Pi camera capture through `rpicam-vid`.
//!
//! On Raspberry Pi OS the CSI camera is only reachable through libcamera.
//! `/dev/video0` returns raw Bayer data from the Unicam sensor, which is
//! unusable without the ISP. This module runs `rpicam-vid` as a subprocess and
//! reads its stdout, which carries one of two [`Output`]s:
//!
//! - [`Output::H264`] is Annex-B from the Pi's hardware encoder. It becomes an
//!   [`EncodedVideoSource`](crate::EncodedVideoSource), and it avoids both the
//!   raw pipe (about 10 MB/s at 640x360) and a second encode.
//! - [`Output::I420`] is raw pictures, which become a
//!   [`VideoSource`](crate::VideoSource) of [`Surface::I420`]. It costs the
//!   pipe and an encode. It is the only path for anything that needs pixels,
//!   such as a preview, a QR scanner or a rendition ladder.
//!
//! The raw path takes a [`RawConfig`], whose geometry is aligned so libcamera
//! does not pad its rows.

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
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::VideoFormat;
use crate::{
    Bitrate,
    error::Error,
    frames::FrameSlot,
    local_task::LocalTask,
    video::{Frame, I420, Size, Surface},
};

/// The camera app this module runs.
const RPICAM_VID: &str = "rpicam-vid";

/// How much stdout to take per read.
///
/// One H.264 access unit at 500 kbps and 30 fps is about 2 KB, so one read
/// takes several frames. A raw picture takes several reads.
const READ_CHUNK: usize = 32 * 1024;

/// How many lines of the subprocess's stderr to keep for the exit report.
///
/// `rpicam-vid` gives its reason in the last line or two, after a libcamera
/// banner.
const STDERR_TAIL_LINES: usize = 10;

/// How long to wait for the exit status once the subprocess closes stdout.
///
/// It only matters for an app that stops writing but does not exit.
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);

/// The pixel alignment libcamera gives each row of a raw picture.
///
/// The raw stream carries no strides, so pictures can only be split off it if
/// the rows are tightly packed. libcamera rounds the luma row up to a multiple
/// of this and the chroma rows to half of it, and the padding goes down the
/// pipe.
///
/// Measured on a Pi 4 (rpicam-apps 2024-06-17, libcamera v0.3.0, IMX708) with
/// 1500 ms captures: 320x240, 640x360 and 1280x720 divide exactly into
/// `width * height * 3 / 2` byte pictures. Widths 642, 656, 672, 688 and 700
/// give the same byte count as 704, 800 the same as 832, and 96 the same as
/// 128. Heights are not padded: 358 and 362 come out exact.
///
/// [`RawConfig::new`] rounds the width up to this, so reading is a plain split
/// with no per-row copy.
const RAW_WIDTH_ALIGN: u32 = 64;

/// Errors raised while running `rpicam-vid`.
#[stack_error(derive, add_meta, from_sources)]
pub(crate) enum RpicamError {
    /// The subprocess could not be started, usually because it is not installed.
    #[error("failed to start {RPICAM_VID}")]
    Spawn {
        /// The spawn failure.
        #[error(source, std_err)]
        source: std::io::Error,
    },
    /// The subprocess started without a stdout pipe.
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
    /// The raw stream ended part way through a picture.
    ///
    /// The rows were not the length the geometry implies. On this camera that
    /// means libcamera padded them, which [`RAW_WIDTH_ALIGN`] should prevent.
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

/// Target bitrate for [`Output::H264`] when the caller names none.
///
/// 500 kbps keeps 640x360 from the Pi's encoder clean. On these machines the
/// uplink is the limit long before the encoder.
pub(crate) const DEFAULT_BITRATE: u32 = 500_000;

/// What `rpicam-vid` writes to its stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Output {
    /// Annex-B H.264 from the Pi's hardware encoder.
    H264 {
        /// Target bitrate in bits per second.
        bitrate: u32,
        /// Keyframe interval, in frames.
        ///
        /// A subscriber cannot start decoding before the next keyframe, so this
        /// sets join latency more than bitrate.
        keyframe_interval: u32,
    },
    /// Raw planar I420 pictures, tightly packed.
    I420,
}

/// How to run `rpicam-vid`.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Capture width in pixels.
    pub(crate) width: u32,
    /// Capture height in pixels.
    pub(crate) height: u32,
    /// Capture and encode framerate.
    pub(crate) framerate: u32,
    /// What the camera app writes to stdout.
    pub(crate) output: Output,
}

/// A raw capture geometry that libcamera does not pad, and its frame rate.
///
/// [`RawConfig::new`] is the only constructor, and it rounds the width up to
/// [`RAW_WIDTH_ALIGN`]. That lets [`frames`] split pictures by size alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawConfig {
    width: u32,
    height: u32,
    framerate: u32,
}

impl RawConfig {
    /// Creates a raw capture config, rounding the geometry up to avoid padding.
    ///
    /// The encoder scales to the renditions, so the rounding costs a few
    /// captured columns and does not change what is published.
    pub(crate) fn new(width: u32, height: u32, framerate: u32) -> Self {
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
        }
    }

    /// Returns the [`Config`] that runs `rpicam-vid` for this capture.
    fn config(self) -> Config {
        Config {
            width: self.width,
            height: self.height,
            framerate: self.framerate,
            output: Output::I420,
        }
    }
}

impl Config {
    /// Returns the command line for this config.
    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--nopreview".to_string(),
            // Run until killed. The process dies with the source.
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

/// How to run the Raspberry Pi camera.
#[derive(Debug, Clone)]
pub struct RpicamConfig {
    /// The capture size.
    ///
    /// The raw path rounds the width up to a multiple of 64, so libcamera does
    /// not pad the rows.
    pub size: Size,
    /// Frames per second.
    pub framerate: u32,
    /// The hardware encoder's target bitrate, for the encoded path.
    pub bitrate: Bitrate,
    /// The keyframe interval in frames, for the encoded path.
    ///
    /// A subscriber cannot start decoding before the next keyframe, so this
    /// sets join latency more than bitrate.
    pub keyframe_interval: u32,
}

impl RpicamConfig {
    /// Creates a config for `size` at `framerate`.
    ///
    /// The encoded path defaults to 500 kbit/s with a keyframe every second.
    pub fn new(size: Size, framerate: u32) -> Self {
        Self {
            size,
            framerate,
            bitrate: Bitrate::from_bps(u64::from(DEFAULT_BITRATE)),
            keyframe_interval: framerate.max(1),
        }
    }
}

/// Starts `rpicam-vid` for the H.264 its hardware encoder writes.
pub(super) fn open_encoded(config: RpicamConfig) -> Result<BoxStream<Bytes>, Error> {
    let bitrate = u32::try_from(config.bitrate.as_bps()).map_err(|_| {
        Error::invalid(format!(
            "rpicam-vid takes at most {} bits per second, not {}",
            u32::MAX,
            config.bitrate.as_bps()
        ))
    })?;
    let output = Output::H264 {
        bitrate,
        keyframe_interval: config.keyframe_interval,
    };
    annexb(Config {
        width: config.size.width,
        height: config.size.height,
        framerate: config.framerate,
        output,
    })
    .map_err(Error::device)
}

/// Starts `rpicam-vid` for raw pictures and waits for the first one.
///
/// Pictures are read into `slot` on a thread of their own.
pub(super) async fn open_raw(
    config: RpicamConfig,
    slot: FrameSlot,
    stop: CancellationToken,
) -> Result<(VideoFormat, LocalTask), Error> {
    let raw = RawConfig::new(config.size.width, config.size.height, config.framerate);
    let rate = crate::video::Rate::new(raw.framerate.max(1), 1)
        .map_err(|err| Error::invalid(err.to_string()))?;
    let format = VideoFormat {
        size: Size::new(raw.width, raw.height),
        rate,
    };
    let mut pictures = frames(raw, moq_mux::Clock::new()).map_err(Error::device)?;
    let (first_tx, first) = tokio::sync::oneshot::channel();
    let task = crate::local_task::spawn("rpicam", stop, move |stop| async move {
        use n0_future::StreamExt;
        let mut first_tx = Some(first_tx);
        loop {
            let frame = tokio::select! {
                frame = pictures.next() => frame,
                () = stop.cancelled() => break,
            };
            let Some(frame) = frame else {
                slot.fail(std::sync::Arc::new(Error::device_msg(format!(
                    "{RPICAM_VID} stopped writing pictures"
                ))));
                break;
            };
            slot.send(std::sync::Arc::new(frame));
            if let Some(tx) = first_tx.take() {
                let _ = tx.send(());
            }
        }
    })?;
    match tokio::time::timeout(super::FIRST_FRAME_PATIENCE, first).await {
        Ok(Ok(())) => Ok((format, task)),
        _ => Err(Error::device_msg(format!(
            "{RPICAM_VID} produced no picture; is the camera connected?"
        ))),
    }
}

/// Starts `rpicam-vid` and returns its raw pictures, stamped on `clock`.
///
/// The broadcast restamps what it reads, so `clock` only has to be monotonic.
///
/// # Errors
///
/// Fails if `rpicam-vid` is not installed or cannot open the camera, or if the
/// geometry is odd or zero in either dimension.
fn frames(config: RawConfig, clock: moq_mux::Clock) -> Result<BoxStream<Frame>, RpicamError> {
    let pictures = Pictures::new(config.width, config.height)?;
    let process = Process::spawn(&config.config())?;

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
                    let frame = Frame::new(Surface::I420(picture), state.clock.now());
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
                        // A stream that ended mid-picture means the rows were
                        // not the length we split at, and every picture was
                        // sheared. The exit status does not show that.
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
            // `split` below empties the buffer, so each read starts from zero
            // length.
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

/// The state of the Annex-B stream.
struct AnnexB {
    process: Process,
    buffer: BytesMut,
}

/// The state of the raw stream.
struct Raw {
    process: Process,
    pictures: Pictures,
    clock: moq_mux::Clock,
}

/// Splits `rpicam-vid`'s raw output into tightly-packed I420 pictures.
///
/// Nothing frames the pictures, so only the geometry says where one ends, and
/// a wrong geometry goes unnoticed while reading. Leftover bytes at the end,
/// reported by [`finish`](Self::finish), are the one sign of it.
struct Pictures {
    width: u32,
    height: u32,
    /// Bytes of one picture: Y, then U, then V, with no row padding.
    frame: usize,
    buffer: BytesMut,
}

impl Pictures {
    /// Creates a split for pictures of the given geometry.
    ///
    /// Fails if either dimension is odd or zero, which 4:2:0 cannot describe.
    fn new(width: u32, height: u32) -> Result<Self, RpicamError> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(n0_error::e!(RpicamError::Geometry { width, height }));
        }
        // After the check above, `len` only refuses a geometry too large to
        // address.
        let frame = I420::len(Size::new(width, height))
            .map_err(|_| n0_error::e!(RpicamError::Geometry { width, height }))?;
        Ok(Self {
            width,
            height,
            frame,
            buffer: BytesMut::with_capacity(frame),
        })
    }

    /// Returns the buffer to read into, with room for at least one more chunk.
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
        // `new` checked the geometry and `frame` is `I420::len` of it, so
        // `I420::new` cannot fail.
        let picture = I420::new(Size::new(self.width, self.height), data)
            .expect("the geometry and the length were both checked");
        Some(picture)
    }

    /// Checks that the stream ended on a picture boundary.
    ///
    /// Leftover bytes mean the camera wrote a different row stride and the
    /// pictures handed on were sheared. The error names the leftover count.
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

/// A running `rpicam-vid` with its stdout and the tail of its stderr.
struct Process {
    /// Killed on drop, which stops the camera.
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    /// The last few lines the subprocess wrote to stderr.
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// The task that collects stderr, stopped with the stream.
    ///
    /// `None` if the child had no stderr pipe.
    stderr_reader: Option<AbortOnDropHandle<()>>,
}

impl Process {
    /// Starts `rpicam-vid` with the command line `config` describes.
    ///
    /// Fails if the subprocess cannot be started or has no stdout pipe.
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

        // The camera app reports every problem on stderr only. An unseated
        // ribbon cable reads as "no cameras available" there, and as an empty
        // stdout here.
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
    /// A failed camera app leaves a publisher that looks healthy. The broadcast
    /// is announced, but no SPS arrives, so the catalog is never written. A
    /// non-zero exit is therefore logged at `warn` with the reason from stderr.
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
        // Wait for the stderr task before reading the tail. An app that fails
        // on startup exits a millisecond after writing its reason, and reading
        // first would report an empty reason.
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
        // `kill_on_drop` already sends the signal. This impl exists for the
        // log line, since a camera that stays on is the failure people notice.
        let _ = self.child.start_kill();
    }
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

    /// Feeds `bytes` to a split in chunks, the way reads from a pipe arrive.
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
        let mut pictures = Pictures::new(WIDTH, HEIGHT).expect("640x360 is even");
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

    /// Each picture comes out whole and in order, whatever the read boundaries.
    #[test]
    fn a_picture_is_not_sheared_by_the_reads_that_carried_it() {
        let mut pictures = Pictures::new(WIDTH, HEIGHT).expect("640x360 is even");
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

    /// A padded row stride shows as leftover bytes, and the error names them.
    #[test]
    fn a_stream_that_does_not_divide_is_reported() {
        let mut pictures = Pictures::new(WIDTH, HEIGHT).expect("640x360 is even");

        // Two pictures with a 704-byte row stride at a 640 pixel width.
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
        assert!(Pictures::new(641, 360).is_err());
        assert!(Pictures::new(640, 361).is_err());
        assert!(Pictures::new(0, 360).is_err());
    }

    /// A 640x360 capture at 30 fps writing H.264.
    fn h264(bitrate: u32, keyframe_interval: u32) -> Config {
        Config {
            width: 640,
            height: 360,
            framerate: 30,
            output: Output::H264 {
                bitrate,
                keyframe_interval,
            },
        }
    }

    /// The raw width rounds up to a multiple of 64 and the height to even.
    #[test]
    fn raw_capture_rounds_up_to_an_unpadded_geometry() {
        assert_eq!(RawConfig::new(640, 360, 30).width, 640);
        assert_eq!(RawConfig::new(1280, 720, 30).width, 1280);
        assert_eq!(RawConfig::new(854, 480, 30).width, 896);
        assert_eq!(RawConfig::new(700, 360, 30).width, 704);
        assert_eq!(RawConfig::new(96, 64, 30).width, 128);
        assert_eq!(RawConfig::new(640, 361, 30).height, 362);
        assert_eq!(RawConfig::new(0, 0, 30).width, RAW_WIDTH_ALIGN);
    }

    #[test]
    fn the_output_picks_the_codec_flag() {
        let h264 = h264(DEFAULT_BITRATE, 30).args().join(" ");
        assert!(h264.contains("--codec h264"), "{h264}");
        assert!(h264.contains("--bitrate 500000"), "{h264}");
        assert!(h264.contains("--intra 30"), "{h264}");

        let raw = RawConfig::new(640, 360, 30).config().args().join(" ");
        assert!(raw.contains("--codec yuv420"), "{raw}");
        assert!(!raw.contains("--bitrate"), "{raw}");
        assert!(!raw.contains("--intra"), "{raw}");
    }

    /// The bitrate and keyframe interval reach the command line.
    #[test]
    fn the_encoder_settings_reach_the_command_line() {
        let args = h264(2_000_000, 60).args().join(" ");
        assert!(args.contains("--bitrate 2000000"), "{args}");
        assert!(args.contains("--intra 60"), "{args}");
    }

    /// A raw capture runs `rpicam-vid` at its aligned geometry.
    #[test]
    fn a_raw_capture_runs_at_the_geometry_it_was_aligned_to() {
        let raw = RawConfig::new(854, 480, 30);
        let args = raw.config().args().join(" ");
        assert!(args.contains("--width 896"), "{args}");
        assert!(args.contains("--height 480"), "{args}");
        assert_eq!(raw.config().output, Output::I420);
    }
}
