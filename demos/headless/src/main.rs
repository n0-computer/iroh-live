//! Headless loopback demo for iroh-live's media pipeline.
//!
//! Publishes a test pattern video and optional test tone audio through the
//! full encode-transport-decode pipeline using in-process moq-lite (no
//! network), then prints per-second stats to stdout. Useful as a health
//! check, benchmark, and quick integration test for the codec pipeline.
//!
//! ```sh
//! cargo run -p headless-demo
//! cargo run -p headless-demo -- --duration 5 --codec h264 --preset 360p
//! cargo run -p headless-demo -- --no-audio
//! ```

use std::time::{Duration, Instant};

use clap::Parser;
use moq_media::{
    codec::{AudioCodec, VideoCodec},
    format::{AudioFormat, AudioPreset, VideoPreset},
    playout::PlaybackPolicy,
    publish::{LocalBroadcast, VideoInput},
    subscribe::RemoteBroadcast,
    test_util::{NullAudioBackend, TestAudioSource, TestVideoSource},
};
use tracing::{debug, error, info, warn};

/// Headless loopback demo: publish test pattern, subscribe, verify frames.
#[derive(Parser, Debug)]
#[command(name = "headless-demo")]
struct Args {
    /// Run duration in seconds (0 = indefinite, Ctrl-C to stop).
    #[arg(long, default_value_t = 10)]
    duration: u64,

    /// Video codec to use.
    #[arg(long, default_value = "h264")]
    codec: String,

    /// Video resolution preset (180p, 360p, 720p, 1080p).
    #[arg(long, default_value = "360p")]
    preset: String,

    /// Disable audio track.
    #[arg(long)]
    no_audio: bool,

    /// Stats reporting interval in seconds.
    #[arg(long, default_value_t = 1)]
    stats_interval: u64,
}

fn parse_video_codec(s: &str) -> anyhow::Result<VideoCodec> {
    s.parse::<VideoCodec>().map_err(|_| {
        let available = VideoCodec::available()
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!("unknown video codec '{s}'. Available: {available}")
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let codec = parse_video_codec(&args.codec)?;
    let preset = VideoPreset::parse_or_list(&args.preset)?;
    let (width, height) = preset.dimensions();

    info!(
        codec = %codec,
        preset = %preset,
        duration_secs = args.duration,
        audio = !args.no_audio,
        "starting headless loopback"
    );

    // --- Publish side ---
    let broadcast = LocalBroadcast::new();

    let source = TestVideoSource::new(width, height).with_fps(30.0);
    broadcast
        .video()
        .set(VideoInput::new(source, codec, [preset]))?;

    if !args.no_audio {
        let audio_source = TestAudioSource::new(AudioFormat::mono_48k());
        broadcast
            .audio()
            .set(audio_source, AudioCodec::Opus, [AudioPreset::Hq])?;
    }

    info!("publish pipeline started");

    // --- Subscribe side (in-process loopback via moq-lite) ---
    let consumer = broadcast.consume();
    let remote =
        RemoteBroadcast::with_playback_policy("loopback", consumer, PlaybackPolicy::unmanaged())
            .await?;

    let catalog = remote.catalog();
    let video_renditions: Vec<String> = catalog.video_renditions().map(String::from).collect();
    let audio_renditions: Vec<String> = catalog.audio_renditions().map(String::from).collect();
    info!(
        video_renditions = ?video_renditions,
        audio_renditions = ?audio_renditions,
        "catalog received"
    );

    let mut video_track = remote.video_ready().await?;
    info!(
        rendition = video_track.rendition(),
        "video track subscribed"
    );

    if !args.no_audio {
        let audio_backend = NullAudioBackend;
        let _audio_track = remote.audio_ready(&audio_backend).await?;
        info!("audio track subscribed (output to null sink)");
    }

    // --- Frame consumption loop with stats ---
    let run_duration = if args.duration == 0 {
        Duration::MAX
    } else {
        Duration::from_secs(args.duration)
    };
    let stats_interval = Duration::from_secs(args.stats_interval);

    let start = Instant::now();
    let mut interval_start = start;
    let mut interval_frames: u64 = 0;
    let mut total_frames: u64 = 0;
    let mut first_frame_latency: Option<Duration> = None;

    let encode_stats = broadcast.stats().encode.clone();

    loop {
        let remaining = run_duration.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }

        let frame = tokio::select! {
            frame = video_track.next_frame() => frame,
            () = tokio::time::sleep(remaining) => {
                debug!("duration elapsed, stopping");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("interrupted");
                break;
            }
        };

        let Some(frame) = frame else {
            warn!("video track closed unexpectedly");
            break;
        };

        total_frames += 1;
        interval_frames += 1;

        if first_frame_latency.is_none() {
            let latency = start.elapsed();
            first_frame_latency = Some(latency);
            info!(
                width = frame.dimensions[0],
                height = frame.dimensions[1],
                first_frame_ms = latency.as_millis(),
                "first frame received"
            );
        }

        let now = Instant::now();
        let interval_elapsed = now.duration_since(interval_start);
        if interval_elapsed >= stats_interval {
            let fps = interval_frames as f64 / interval_elapsed.as_secs_f64();
            let encode_fps = encode_stats.fps.current();
            let encode_ms = encode_stats.encode_ms.current();
            let bitrate_kbps = encode_stats.bitrate_kbps.current();
            let codec_label = encode_stats.codec.get();
            let resolution = encode_stats.resolution.get();

            println!(
                "[{elapsed:>5.1}s] decode_fps={fps:.1}  encode_fps={encode_fps:.1}  \
                 encode_ms={encode_ms:.1}  bitrate={bitrate_kbps:.0} kbps  \
                 codec={codec_label}  res={resolution}  frames={total_frames}",
                elapsed = start.elapsed().as_secs_f64(),
            );

            interval_start = now;
            interval_frames = 0;
        }
    }

    // --- Summary ---
    let elapsed = start.elapsed();
    let avg_fps = if elapsed.as_secs_f64() > 0.0 {
        total_frames as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    println!();
    println!("--- summary ---");
    println!("  elapsed:      {:.1}s", elapsed.as_secs_f64());
    println!("  total frames: {total_frames}");
    println!("  average fps:  {avg_fps:.1}");
    if let Some(latency) = first_frame_latency {
        println!("  first frame:  {}ms", latency.as_millis());
    }

    let final_bitrate = encode_stats.bitrate_kbps.current();
    let final_encode_ms = encode_stats.encode_ms.current();
    if encode_stats.fps.has_samples() {
        println!("  encode time:  {final_encode_ms:.1}ms");
        println!("  bitrate:      {final_bitrate:.0} kbps");
    }

    if total_frames == 0 {
        error!("no frames received -- pipeline health check FAILED");
        std::process::exit(1);
    }

    info!(
        total_frames,
        avg_fps = format!("{avg_fps:.1}"),
        "pipeline health check passed"
    );
    Ok(())
}
