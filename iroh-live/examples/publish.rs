use clap::Parser;
use iroh::{Endpoint, SecretKey};
use iroh_live::{
    Live,
    media::{
        AudioBackend,
        capture::CameraCapturer,
        codec::{AudioCodec, VideoCodec},
        format::{AudioPreset, VideoPreset},
        publish::{LocalBroadcast, VideoInput},
        test_util::TestVideoSource,
    },
    ticket::LiveTicket,
};

mod common;

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // Setup iroh and iroh-live.
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key_from_env()?)
        .bind()
        .await?;
    let live = Live::builder(endpoint).spawn_with_router();

    // Create a publish broadcast.
    let broadcast = LocalBroadcast::new();

    // Capture audio, and encode with the cli-provided preset.
    // AudioBackend must outlive the InputStream it creates — keep it alive
    // for the duration of the program.
    let audio_ctx = AudioBackend::default();
    if !cli.no_audio && !cli.test_source {
        let mic = audio_ctx.default_input().await?;
        broadcast
            .audio()
            .set(mic, AudioCodec::Opus, [cli.audio_preset])?;
    }

    // Capture video: either test source or camera.
    if !cli.no_video {
        if cli.test_source {
            let source = TestVideoSource::new(640, 480);
            broadcast
                .video()
                .set(VideoInput::new(source, cli.codec, cli.video_presets))?;
        } else {
            let camera = CameraCapturer::new()?;
            broadcast
                .video()
                .set(VideoInput::new(camera, cli.codec, cli.video_presets))?;
        }
    }

    // Publish under the given name (default "hello").
    let name = cli.name.as_deref().unwrap_or("hello");
    live.publish(name, &broadcast).await?;

    // Additionally push to relay if specified.
    if let Some(ref relay_addr) = cli.relay {
        let id: iroh::EndpointId = relay_addr.parse().map_err(|e| anyhow::anyhow!("{e}"))?;
        let session = live.transport().connect(id).await?;
        session.publish(name, broadcast.producer().consume());
        tracing::info!(%relay_addr, "published to relay");
    }

    // Create a ticket with the full endpoint address (includes direct
    // addresses for faster P2P connection, not just the node ID).
    let ticket = LiveTicket::new(live.endpoint().addr(), name);
    let ticket_str = ticket.to_string();
    println!("publishing at {ticket_str}");

    // Print a scannable QR code in the terminal.
    if !cli.no_qr {
        print_qr(&ticket_str);
    }

    // Wait for ctrl-c and then shutdown.
    tokio::signal::ctrl_c().await?;
    live.shutdown().await;

    Ok(())
}

/// Renders a QR code to the terminal using Unicode block characters.
fn print_qr(data: &str) {
    if let Err(e) = qr2term::print_qr(data) {
        tracing::warn!("could not print QR code: {e}");
    }
}

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long, default_value_t=VideoCodec::best_available().expect("no video codec available"), value_parser = clap_enum_variants!(VideoCodec))]
    codec: VideoCodec,
    #[arg(long, value_delimiter=',', default_values_t=[VideoPreset::P180, VideoPreset::P360, VideoPreset::P720, VideoPreset::P1080])]
    video_presets: Vec<VideoPreset>,
    #[arg(long, default_value_t=AudioPreset::Hq)]
    audio_preset: AudioPreset,
    #[arg(long)]
    no_video: bool,
    #[arg(long)]
    no_audio: bool,
    /// Use test video source instead of camera (no hardware needed).
    #[arg(long)]
    test_source: bool,
    /// Relay's iroh endpoint address (additionally publishes to relay).
    #[arg(long)]
    relay: Option<String>,
    /// Broadcast name (default: "hello").
    #[arg(long)]
    name: Option<String>,
    /// Suppress QR code display in terminal.
    #[arg(long)]
    no_qr: bool,
}

fn secret_key_from_env() -> n0_error::Result<SecretKey> {
    iroh_live::util::secret_key_from_env()
}
