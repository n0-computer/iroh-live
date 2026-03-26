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
use tracing::info;

use crate::args::ImportFormat;

/// Opens the input source (file, stdin, or transcode pipe).
pub async fn open_input(
    file: &Option<PathBuf>,
    transcode: bool,
    format: ImportFormat,
) -> anyhow::Result<Pin<Box<dyn AsyncRead + Send + 'static>>> {
    match (file, transcode) {
        (Some(path), true) => Ok(Box::pin(transcode_file(path.clone(), format).await?)),
        (Some(path), false) => Ok(Box::pin(tokio::fs::File::open(path).await?)),
        (None, false) => Ok(Box::pin(tokio::io::stdin())),
        (None, true) => anyhow::bail!("transcoding stdin is not supported"),
    }
}

/// Reads the file header and publishes the initial catalog to the producer.
/// After this returns, consumers can subscribe to the catalog.
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
    while !decoder.is_initialized() && input.read_buf(&mut buffer).await? > 0 {
        decoder.decode_stream(&mut buffer)?;
    }
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
        info!("input is h264, copy video");
        cmd.args(["-c:v", "copy"]);
    } else {
        info!("input is not h264, transcode");
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
            cmd.args([
                "-a",
                "n",
                "-bsf:v",
                "h264_mp4toannexb",
                "-f",
                "h264",
                "-movflags",
                "cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame",
                "-f",
                "mp4",
            ]);
        }
    }
    cmd.arg("-");

    let mut child = cmd.stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().unwrap();
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
        .await?;
    Ok(String::from_utf8_lossy(&out.stdout).trim() == "h264")
}
