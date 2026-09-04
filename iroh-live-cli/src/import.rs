//! File import: reads media files (mp4, h264 streams, stdin) into a broadcast
//! producer, with optional ffmpeg transcoding.

use std::{
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};

use bytes::{Bytes, BytesMut};
use moq_lite::broadcast::Producer as BroadcastProducer;
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::{info, warn};

use crate::args::ImportFormat;

/// Opens the input source (file, stdin, or transcode pipe).
pub async fn open_input(
    file: &Option<PathBuf>,
    transcode: bool,
    format: ImportFormat,
) -> anyhow::Result<Pin<Box<dyn AsyncRead + Send + 'static>>> {
    match (file, transcode) {
        (Some(path), true) => {
            let stream = transcode_file(path.clone(), format).await?;
            let stream: Pin<Box<dyn AsyncRead + Send + 'static>> = Box::pin(stream);
            Ok(stream)
        }
        (Some(path), false) => {
            let path = path.clone();
            let file = tokio::fs::File::open(&path)
                .await
                .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", path.display()))?;
            let file: Pin<Box<dyn AsyncRead + Send + 'static>> = Box::pin(file);
            Ok(file)
        }
        (None, false) => {
            let stream = tokio::io::stdin();
            let stream: Pin<Box<dyn AsyncRead + Send + 'static>> = Box::pin(stream);
            Ok(stream)
        }
        (None, true) => anyhow::bail!("transcoding stdin is not supported"),
    }
}

/// A byte-stream importer for one of the supported input formats.
///
/// moq-mux 0.9 split importers by multiplicity: a container that may publish
/// several tracks (`fmp4`) versus a single codec on one track (`avc3`). They no
/// longer share a type, so this enum keeps one `decode`/`finish` surface for the
/// CLI, which only cares about feeding bytes.
pub enum Importer {
    /// A container that demuxes its own tracks (fMP4).
    Container(moq_mux::import::ContainerStream),
    /// A single codec on one reserved track (raw Annex-B H.264).
    ///
    /// Boxed: it is two orders of magnitude larger than the container variant.
    Track(Box<moq_mux::import::TrackStream>),
}

impl Importer {
    fn decode(&mut self, data: &[u8]) -> Result<(), moq_mux::Error> {
        match self {
            Self::Container(inner) => inner.decode(data),
            Self::Track(inner) => inner.decode(data),
        }
    }

    fn finish(&mut self) -> Result<(), moq_mux::Error> {
        match self {
            Self::Container(inner) => inner.finish(),
            Self::Track(inner) => inner.finish(),
        }
    }
}

/// Builds the importer and consumes enough input to surface an unusable stream.
///
/// The catalog is no longer published synchronously here: 0.9 resolves it inside
/// the importer, as the container's header or the codec's first keyframe arrives.
/// We still read one chunk so an empty or obviously wrong input fails before the
/// broadcast is announced, which is what the old header loop was really for.
pub async fn init_import(
    broadcast: &mut BroadcastProducer,
    format: ImportFormat,
    input: &mut Pin<Box<dyn AsyncRead + Send + 'static>>,
) -> anyhow::Result<Importer> {
    let catalog = moq_mux::catalog::Producer::new(broadcast).unwrap();

    let mut importer = match format {
        ImportFormat::Fmp4 => Importer::Container(moq_mux::import::ContainerStream::new(
            broadcast.clone(),
            catalog.reserve(),
            "fmp4",
        )?),
        ImportFormat::Avc3 => {
            let request = broadcast.reserve_track("video")?;
            Importer::Track(Box::new(moq_mux::import::TrackStream::new(
                request,
                catalog.reserve(),
                moq_mux::import::Init::new("avc3", Bytes::new()),
            )?))
        }
    };

    let mut buffer = BytesMut::new();
    let n = input.read_buf(&mut buffer).await?;
    if n == 0 {
        anyhow::bail!("input is empty — expected {format:?} data on stdin or from file");
    }
    importer.decode(&buffer).map_err(|e| {
        anyhow::anyhow!(
            "failed to parse {format:?} input after {n} bytes: {e:#}. \
             If the file is a regular (non-fragmented) MP4, use `--transcode` \
             to re-mux it."
        )
    })?;

    info!(bytes_read = n, "input opened, importing");
    Ok(importer)
}

/// Continues reading media data from `input` until EOF.
pub async fn run_import(
    mut importer: Importer,
    mut input: Pin<Box<dyn AsyncRead + Send + 'static>>,
) -> anyhow::Result<()> {
    let mut buffer = BytesMut::new();
    while input.read_buf(&mut buffer).await? > 0 {
        importer.decode(&buffer)?;
        buffer.clear();
    }
    importer.finish()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// ffmpeg transcode helpers
// ---------------------------------------------------------------------------

/// Spawns an ffmpeg process that reads `input`, re-muxes (or re-encodes) it
/// into the requested format, and writes to stdout.
///
/// Wraps the ffmpeg child process in a [`ChildStdout`] that, when dropped,
/// kills the child via SIGPIPE on the broken pipe. The child is spawned
/// with stderr inherited so ffmpeg errors appear in the terminal.
async fn transcode_file(input: PathBuf, format: ImportFormat) -> anyhow::Result<impl AsyncRead> {
    let copy_video = is_h264(&input).await?;

    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-stream_loop",
        "-1",
        "-re",
        "-i",
    ]);
    cmd.arg(input.as_os_str());

    if copy_video {
        info!("input is h264, copying video stream");
        cmd.args(["-c:v", "copy"]);
    } else {
        info!("input is not h264, transcoding to h264");
        cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    }

    match format {
        ImportFormat::Fmp4 => {
            cmd.args(["-c:a", "libopus", "-b:a", "128k"]);
            cmd.args([
                "-movflags",
                "cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame",
                "-f",
                "mp4",
            ]);
        }
        ImportFormat::Avc3 => {
            // Annex B raw H.264 output: strip audio, apply mp4-to-annexb
            // bitstream filter, output raw h264.
            cmd.args(["-an", "-bsf:v", "h264_mp4toannexb", "-f", "h264"]);
        }
    }
    cmd.arg("-");

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn ffmpeg — is ffmpeg installed? {e}"))?;

    let stdout = child.stdout.take().expect("stdout was piped but is None");

    // Spawn a background task to reap the child when it exits. Without
    // this the child becomes a zombie after stdout closes.
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) if !status.success() => {
                warn!(code = ?status.code(), "ffmpeg exited with non-zero status");
            }
            Err(e) => {
                warn!("failed to wait on ffmpeg child: {e}");
            }
            Ok(_) => {}
        }
    });

    Ok(stdout)
}

async fn is_h264(input: &Path) -> anyhow::Result<bool> {
    let out = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=nokey=1:noprint_wrappers=1",
        ])
        .arg(input.as_os_str())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run ffprobe — is ffmpeg installed? {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim() == "h264")
}
