use std::{path::PathBuf, pin::Pin};

use clap::Parser;
use iroh::EndpointId;
use iroh_live::LiveNode;
use moq_lite::BroadcastProducer;
use n0_error::Result;
use tokio::io::AsyncRead;
use tracing::warn;

mod common;
use self::common::import::{Import, ImportType, transcode};

#[derive(Debug, Parser)]
struct Cli {
    #[clap(short, long)]
    target: EndpointId,
    #[clap(short, long, default_value = "anon/bbb")]
    path: String,

    /// The format of the input media.
    #[clap(long, value_enum, default_value_t = ImportType::Cmaf)]
    format: ImportType,

    /// Input file.
    #[clap(short, long)]
    file: Option<PathBuf>,

    /// Transcode the video with ffmpeg.
    #[clap(long)]
    transcode: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let node = LiveNode::spawn_from_env().await?;
    let session = node.live.connect(cli.target).await?;

    let mut input: Pin<Box<dyn AsyncRead + Send + 'static>> = match (cli.file, cli.transcode) {
        (Some(path), true) => Box::pin(transcode(path.clone(), cli.format).await?),
        (Some(path), false) => Box::pin(tokio::fs::File::open(path).await?),
        (None, false) => Box::pin(tokio::io::stdin()),
        (None, true) => panic!("transcoding stdin is not supported"),
    };

    let broadcast = BroadcastProducer::default();
    session.publish(cli.path, broadcast.consume());

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
    node.shutdown().await?;
    Ok(())
}
