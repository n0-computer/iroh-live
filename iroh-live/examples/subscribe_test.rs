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

    let live = iroh_live::Live::builder(endpoint).with_router().spawn();

    let id: iroh::EndpointId = cli.relay.parse().map_err(|e| anyhow::anyhow!("{e}"))?;

    tracing::info!(%cli.relay, %cli.name, frames = cli.frames, "subscribing");

    // Retry the initial source attach: the publisher may not have
    // announced the catalog yet.
    let sub = live.subscribe(id, &cli.name);
    let active = {
        let mut last_err = None;
        let mut result = None;
        for attempt in 0..5 {
            match sub.ready().await {
                Ok(active) => {
                    result = Some(active);
                    break;
                }
                Err(e) => {
                    tracing::warn!(attempt, %e, "ready failed, retrying in 1s");
                    last_err = Some(format!("{e:#}"));
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
        result.ok_or_else(|| {
            anyhow::anyhow!(
                "subscribe failed after retries: {}",
                last_err.unwrap_or_default()
            )
        })?
    };

    tracing::info!("subscribed, waiting for video");
    let mut track = active.broadcast().video_ready().await?;

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
