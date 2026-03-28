//! File import: reads media files (mp4, h264 streams, stdin) into a broadcast
//! producer, with optional ffmpeg transcoding.

use std::{
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};

use bytes::BytesMut;
use moq_lite::BroadcastProducer;
use moq_mux::import::StreamFormat;
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
        (Some(path), true) => Ok(Box::pin(transcode_file(path.clone(), format).await?)),
        (Some(path), false) => {
            let path = path.clone();
            let file = tokio::fs::File::open(&path)
                .await
                .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", path.display()))?;
            Ok(Box::pin(file))
        }
        (None, false) => Ok(Box::pin(tokio::io::stdin())),
        (None, true) => anyhow::bail!("transcoding stdin is not supported"),
    }
}

/// Reads the file header and publishes the initial catalog to the producer.
///
/// For fmp4 input this parses the moov box; for avc3 it reads until SPS/PPS
/// are found. After this returns, consumers can subscribe to the catalog.
pub async fn init_import(
    broadcast: &mut BroadcastProducer,
    format: ImportFormat,
    input: &mut Pin<Box<dyn AsyncRead + Send + 'static>>,
) -> anyhow::Result<moq_mux::import::StreamDecoder> {
    let catalog = moq_mux::CatalogProducer::new(broadcast).unwrap();
    let stream_format = match format {
        ImportFormat::Fmp4 => StreamFormat::Fmp4,
        ImportFormat::Avc3 => StreamFormat::Avc3,
    };
    let mut decoder =
        moq_mux::import::StreamDecoder::new(broadcast.clone(), catalog, stream_format);

    let mut buffer = BytesMut::new();
    let mut total_read = 0usize;
    while !decoder.is_initialized() {
        let n = input.read_buf(&mut buffer).await?;
        if n == 0 {
            if total_read == 0 {
                anyhow::bail!("input is empty — expected {format:?} data on stdin or from file");
            }
            anyhow::bail!(
                "reached end of input ({total_read} bytes) before finding a valid \
                 {format:?} header. The file may not be fragmented MP4 — use \
                 `--transcode` to re-mux with ffmpeg."
            );
        }
        total_read += n;
        decoder.decode_stream(&mut buffer).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse {format:?} header after {total_read} bytes: {e:#}. \
                 If the file is a regular (non-fragmented) MP4, use `--transcode` \
                 to re-mux it."
            )
        })?;
    }

    info!(
        bytes_read = total_read,
        "file header parsed, catalog published"
    );
    Ok(decoder)
}

/// Continues reading media data from `input` until EOF.
pub async fn run_import(
    mut decoder: moq_mux::import::StreamDecoder,
    mut input: Pin<Box<dyn AsyncRead + Send + 'static>>,
) -> anyhow::Result<()> {
    let mut buffer = BytesMut::new();
    while input.read_buf(&mut buffer).await? > 0 {
        decoder.decode_stream(&mut buffer)?;
    }
    decoder.finish()
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
