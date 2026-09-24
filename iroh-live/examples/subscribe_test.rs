//! Test helper: connects to a relay, subscribes to a broadcast,
//! receives N video frames, and exits 0 on success.

use clap::Parser;
use iroh::Endpoint;
use iroh_live::moq::{ConnectOptions, Grant};

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

    // The relay names broadcasts as its publishers did, so the name is resolved
    // on the session with the relay rather than through the route table, and
    // the relay may publish anything on it. Retried: the publisher may not have
    // announced the catalog yet.
    let broadcast = {
        let mut last_err = String::new();
        let mut result = None;
        for attempt in 0..5 {
            let attempt_result = async {
                let trusted = ConnectOptions::default().with_grant(Grant::everything());
                let session = live.moq().connect_with(id, trusted).await?;
                let subscription = session.subscribe(cli.name.as_str()).await?;
                Ok::<_, iroh_live::moq::Error>(live.remote_broadcast(&subscription))
            }
            .await;
            match attempt_result {
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
    let player = broadcast
        .play(iroh_live::PlayerConfig::default())
        .map_err(|err| anyhow::anyhow!("{err:#}"))?;
    let mut frames = player.video();

    let mut received = 0u32;
    while received < cli.frames {
        match tokio::time::timeout(std::time::Duration::from_secs(10), frames.next()).await {
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
