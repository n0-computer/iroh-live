//! Publishes a camera and a microphone.
//!
//! Captures the default camera and, when one is available, the default
//! microphone, publishes both over iroh, and prints a ticket.
//!
//! Watch it with `irl watch TICKET`.

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

    // A stable identity from `IROH_SECRET`, so the ticket survives a restart.
    let mut options = EndpointOptions::default();
    if let Ok(key) = std::env::var("IROH_SECRET") {
        options = options.with_secret_key(key.parse()?);
    }
    let live = Live::builder(options.bind().await?).with_router().spawn();
    info!(id = %live.endpoint().id(), "endpoint ready");

    let broadcast = LocalBroadcast::new();

    let mut capture = video::capture::Config::default();
    capture.height = Some(args.height);
    let source = VideoSource::capture(capture).await?;
    let encoding = match args.simulcast {
        false => VideoEncoding::single(VideoRendition::new("video")),
        true => VideoEncoding::ladder([
            VideoRendition::new("high"),
            VideoRendition::new("low").with_size(video::Size::new(320, 180)),
        ]),
    };
    broadcast.set_video(source, encoding)?;

    // A machine with no microphone still publishes video.
    match AudioSource::microphone(MicrophoneConfig::default()).await {
        Ok(microphone) => broadcast.set_audio(microphone, AudioEncoding::voice())?,
        Err(err) => warn!(error = %err, "no microphone, publishing video only"),
    }

    // Held for as long as the broadcast should stay published.
    let _publication = live.publish(&args.name, &broadcast)?;
    println!("{}", live.ticket(&args.name));
    info!(name = %args.name, "publishing");

    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
