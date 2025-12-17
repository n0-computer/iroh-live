use std::{path::PathBuf, pin::Pin};

use clap::Parser;
use iroh_live::{LiveNode, rooms::RoomTicket};
use moq_lite::BroadcastProducer;
use n0_error::Result;
use tokio::io::AsyncRead;
use tracing::warn;

mod common;
use self::common::import::{Import, ImportType, transcode};

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
