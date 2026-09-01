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

    // Retry subscribe — the publisher may not have announced the catalog yet.
    let _sub = {
        let mut last_err = String::new();
        let mut result = None;
        for attempt in 0..5 {
            match live.subscribe(id, &cli.name).await {
                Ok(r) => {
                    result = Some(r);
                    break;
                }
                Err(e) => {
                    tracing::warn!(attempt, %e, "subscribe failed, retrying in 1s");
                    last_err = format!("{e:#}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
        result.ok_or_else(|| anyhow::anyhow!("subscribe failed after retries: {last_err}"))?
    };

    tracing::info!("subscribed, waiting for video");
    let track = _sub
        .broadcast()
        .video()
        .await
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;

    let mut received = 0u32;
    while received < cli.frames {
        match tokio::time::timeout(std::time::Duration::from_secs(10), track.recv()).await {
            Ok(Some(frame)) => {
                received += 1;
                let size = frame.size();
                tracing::info!(received, %size, "got frame");
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
