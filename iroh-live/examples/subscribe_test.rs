//! Test helper: connects to a relay, subscribes to a broadcast,
//! receives N video frames, and exits 0 on success.

use clap::Parser;
use iroh::Endpoint;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .bind()
        .await?;

    let live = iroh_live::Live::builder(endpoint).spawn_with_router();

    let id: iroh::EndpointId = cli.relay.parse().map_err(|e| anyhow::anyhow!("{e}"))?;

    tracing::info!(%cli.relay, %cli.name, frames = cli.frames, "subscribing");

    let (_session, broadcast) = live.subscribe(id, &cli.name).await?;

    tracing::info!("subscribed, waiting for video");
    let mut track = broadcast.video_ready().await?;

    let mut received = 0u32;
    while received < cli.frames {
        match tokio::time::timeout(std::time::Duration::from_secs(10), track.next_frame()).await {
            Ok(Some(frame)) => {
                received += 1;
                tracing::info!(
                    received,
                    width = frame.width(),
                    height = frame.height(),
                    "got frame"
                );
            }
            Ok(None) => {
                anyhow::bail!(
                    "video track ended after {received} frames (expected {})",
                    cli.frames
                );
            }
            Err(_) => {
                anyhow::bail!("timeout waiting for frame {received}");
            }
        }
    }

    tracing::info!(received, "all frames received, exiting");
    live.shutdown().await;
    Ok(())
}

#[derive(Parser)]
struct Cli {
    /// Relay's iroh endpoint ID.
    #[arg(long)]
    relay: String,

    /// Broadcast name to subscribe to.
    #[arg(long)]
    name: String,

    /// Number of video frames to receive before exiting.
    #[arg(long, default_value_t = 3)]
    frames: u32,
}
