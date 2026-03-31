//! Minimal Raspberry Pi publisher — pre-encoded H.264 from libcamera.
//!
//! Publishes the Pi camera's hardware-encoded H.264 stream directly to
//! the MoQ transport. No software encoding needed. Prints a ticket for
//! viewers to connect with `irl play <TICKET>`.
//!
//! Build:
//!   cargo build -p pi-zero-minimal --release
//!
//! Cross-compile:
//!   cargo make cross-build-aarch64 -- -p pi-zero-minimal --release

use iroh_live::{
    Live,
    media::publish::{LocalBroadcast, VideoInput},
    ticket::LiveTicket,
};
use rusty_capture::{LibcameraH264Config, LibcameraH264Source};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let live = Live::from_env().await?.with_router().spawn();
    let broadcast = LocalBroadcast::new();

    // Pre-encoded H.264 from libcamera. rpicam-vid runs as a subprocess
    // and feeds hardware-encoded NALs straight to the transport — no
    // software encode step, minimal CPU usage on the Pi Zero.
    let config = LibcameraH264Config::new(640, 360, 30);
    let video_config = config.video_config();
    broadcast.video().set(VideoInput::pre_encoded(
        "video/h264-libcamera",
        video_config,
        move || Ok(Box::new(LibcameraH264Source::new(config.clone()))),
    ))?;

    let name = "pi-cam";
    live.publish(name, &broadcast).await?;
    let ticket = LiveTicket::new(live.endpoint().addr(), name);
    println!("{ticket}");

    tracing::info!("publishing, press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
