//! Publishes an MP4 file in real time, as it is or re-encoded, and prints a ticket.
//!
//! `cargo run -p iroh-live --features file --example publish-file -- clip.mp4`
//! publishes the file's H.264 and AAC as they are: no CPU, the original
//! quality, but one rendition with the file's keyframes and bitrate. With
//! `--reencode` it decodes the file and encodes a 720p and a 360p rendition,
//! like a camera. `--loop` starts the file over at its end. Watch with
//! `irl watch TICKET`.

use std::path::PathBuf;

use clap::Parser;
use iroh::protocol::Router;
use iroh_live::{
    EndpointConfig, Node,
    layout::LiveLayout,
    media::{
        FileEnd, LocalBroadcast, MediaFile, VideoEncoding, VideoFileConfig, VideoRendition,
        audio_encoding,
    },
};
use n0_error::StdResultExt;
use tracing::info;

/// Command-line arguments of the example.
#[derive(Parser)]
#[command(about = "Publishes an MP4 file over iroh-live")]
struct Args {
    /// The MP4 file, plain or fragmented.
    path: PathBuf,

    /// Decode the file and encode it into two renditions.
    #[clap(long)]
    reencode: bool,

    /// Start the file over at its end.
    #[clap(long = "loop")]
    looping: bool,
}

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let layout = LiveLayout::default();
    let node = Node::new(endpoint.clone(), layout.config());
    let router = node.mount(Router::builder(endpoint)).spawn();
    let broadcast: LocalBroadcast = layout.publish(&node, "file")?;

    let end = if args.looping {
        FileEnd::Restart
    } else {
        FileEnd::Stop
    };
    // Reads the file's index; its streams share one clock and loop together.
    let file = MediaFile::open(&args.path).await?.with_end(end);
    info!(info = ?file.info(), "file opened");
    let has_audio = file.info().audio.is_some();
    if args.reencode {
        let video = file.video(VideoFileConfig::default()).await?;
        let ladder = VideoEncoding::ladder([VideoRendition::p720(), VideoRendition::p360()]);
        broadcast.set_video(video, ladder)?;
        if has_audio {
            broadcast.set_audio(file.audio()?, audio_encoding::music())?;
        }
    } else {
        broadcast.set_encoded_video(file.encoded_video()?)?;
        if has_audio {
            broadcast.set_encoded_audio(file.encoded_audio()?)?;
        }
    }

    println!("{}", layout.ticket(node.id(), "file")?);
    tokio::signal::ctrl_c().await?;
    router
        .shutdown()
        .await
        .std_context("the router failed to shut down")?;
    Ok(())
}
