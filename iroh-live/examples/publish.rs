use clap::Parser;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_live::{audio::AudioBackend, capture::CameraCapturer, ffmpeg::H264Encoder};
use iroh_moq::{
    Live,
    av::{AudioEncoder, AudioPreset, VideoCodec, VideoEncoder, VideoPreset},
    publish::PublishBroadcast,
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
        .accept(iroh_moq::ALPN, live.protocol_handler())
        .spawn();

    let mut broadcast = PublishBroadcast::new("hello");

    // Audio: default microphone + Opus encoder with preset
    let mic = audio_ctx.default_microphone().await?;
    let opus = iroh_live::ffmpeg::OpusEncoder::with_preset(cli.audio_preset)?;
    broadcast.set_audio(mic, [(opus, cli.audio_preset)])?;

    // Video: camera capture + encoders by backend (fps 30)
    let camera = CameraCapturer::new()?;
    let renditions = cli
        .video_presets
        .into_iter()
        .map(|p| (H264Encoder::with_preset(p).unwrap(), p));
    broadcast.set_video(camera, renditions)?;
    // let renditions = cli.video_presets.into_iter();
    // match (cli.backend, cli.codec) {
    //     (Backend::Native, _) => bail_any!("native backend is unsupported"),
    //     // (Backend::Native, VideoCodec::Av1) => {
    //     //     let encs = renditions.map(|p| (Av1Encoder::with_preset(p).unwrap(), p));
    //     //     broadcast.set_video(camera, encs)?;
    //     // }
    //     (Backend::Ffmpeg, VideoCodec::Av1) => {
    //         let renditions = renditions.map(|p| (Av1FfmpegEncoder::with_preset(p).unwrap(), p));
    //         broadcast.set_video(camera, renditions)?;
    //         let opus = iroh_live::ffmpeg::audio::OpusEncoder::with_preset(cli.audio_preset)?;
    //         broadcast.set_audio(mic, [(opus, cli.audio_preset)])?;
    //     }
    //     (_, VideoCodec::H264) => {
    //         // Use ffmpeg H264 for both backends for now
    //         let encs = renditions.map(|p| (H264Encoder::with_preset(p).unwrap(), p));
    //         broadcast.set_video(camera, encs)?;
    //     }
    // }
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
