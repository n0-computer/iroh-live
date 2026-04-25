//! Combined publish/subscribe demo.
//!
//! Demonstrates the two sides of a broadcast: publishing from a camera
//! and microphone, and subscribing to a remote broadcast by ticket.
//!
//! Usage:
//!   cargo run -p iroh-live --example demo -- publish
//!   cargo run -p iroh-live --example demo -- subscribe <TICKET>

use iroh_live::{
    Live,
    media::{
        AudioBackend,
        capture::CameraCapturer,
        codec::{AudioCodec, VideoCodec},
        format::{AudioPreset, VideoPreset},
    },
    ticket::LiveTicket,
};
use moq_media::publish::LocalBroadcast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();

    match mode.as_str() {
        "publish" => publish_side().await,
        "subscribe" => {
            let ticket: LiveTicket = args
                .next()
                .expect("usage: demo subscribe <TICKET>")
                .parse()?;
            subscribe_side(ticket).await
        }
        other => {
            eprintln!("unknown mode '{other}'. Usage:\n  demo publish\n  demo subscribe <TICKET>");
            std::process::exit(1);
        }
    }
}

async fn publish_side() -> anyhow::Result<()> {
    let live = Live::from_env().await?.with_router().spawn();
    let broadcast = LocalBroadcast::new();

    // Attach camera — encodes a 720p rendition with H.264
    let camera = CameraCapturer::new()?;
    broadcast
        .video()
        .set_source(camera, VideoCodec::H264, [VideoPreset::P720])?;

    // Attach microphone — encodes with Opus
    let audio = AudioBackend::default();
    let mic = audio.default_input().await?;
    broadcast
        .audio()
        .set(mic, AudioCodec::Opus, [AudioPreset::Hq])?;

    // Publish and print a ticket that subscribers can use to connect
    live.publish("hello", &broadcast).await?;
    let ticket = LiveTicket::new(live.endpoint().addr(), "hello");
    println!("{ticket}");

    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}

async fn subscribe_side(ticket: LiveTicket) -> anyhow::Result<()> {
    let live = Live::from_env().await?.spawn();
    let sub = live.subscribe(ticket.endpoint, &ticket.broadcast_name);
    let audio = AudioBackend::default();
    let tracks = sub.media(&audio, Default::default()).await?;

    if let Some(video) = tracks.video {
        while let Some(frame) = video.next_frame().await {
            println!("frame {}x{}", frame.dimensions[0], frame.dimensions[1]);
        }
    }

    live.shutdown().await;
    Ok(())
}
