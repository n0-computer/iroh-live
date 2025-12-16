use std::{
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};

use bytes::BytesMut;
use clap::{Parser, ValueEnum};
use iroh_live::{LiveNode, rooms::RoomTicket};
use moq_lite::BroadcastProducer;
use n0_error::Result;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tracing::{info, warn};

#[derive(Debug, Parser)]
struct Cli {
    /// Room to join. If empty a new room will be created.
    /// Will also be read from the IROH_LIVE_ROOM environment variable.
    #[clap(short, long)]
    room: Option<RoomTicket>,

    /// The format of the input media.
    #[clap(long, value_enum, default_value_t = ImportType::Cmaf)]
    format: ImportType,

    /// Input file. If empty reads from stdin.
    file: Option<PathBuf>,

    /// Transcode the video with ffmpeg.
    #[clap(long)]
    transcode: bool,
}

#[derive(ValueEnum, Debug, Clone, Default, Copy)]
enum ImportType {
    #[default]
    Cmaf,
    AnnexB,
}

impl ImportType {
    fn as_str(&self) -> &'static str {
        match self {
            ImportType::AnnexB => "annex-b",
            ImportType::Cmaf => "cmaf",
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let ticket = match cli.room {
        Some(ticket) => ticket,
        None => RoomTicket::new_from_env()?,
    };

    let node = LiveNode::spawn_from_env().await?;
    let room = node.join_room(ticket).await?;

    let mut input: Pin<Box<dyn AsyncRead + Send + 'static>> = match (cli.file, cli.transcode) {
        (Some(path), true) => Box::pin(transcode(path.clone(), cli.format).await?),
        (Some(path), false) => Box::pin(tokio::fs::File::open(path).await?),
        (None, false) => Box::pin(tokio::io::stdin()),
        (None, true) => panic!("transcoding stdin is not supported"),
    };

    let broadcast = BroadcastProducer::default();
    room.publish("file", broadcast.clone()).await?;

    let import = async move {
        let mut import = Import::new(broadcast.into(), cli.format);

        import.init_from(&mut input).await?;
        import.read_from(&mut input).await?;
        n0_error::Ok(())
    };
    tokio::pin!(import);

    tokio::select! {
        res = &mut import => {
            if let Err(err) = res {
                warn!("Import failed: {err:#}");
            }
        }
        _ = tokio::signal::ctrl_c() => {}
    };
    drop(import);
    drop(room);
    node.shutdown().await?;
    Ok(())
}

// Taken from
// https://github.com/moq-dev/moq/blob/30c28b8c3b6bd941fe1279c0fd8855139a1d4f6a/rs/hang-cli/src/import.rs
// License: Apache-2.0
struct Import {
    decoder: hang::import::Decoder,
    buffer: BytesMut,
}

impl Import {
    pub fn new(broadcast: BroadcastProducer, format: ImportType) -> Self {
        let decoder = hang::import::Decoder::new(broadcast.into(), format.as_str())
            .expect("supported format");
        Self {
            decoder,
            buffer: BytesMut::new(),
        }
    }
}

impl Import {
    pub async fn init_from<T: AsyncRead + Unpin>(&mut self, input: &mut T) -> anyhow::Result<()> {
        while !self.decoder.is_initialized() && input.read_buf(&mut self.buffer).await? > 0 {
            self.decoder.decode_stream(&mut self.buffer)?;
        }

        Ok(())
    }

    pub async fn read_from<T: AsyncRead + Unpin>(&mut self, input: &mut T) -> anyhow::Result<()> {
        while input.read_buf(&mut self.buffer).await? > 0 {
            self.decoder.decode_stream(&mut self.buffer)?;
        }

        // Flush the final frame.
        self.decoder.decode_frame(&mut self.buffer, None)
    }
}

async fn transcode(input: PathBuf, format: ImportType) -> Result<impl AsyncRead> {
    let copy_video = is_h264(&input).await?;

    let mut cmd = Command::new("ffmpeg");
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
        ImportType::Cmaf => {
            cmd.args(["-c:a", "libopus", "-b:a", "128k"]);
            cmd.args([
                "-movflags",
                "cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame",
                "-f",
                "mp4",
            ]);
        }
        ImportType::AnnexB => {
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

async fn is_h264(input: &Path) -> Result<bool> {
    let out = Command::new("ffprobe")
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
