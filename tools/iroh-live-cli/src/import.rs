//! File import: reads media files (mp4, h264 streams, stdin) into a broadcast
//! producer, with optional ffmpeg transcoding.

use std::{
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};

use bytes::BytesMut;
use moq_lite::BroadcastProducer;
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
        (Some(path), false) => Ok(Box::pin(tokio::fs::File::open(path).await?)),
        (None, false) => Ok(Box::pin(tokio::io::stdin())),
        (None, true) => anyhow::bail!("transcoding stdin is not supported"),
    }
}

/// Two-phase import handle. Call [`init`] to read the file header and publish
/// the catalog, then [`run`] to stream the remaining data.
///
/// Splitting into phases allows creating a preview subscription between init
/// (catalog available) and run (media flowing).
pub struct ImportHandle {
    import: Import,
    input: Pin<Box<dyn AsyncRead + Send + 'static>>,
}

impl ImportHandle {
    /// Creates the import and reads the file header, publishing the catalog
    /// to the broadcast producer. After this returns, a consumer of the
    /// producer can subscribe to the catalog and media tracks.
    pub async fn init(
        broadcast: BroadcastProducer,
        format: ImportFormat,
        mut input: Pin<Box<dyn AsyncRead + Send + 'static>>,
    ) -> anyhow::Result<Self> {
        let mut import = Import::new(broadcast, format);
        import.init_from(&mut input).await?;
        Ok(Self { import, input })
    }

    /// Continues reading media data until EOF or cancellation.
    pub async fn run(mut self) -> anyhow::Result<()> {
        let read = async {
            self.import.read_from(&mut self.input).await?;
            anyhow::Ok(())
        };
        tokio::pin!(read);

        tokio::select! {
            res = &mut read => {
                if let Err(err) = res {
                    warn!("import failed: {err:#}");
                }
            }
            _ = tokio::signal::ctrl_c() => {}
        }
        Ok(())
    }
}

/// One-shot import: init + run in a single call. Use [`ImportHandle`] when
/// you need to create a preview between init and run.
pub async fn run_import(
    broadcast: BroadcastProducer,
    format: ImportFormat,
    input: &mut Pin<Box<dyn AsyncRead + Send + 'static>>,
) -> anyhow::Result<()> {
    let import = async move {
        let mut imp = Import::new(broadcast, format);
        imp.init_from(input).await?;
        imp.read_from(input).await?;
        anyhow::Ok(())
    };
    tokio::pin!(import);

    tokio::select! {
        res = &mut import => {
            if let Err(err) = res {
                warn!("import failed: {err:#}");
            }
        }
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Import decoder (ported from examples/common/import.rs)
// ---------------------------------------------------------------------------

struct Import {
    decoder: moq_mux::import::StreamDecoder,
    buffer: BytesMut,
}

impl Import {
    fn new(mut broadcast: BroadcastProducer, format: ImportFormat) -> Self {
        use moq_mux::import::StreamFormat;
        let catalog = moq_mux::CatalogProducer::new(&mut broadcast).unwrap();
        let stream_format = match format {
            ImportFormat::Fmp4 => StreamFormat::Fmp4,
            ImportFormat::Avc3 => StreamFormat::Avc3,
        };
        let decoder = moq_mux::import::StreamDecoder::new(broadcast, catalog, stream_format);
        Self {
            decoder,
            buffer: BytesMut::new(),
        }
    }

    async fn init_from<T: AsyncRead + Unpin>(&mut self, input: &mut T) -> anyhow::Result<()> {
        while !self.decoder.is_initialized() && input.read_buf(&mut self.buffer).await? > 0 {
            self.decoder.decode_stream(&mut self.buffer)?;
        }
        Ok(())
    }

    async fn read_from<T: AsyncRead + Unpin>(&mut self, input: &mut T) -> anyhow::Result<()> {
        while input.read_buf(&mut self.buffer).await? > 0 {
            self.decoder.decode_stream(&mut self.buffer)?;
        }
        self.decoder.finish()
    }
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
