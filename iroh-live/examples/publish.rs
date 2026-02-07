use clap::Parser;
use iroh::{Endpoint, SecretKey, protocol::Router};
#[cfg(feature = "av1")]
use iroh_live::media::codec::Av1Encoder;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
use iroh_live::media::codec::VaapiEncoder;
#[cfg(all(target_os = "macos", feature = "videotoolbox"))]
use iroh_live::media::codec::VtbEncoder;
use iroh_live::{
    Live,
    media::{
        audio::AudioBackend,
        av::{AudioPreset, VideoCodec, VideoPreset},
        capture::CameraCapturer,
        codec::{H264Encoder, OpusEncoder},
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
    let audio_ctx = AudioBackend::new(None);

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
        let video = match cli.codec {
            VideoCodec::H264 => VideoRenditions::new::<H264Encoder>(camera, cli.video_presets),
            #[cfg(feature = "av1")]
            VideoCodec::Av1 => VideoRenditions::new::<Av1Encoder>(camera, cli.video_presets),
            #[cfg(not(feature = "av1"))]
            VideoCodec::Av1 => {
                eprintln!("AV1 support requires the `av1` feature");
                std::process::exit(1);
            }
            #[cfg(all(target_os = "macos", feature = "videotoolbox"))]
            VideoCodec::VtbH264 => VideoRenditions::new::<VtbEncoder>(camera, cli.video_presets),
            #[cfg(not(all(target_os = "macos", feature = "videotoolbox")))]
            VideoCodec::VtbH264 => {
                eprintln!("VideoToolbox support requires macOS and the `videotoolbox` feature");
                std::process::exit(1);
            }
            #[cfg(all(target_os = "linux", feature = "vaapi"))]
            VideoCodec::VaapiH264 => {
                VideoRenditions::new::<VaapiEncoder>(camera, cli.video_presets)
            }
            #[cfg(not(all(target_os = "linux", feature = "vaapi")))]
            VideoCodec::VaapiH264 => {
                eprintln!("VAAPI support requires Linux and the `vaapi` feature");
                std::process::exit(1);
            }
        };
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
