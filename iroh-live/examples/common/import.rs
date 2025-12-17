use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use bytes::BytesMut;
use clap::ValueEnum;
use moq_lite::BroadcastProducer;
use n0_error::Result;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tracing::info;

#[derive(ValueEnum, Debug, Clone, Default, Copy)]
pub enum ImportType {
    #[default]
    Cmaf,
    AnnexB,
}

impl ImportType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImportType::AnnexB => "annex-b",
            ImportType::Cmaf => "cmaf",
        }
    }
}

// Taken from
// https://github.com/moq-dev/moq/blob/30c28b8c3b6bd941fe1279c0fd8855139a1d4f6a/rs/hang-cli/src/import.rs
// License: Apache-2.0
pub struct Import {
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

pub async fn transcode(input: PathBuf, format: ImportType) -> Result<impl AsyncRead> {
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

pub async fn is_h264(input: &Path) -> Result<bool> {
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
