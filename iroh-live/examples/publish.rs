use clap::Parser;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_live::{
    audio::AudioBackend,
    capture::CameraCapturer,
    ffmpeg::{H264Encoder, OpusEncoder},
};
use iroh_moq::{
    Live,
    av::{AudioPreset, VideoCodec, VideoPreset},
    publish::{AudioRenditions, PublishBroadcast, VideoRenditions},
};
use n0_error::StdResultExt;

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let audio_ctx = AudioBackend::new();

    let secret_key = secret_key_from_env()?;
    let endpoint = Endpoint::builder().secret_key(secret_key).bind().await?;
    let live = Live::new(endpoint.clone());
    let router = Router::builder(endpoint)
        .accept(iroh_moq::ALPN, live.protocol_handler())
        .spawn();

    let mut broadcast = PublishBroadcast::new("hello");

    // Audio: default microphone + Opus encoder with preset
    let mic = audio_ctx.default_microphone().await?;
    let audio = AudioRenditions::new::<OpusEncoder>(mic, [cli.audio_preset]);
    broadcast.set_audio(audio)?;

    // Video: camera capture + encoders by backend (fps 30)
    let camera = CameraCapturer::new()?;
    let video = VideoRenditions::new::<H264Encoder>(camera, cli.video_presets);
    broadcast.set_video(video)?;

    let ticket = live.publish(&broadcast).await?;
    println!("publishing at {ticket}");

    tokio::signal::ctrl_c().await?;
    live.shutdown();
    router.shutdown().await.std_context("router shutdown")?;

    Ok(())
}

#[derive(Parser, Debug)]
struct Cli {
    // #[arg(long, default_value_t=Backend::Ffmpeg)]
    // backend: Backend,
    #[arg(long, default_value_t=VideoCodec::H264)]
    codec: VideoCodec,
    #[arg(long, value_delimiter=',', default_values_t=[VideoPreset::P720])]
    video_presets: Vec<VideoPreset>,
    #[arg(long, default_value_t=AudioPreset::Hq)]
    audio_preset: AudioPreset,
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
