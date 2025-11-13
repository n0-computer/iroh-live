use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_live::{
    Live, PublishBroadcast,
    audio::{AudioBackend, OpusEncoder},
    ffmpeg_log_init,
    video::{CameraCapturer, H264Encoder},
};
use n0_error::StdResultExt;

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    ffmpeg_log_init();

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

    // Audio: default microphone + Opus encoder
    let mic = audio_ctx.default_microphone().await?;
    let opus = OpusEncoder::stereo()?;
    broadcast.set_audio(mic, opus)?;

    // Video: camera capture + H264 encoder (fps 30)
    let camera = CameraCapturer::new()?;
    let h264 = H264Encoder::for_source(&camera, 30)?;
    broadcast.set_video(camera, h264)?;
    let ticket = live.publish(&broadcast).await?;
    println!("publishing at {ticket}");

    tokio::signal::ctrl_c().await?;
    live.shutdown();
    router.shutdown().await.std_context("router shutdown")?;

    Ok(())
}
