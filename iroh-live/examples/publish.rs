use clap::Parser;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_live::{
    Live,
    media::{
        audio::AudioBackend,
        av::{AudioPreset, VideoCodec, VideoEncoderKind, VideoPreset},
        capture::CameraCapturer,
        codec::OpusEncoder,
        publish::{AudioRenditions, PublishBroadcast, VideoRenditions},
    },
    ticket::LiveTicket,
};
use n0_error::StdResultExt;

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // Setup audio backend.
    let audio_ctx = AudioBackend::new(None, None);

    // Setup iroh and iroh-live.
    let endpoint = Endpoint::builder()
        .secret_key(secret_key_from_env()?)
        .bind()
        .await?;
    let live = Live::new(endpoint.clone());
    let router = Router::builder(endpoint)
        .accept(iroh_live::ALPN, live.moq.protocol_handler())
        .spawn();

    // Create a publish broadcast.
    let mut broadcast = PublishBroadcast::new();

    // Capture audio, and encode with the cli-provided preset.
    if !cli.no_audio {
        let mic = audio_ctx.default_input().await?;
        let audio = AudioRenditions::new::<OpusEncoder>(mic, [cli.audio_preset]);
        broadcast.set_audio(Some(audio))?;
    }

    // Capture camera, and encode with the cli-provided presets.
    if !cli.no_video {
        let camera = CameraCapturer::new()?;
        let codec = VideoEncoderKind::try_from(cli.codec)?;
        let video = VideoRenditions::new_for_codec(codec, camera, cli.video_presets);
        broadcast.set_video(Some(video))?;
    }

    // Publish under the name "hello".
    let name = "hello";
    live.publish(name, broadcast.producer()).await?;

    // Create a ticket string and print
    let ticket = LiveTicket::new(router.endpoint().id(), name);
    println!("publishing at {ticket}");
    let long_ticket = LiveTicket::new(router.endpoint().addr(), name);
    println!("\nticket with addrs: {long_ticket}");

    // Wait for ctrl-c and then shutdown.
    tokio::signal::ctrl_c().await?;
    live.shutdown();
    router.shutdown().await.std_context("router shutdown")?;

    Ok(())
}

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long, default_value_t=VideoCodec::H264)]
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
