//! Publishes the default camera and microphone, and prints a ticket.
//!
//! Run it with `cargo run -p iroh-live --example publish` and watch with
//! `irl watch TICKET`. Without a microphone it publishes video only.

use clap::Parser;
use iroh::protocol::Router;
use iroh_live::{
    EndpointConfig, Node,
    layout::LiveLayout,
    media::{
        AudioEncoding, AudioSource, LocalBroadcast, MicrophoneConfig, VideoEncoding,
        VideoRendition, VideoSource, video,
    },
};
use n0_error::StdResultExt;
use tracing::{info, warn};

/// Command-line arguments of the example.
#[derive(Parser)]
#[command(about = "Publishes the default camera and microphone over iroh-live")]
struct Args {
    /// The broadcast name.
    #[clap(long, default_value = "demo")]
    name: String,

    /// Encode height in pixels. The camera snaps to its nearest mode.
    #[clap(long, default_value_t = 360)]
    height: u32,

    /// Publish a second, smaller rendition so a subscriber can adapt down.
    #[clap(long)]
    simulcast: bool,
}

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // Set `IROH_SECRET` to keep the same ticket across restarts.
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let layout = LiveLayout::default();
    let node = Node::new(endpoint.clone(), layout.config(&[]));
    // Accepts the viewers. Dropping the router stops accepting.
    let router = node.mount(Router::builder(endpoint)).spawn();
    info!(id = %node.id(), "endpoint ready");

    // Published while `broadcast` lives, or until the node shuts down.
    let broadcast: LocalBroadcast = layout.publish(&node, &args.name)?;

    let mut capture = video::capture::Config::default();
    capture.height = Some(args.height);
    let source = VideoSource::capture(capture).await?;
    let encoding = if args.simulcast {
        VideoEncoding::ladder([
            VideoRendition::new("high"),
            VideoRendition {
                size: Some(video::Size::new(320, 180)),
                ..VideoRendition::new("low")
            },
        ])
    } else {
        VideoEncoding::single(VideoRendition::new("video"))
    };
    broadcast.set_video(source, encoding)?;

    match AudioSource::microphone(MicrophoneConfig::default()).await {
        Ok(microphone) => broadcast.set_audio(microphone, AudioEncoding::voice())?,
        Err(err) => warn!(error = %err, "no microphone, publishing video only"),
    }

    println!("{}", layout.ticket(node.id(), &args.name)?);
    info!(name = %args.name, "publishing");

    tokio::signal::ctrl_c().await?;
    node.shutdown().await;
    // Also closes the endpoint.
    router
        .shutdown()
        .await
        .std_context("the router failed to shut down")?;
    Ok(())
}
