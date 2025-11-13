use clap::{Parser, ValueEnum};
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_live::{
    Live, PublishBroadcast,
    audio::AudioBackend,
    av::{VideoSource, Backend, VideoPreset, AudioPreset},
    native::video::Av1Encoder,
    video::{CameraCapturer, Av1FfmpegEncoder},
};
use n0_error::StdResultExt;

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let audio_ctx = AudioBackend::new();

    let secret_key = match std::env::var("IROH_SECRET") {
        Ok(key) => key.parse()?,
        Err(_) => {
            let key = SecretKey::generate(&mut rand::rng());
            println!(
                "Created new secret. Reuse with IROH_SECRET={}",
                data_encoding::HEXLOWER.encode(&key.to_bytes())
            );
            key
        }
    };
    let endpoint = Endpoint::builder().secret_key(secret_key).bind().await?;
    let live = Live::new(endpoint.clone());
    let router = Router::builder(endpoint)
        .accept(iroh_live::ALPN, live.protocol_handler())
        .spawn();

    let mut broadcast = PublishBroadcast::new("hello");

    // Audio: default microphone + Opus encoder with preset
    let mic = audio_ctx.default_microphone().await?;
    let opus = iroh_live::native::audio::OpusEncoder::with_preset(cli.audio_preset.into())?;
    broadcast.set_audio(mic, [(opus, cli.audio_preset.into())])?;

    // Video: camera capture + encoders by backend (fps 30)
    let camera = CameraCapturer::new()?;
    let [w, h] = camera.format().dimensions;
    match cli.backend.into() {
        Backend::Native => {
            let renditions: Vec<_> = cli.video_presets.iter().map(|p| {
                let enc = Av1Encoder::with_preset(w, h, 30, (*p).into()).unwrap();
                (enc, (*p).into())
            }).collect();
            broadcast.set_video(camera, renditions)?;
        }
        Backend::Ffmpeg => {
            let renditions: Vec<_> = cli.video_presets.iter().map(|p| {
                let enc = Av1FfmpegEncoder::new(w, h, 30).unwrap();
                (enc, (*p).into())
            }).collect();
            broadcast.set_video(camera, renditions)?;
        }
    }
    let ticket = live.publish(&broadcast).await?;
    println!("publishing at {ticket}");

    tokio::signal::ctrl_c().await?;
    live.shutdown();
    router.shutdown().await.std_context("router shutdown")?;

    Ok(())
}

#[derive(Copy, Clone, ValueEnum, Debug)]
enum CliVideoPreset { P180, P360, P720, P1080 }
impl From<CliVideoPreset> for VideoPreset { fn from(v: CliVideoPreset) -> Self { match v { CliVideoPreset::P180 => VideoPreset::P180, CliVideoPreset::P360 => VideoPreset::P360, CliVideoPreset::P720 => VideoPreset::P720, CliVideoPreset::P1080 => VideoPreset::P1080 } } }

#[derive(Copy, Clone, ValueEnum, Debug)]
enum CliAudioPreset { Hq, Lq }
impl From<CliAudioPreset> for AudioPreset { fn from(v: CliAudioPreset) -> Self { match v { CliAudioPreset::Hq => AudioPreset::Hq, CliAudioPreset::Lq => AudioPreset::Lq } } }

#[derive(Copy, Clone, ValueEnum, Debug)]
enum CliBackend { Native, Ffmpeg }
impl From<CliBackend> for Backend { fn from(v: CliBackend) -> Self { match v { CliBackend::Native => Backend::Native, CliBackend::Ffmpeg => Backend::Ffmpeg } } }

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long, value_enum, default_value_t=CliBackend::Native)]
    backend: CliBackend,
    #[arg(long, value_enum, value_delimiter=',', default_values_t=[CliVideoPreset::P720])]
    video_presets: Vec<CliVideoPreset>,
    #[arg(long, value_enum, default_value_t=CliAudioPreset::Hq)]
    audio_preset: CliAudioPreset,
}
