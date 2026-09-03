//! Publishes a camera and a microphone.
//!
//! Captures the default camera and, when one is available, the default
//! microphone, publishes both over iroh, and prints a ticket.
//!
//! Watch it with `irl play TICKET`.

use clap::Parser;
use iroh_live::{
    Live,
    media::{
        audio,
        publish::{VideoRendition, VideoSource},
        video,
    },
    ticket::LiveTicket,
};
use tracing::info;

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

    let live = Live::from_env().await?.with_router().spawn();
    info!(id = %live.endpoint().id(), "endpoint ready");

    let broadcast = live.publish(&args.name)?;

    let mut capture = video::capture::Config::default();
    capture.height = Some(args.height);
    let source = VideoSource::Capture(capture);
    match args.simulcast {
        false => broadcast.video().set(source)?,
        true => broadcast.video().set_renditions(
            source,
            vec![
                VideoRendition::new("high"),
                VideoRendition::new("low").with_size(video::Size::new(320, 180)),
            ],
        )?,
    }

    // A machine with no microphone still publishes video: the device opens
    // inside the publish task, which logs and ends the audio track on failure.
    broadcast.audio().set(audio::capture::Config::default());

    let ticket = LiveTicket::new(live.endpoint().id(), &args.name);
    println!("{ticket}");
    info!(name = %args.name, "publishing");

    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
