//! Subscribes to a broadcast through a relay and exits after N decoded frames.
//!
//! The browser end-to-end tests in `tests/e2e-browser` run it. Build it with
//! `cargo make e2e-prebuild`.

use std::time::Duration;

use clap::Parser;
use iroh::EndpointId;
use iroh_live::{
    EndpointOptions, Live, Mdns,
    media::PlayerConfig,
    moq::{Reach, RelayConfig, RelayOffer},
};
use n0_error::bail_any;
use tracing::info;

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // The relay is the only peer, so skip mDNS.
    let endpoint = EndpointOptions {
        mdns: Mdns::Off,
        ..Default::default()
    }
    .bind()
    .await?;
    let live = Live::builder(endpoint).spawn();

    info!(relay = %cli.relay, name = %cli.name, frames = cli.frames, "subscribing");
    // The link redials, and the subscribe waits until the relay routes the name.
    let _link = live.moq().attach_relay(RelayConfig {
        offer: RelayOffer::Nothing,
        ..RelayConfig::iroh(cli.relay)
    })?;
    let subscription = live.subscribe_path(&cli.name, Reach::Relays).await?;
    let player = live
        .remote_broadcast(&subscription)
        .play(PlayerConfig::default())?;
    let mut frames = player.video();

    info!("subscribed, waiting for video");
    for received in 1..=cli.frames {
        match tokio::time::timeout(Duration::from_secs(10), frames.next()).await {
            Ok(Some(frame)) => info!(received, size = %frame.size(), "got frame"),
            Ok(None) => bail_any!("the video ended after {} frames", received - 1),
            Err(_) => bail_any!("no frame {received} within 10 s"),
        }
    }

    info!("all frames received, exiting");
    live.shutdown().await;
    Ok(())
}

#[derive(Parser)]
struct Cli {
    /// The relay's endpoint id.
    #[arg(long)]
    relay: EndpointId,

    /// The broadcast name on the relay.
    #[arg(long)]
    name: String,

    /// Video frames to receive before exiting.
    #[arg(long, default_value_t = 3)]
    frames: u32,
}
