use clap::Parser;
use iroh::{Endpoint, SecretKey};
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

mod common;

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // Setup audio backend.
    let audio_ctx = AudioBackend::default();

    // Setup iroh and iroh-live.
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key_from_env()?)
        .bind()
        .await?;
    let live = Live::builder(endpoint).spawn_with_router();

    // Create a publish broadcast.
    let broadcast = LocalBroadcast::new();

    // Capture audio, and encode with the cli-provided preset.
    if !cli.no_audio {
        let mic = audio_ctx.default_input().await?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [cli.audio_preset])?;
    }

    // Capture camera, and encode with the cli-provided presets.
    if !cli.no_video {
        let camera = CameraCapturer::new()?;
        broadcast
            .video()
            .set(camera, cli.codec, cli.video_presets)?;
    }

    // Publish under the name "hello".
    let name = "hello";
    live.publish(name, &broadcast).await?;

    // Create a ticket string and print
    let ticket = LiveTicket::new(live.endpoint().id(), name);
    println!("publishing at {ticket}");

    // Wait for ctrl-c and then shutdown.
    tokio::signal::ctrl_c().await?;
    live.shutdown().await;

    Ok(())
}

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long, default_value_t=VideoCodec::best_available(), value_parser = clap_enum_variants!(VideoCodec))]
    codec: VideoCodec,
    #[arg(long, value_delimiter=',', default_values_t=[VideoPreset::P180, VideoPreset::P360, VideoPreset::P720, VideoPreset::P1080])]
    video_presets: Vec<VideoPreset>,
    #[arg(long, default_value_t=AudioPreset::Hq)]
    audio_preset: AudioPreset,
    #[arg(long)]
    no_video: bool,
    #[arg(long)]
    no_audio: bool,
}

fn secret_key_from_env() -> n0_error::Result<SecretKey> {
    Ok(match std::env::var("IROH_SECRET") {
        Ok(key) => key.parse()?,
        Err(_) => {
            let key = SecretKey::generate(&mut rand::rng());
            println!(
                "Created new secret. Reuse with IROH_SECRET={}",
                data_encoding::HEXLOWER.encode(&key.to_bytes())
            );
            key
        }
    })
}
