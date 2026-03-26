//! Minimal subscribe example from the README.
//!
//! Connects to a publisher using a ticket, decodes video,
//! and prints each frame's dimensions.

use iroh_live::{Live, media::AudioBackend, ticket::LiveTicket};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let ticket: LiveTicket = std::env::args()
        .nth(1)
        .expect("usage: readme_subscribe <TICKET>")
        .parse()?;

    let live = Live::from_env().await?.spawn();
    let sub = live
        .subscribe(ticket.endpoint, &ticket.broadcast_name)
        .await?;

    let audio = AudioBackend::default();
    let tracks = sub.media(&audio, Default::default()).await?;

    if let Some(mut video) = tracks.video {
        while let Some(frame) = video.next_frame().await {
            println!("frame {}x{}", frame.dimensions[0], frame.dimensions[1]);
        }
    }

    live.shutdown().await;
    Ok(())
}
