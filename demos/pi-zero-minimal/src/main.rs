//! Minimal Raspberry Pi publisher: the camera's own hardware H.264.
//!
//! `rpicam-vid` drives the libcamera ISP and the Pi's hardware encoder, so the
//! bytes it writes are published unchanged and the Pi never software-encodes.
//! Prints a ticket for viewers to connect with `irl play <TICKET>`.
//!
//! Build:
//!   cargo build -p pi-zero-minimal --release
//!
//! Cross-compile:
//!   cargo make cross-build-aarch64 -- -p pi-zero-minimal --release

use iroh_live::{Live, media::rpicam, ticket::LiveTicket};

/// The path the camera publishes on. Viewers subscribe to the same one.
const BROADCAST: &str = "pi-cam";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let live = Live::from_env().await?.with_router().spawn();
    let broadcast = live.publish(BROADCAST)?;
    broadcast
        .video()
        .set(rpicam::open(rpicam::Config::new(640, 360, 30))?)?;

    let ticket = LiveTicket::new(live.endpoint().addr(), BROADCAST);
    println!("{ticket}");

    tracing::info!("publishing, press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
