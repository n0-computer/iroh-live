//! Publishes the default camera and microphone, and prints a ticket.
//!
//! Run it with `cargo run -p iroh-live --example publish` and watch with
//! `irl watch TICKET`. Without a microphone it publishes video only.

use clap::Parser;
use iroh_live::{
    EndpointOptions, Live, LocalBroadcast,
    media::{
        AudioEncoding, AudioSource, MicrophoneConfig, VideoEncoding, VideoRendition, VideoSource,
        video,
    },
};
use tracing::{info, warn};

#[derive(Parser)]
#[command(about = "Publishes the default camera and microphone over iroh-live")]
struct Args {
    /// The path to publish on.
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
    let options = EndpointOptions::from_env()?;
    let live = Live::builder(options.bind().await?).with_router().spawn();
    info!(id = %live.endpoint().id(), "endpoint ready");

    let broadcast = LocalBroadcast::new();

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

    // The broadcast stays published while this is held.
    let _publication = live.publish(&args.name, &broadcast)?;
    println!("{}", live.ticket(&args.name));
    info!(name = %args.name, "publishing");

    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
