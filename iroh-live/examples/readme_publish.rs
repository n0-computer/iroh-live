//! Minimal publish example from the README.
//!
//! Captures camera and microphone, encodes with H.264 and Opus,
//! publishes a broadcast, and prints a ticket for subscribers.

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
