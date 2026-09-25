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
//! The raw path aligns its geometry with [`aligned`], so libcamera does not
//! pad its rows.

use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
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
/// [`aligned`] rounds the width up to this, so reading is a plain split with no
/// per-row copy.
const RAW_WIDTH_ALIGN: u32 = 64;

/// Target bitrate for [`Output::H264`] when the caller names none.
///
/// 500 kbps keeps 640x360 from the Pi's encoder clean. On these machines the
/// uplink is the limit long before the encoder.
const DEFAULT_BITRATE: u32 = 500_000;

/// What `rpicam-vid` writes to its stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
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

/// Returns the raw capture size nearest above `size` that libcamera does not pad.
///
/// The width rounds up to [`RAW_WIDTH_ALIGN`] and the height to even. That lets
/// [`frames`] split pictures by size alone. The encoder scales to the
/// renditions, so the rounding costs a few captured columns and does not change
/// what is published.
fn aligned(size: Size) -> Size {
    let capture = Size::new(
        size.width.max(1).div_ceil(RAW_WIDTH_ALIGN) * RAW_WIDTH_ALIGN,
        size.height.max(1).next_multiple_of(2),
    );
    if capture != size {
        info!(
            requested = %size,
            capturing = %capture,
            align = RAW_WIDTH_ALIGN,
            "rounding the raw capture up to a geometry libcamera does not pad",
        );
    }
    capture
}

/// Returns the `rpicam-vid` command line for `output` at `size` and `framerate`.
fn args(size: Size, framerate: u32, output: Output) -> Vec<String> {
    let mut args = vec![
        "--nopreview".to_string(),
        // Run until killed. The process dies with the source.
        "--timeout".to_string(),
        "0".to_string(),
        "--width".to_string(),
        size.width.to_string(),
        "--height".to_string(),
        size.height.to_string(),
        "--framerate".to_string(),
        framerate.to_string(),
        "--output".to_string(),
        "-".to_string(),
    ];
    match output {
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
    annexb(config.size, config.framerate, output)
}

/// Starts `rpicam-vid` for raw pictures and waits for the first one.
///
/// Pictures are read into `slot` on a thread of their own.
pub(super) async fn open_raw(
    config: RpicamConfig,
    slot: FrameSlot,
    stop: CancellationToken,
) -> Result<(VideoFormat, LocalTask), Error> {
    let size = aligned(config.size);
    let rate = crate::video::Rate::new(config.framerate.max(1), 1)
        .map_err(|err| Error::invalid(err.to_string()))?;
    let format = VideoFormat { size, rate };
    let mut pictures = frames(size, config.framerate, moq_mux::Clock::new())?;
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
/// Fails if `rpicam-vid` does not start, or if `size` is odd or zero in either
/// dimension.
fn frames(size: Size, framerate: u32, clock: moq_mux::Clock) -> Result<BoxStream<Frame>, Error> {
    let pictures = Pictures::new(size)?;
    let process = Process::spawn(size, framerate, Output::I420)?;

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
                        let trailing = state.pictures.leftover();
                        if trailing > 0 {
                            warn!(
                                trailing,
                                frame = state.pictures.frame,
                                size = %state.pictures.size,
                                "the raw camera stream ended part way through a picture; \
                                 its rows are not the length this size implies",
                            );
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
fn annexb(size: Size, framerate: u32, output: Output) -> Result<BoxStream<Bytes>, Error> {
    let process = Process::spawn(size, framerate, output)?;
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
/// counted by [`leftover`](Self::leftover), are the one sign of it.
struct Pictures {
    size: Size,
    /// Bytes of one picture: Y, then U, then V, with no row padding.
    frame: usize,
    buffer: BytesMut,
}

impl Pictures {
    /// Creates a split for pictures of `size`.
    ///
    /// Fails if either dimension is odd or zero, which 4:2:0 cannot describe.
    fn new(size: Size) -> Result<Self, Error> {
        let frame = I420::len(size).map_err(|err| Error::invalid(err.to_string()))?;
        Ok(Self {
            size,
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
        // `new` checked the size and `frame` is `I420::len` of it, so
        // `I420::new` cannot fail.
        let picture =
            I420::new(self.size, data).expect("the size and the length were both checked");
        Some(picture)
    }

    /// Returns the bytes that do not make a whole picture.
    ///
    /// Bytes left at the end of the stream mean the camera wrote another row
    /// stride, and the pictures handed on were sheared.
    fn leftover(&self) -> usize {
        self.buffer.len()
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
    /// Starts `rpicam-vid` writing `output` at `size` and `framerate`.
    ///
    /// Fails if the subprocess cannot be started, usually because it is not
    /// installed, or has no stdout pipe.
    fn spawn(size: Size, framerate: u32, output: Output) -> Result<Self, Error> {
        info!(%size, framerate, ?output, "starting {RPICAM_VID}");

        let mut child = tokio::process::Command::new(RPICAM_VID)
            .args(args(size, framerate, output))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| Error::device_msg(format!("failed to start {RPICAM_VID}: {err}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::device_msg(format!("{RPICAM_VID} produced no output pipe")))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A geometry the Pi 4 was measured to leave tightly packed.
    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 360;

    /// Bytes one 640x360 I420 picture takes: 345,600.
    const FRAME: usize = (WIDTH * HEIGHT) as usize * 3 / 2;

    const SIZE: Size = Size {
        width: WIDTH,
        height: HEIGHT,
    };

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
        let mut pictures = Pictures::new(SIZE).expect("640x360 is even");
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
        assert_eq!(pictures.leftover(), 0);
    }

    /// Each picture comes out whole and in order, whatever the read boundaries.
    #[test]
    fn a_picture_is_not_sheared_by_the_reads_that_carried_it() {
        let mut pictures = Pictures::new(SIZE).expect("640x360 is even");
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

    /// A padded row stride shows as leftover bytes.
    #[test]
    fn a_stream_that_does_not_divide_is_reported() {
        let mut pictures = Pictures::new(SIZE).expect("640x360 is even");

        // Two pictures with a 704-byte row stride at a 640 pixel width.
        let padded = (704 * HEIGHT) as usize * 3 / 2;
        let stream = vec![0u8; padded * 2];
        let taken = split(&mut pictures, &stream, READ_CHUNK);

        assert_eq!(taken.len(), stream.len() / FRAME);
        assert_eq!(pictures.leftover(), stream.len() % FRAME);
    }

    #[test]
    fn an_odd_geometry_is_refused() {
        assert!(Pictures::new(Size::new(641, 360)).is_err());
        assert!(Pictures::new(Size::new(640, 361)).is_err());
        assert!(Pictures::new(Size::new(0, 360)).is_err());
    }

    /// The command line of a 640x360 capture at 30 fps writing H.264.
    fn h264(bitrate: u32, keyframe_interval: u32) -> String {
        let output = Output::H264 {
            bitrate,
            keyframe_interval,
        };
        args(SIZE, 30, output).join(" ")
    }

    /// The raw width rounds up to a multiple of 64 and the height to even.
    #[test]
    fn raw_capture_rounds_up_to_an_unpadded_geometry() {
        let width = |width, height| aligned(Size::new(width, height)).width;
        assert_eq!(width(640, 360), 640);
        assert_eq!(width(1280, 720), 1280);
        assert_eq!(width(854, 480), 896);
        assert_eq!(width(700, 360), 704);
        assert_eq!(width(96, 64), 128);
        assert_eq!(aligned(Size::new(640, 361)).height, 362);
        assert_eq!(width(0, 0), RAW_WIDTH_ALIGN);
    }

    #[test]
    fn the_output_picks_the_codec_flag() {
        let h264 = h264(DEFAULT_BITRATE, 30);
        assert!(h264.contains("--codec h264"), "{h264}");
        assert!(h264.contains("--bitrate 500000"), "{h264}");
        assert!(h264.contains("--intra 30"), "{h264}");

        let raw = args(SIZE, 30, Output::I420).join(" ");
        assert!(raw.contains("--codec yuv420"), "{raw}");
        assert!(!raw.contains("--bitrate"), "{raw}");
        assert!(!raw.contains("--intra"), "{raw}");
    }

    /// The bitrate and keyframe interval reach the command line.
    #[test]
    fn the_encoder_settings_reach_the_command_line() {
        let args = h264(2_000_000, 60);
        assert!(args.contains("--bitrate 2000000"), "{args}");
        assert!(args.contains("--intra 60"), "{args}");
    }
}
