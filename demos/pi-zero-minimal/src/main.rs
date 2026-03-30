//! Minimal camera + mic publisher for Raspberry Pi (or any Linux/macOS host).
//!
//! Captures from the first available camera, optionally captures audio from
//! the default microphone, publishes over iroh, and prints the connection
//! ticket. No e-paper, no TUI, no rendering dependencies.

use clap::Parser;
use iroh_live::{
    Live,
    media::{
        AudioBackend,
        capture::CameraCapturer,
        codec::{AudioCodec, VideoCodec},
        format::{AudioPreset, VideoPreset},
        publish::LocalBroadcast,
    },
    ticket::LiveTicket,
};
use tracing::{info, warn};

#[derive(Parser)]
#[command(about = "Minimal camera + mic publisher over iroh")]
struct Args {
    /// Broadcast name.
    #[clap(long, default_value = "pi-cam")]
    name: String,

    /// Video resolution preset.
    #[clap(long, default_value = "360p")]
    video_preset: VideoPreset,

    /// Video codec.
    #[clap(long, default_value = "h264")]
    video_codec: VideoCodec,
}

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // --- iroh endpoint (reads IROH_SECRET if set, otherwise generates a key) ---
    let live = Live::from_env().await?.with_router().spawn();
    info!(id = %live.endpoint().id(), "endpoint ready");

    // --- broadcast ---
    let broadcast = LocalBroadcast::new();

    // Video: first available camera.
    let camera = CameraCapturer::new()?;
    broadcast
        .video()
        .set_source(camera, args.video_codec, vec![args.video_preset])?;
    info!(preset = %args.video_preset, codec = %args.video_codec, "video source set");

    // Audio: default mic, best-effort.
    let audio_ctx = AudioBackend::default();
    match audio_ctx.default_input().await {
        Ok(mic) => {
            broadcast
                .audio()
                .set(mic, AudioCodec::Opus, [AudioPreset::Lq])?;
            info!("audio source set (default mic)");
        }
        Err(e) => {
            warn!(error = %e, "no microphone available, publishing video only");
        }
    }

    // --- publish and print ticket ---
    live.publish(&args.name, &broadcast).await?;
    let ticket = LiveTicket::new(live.endpoint().addr(), &args.name);
    println!("{ticket}");
    info!(name = %args.name, "publishing");

    // --- wait for shutdown ---
    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
