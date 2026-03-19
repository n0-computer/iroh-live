//! Receives video frames from a remote broadcast and saves them as PNGs.
//!
//! Optionally compares received frames against the SMPTE test pattern to
//! verify end-to-end codec correctness. Can also publish a test pattern
//! itself for loopback testing.
//!
//! Usage:
//!   # Watch a remote stream and dump frames
//!   cargo run --example frame_dump -- watch --ticket <TICKET> --out /tmp/frames
//!
//!   # Publish a test pattern (run in another terminal, then watch it)
//!   cargo run --example frame_dump -- publish
//!
//!   # Watch and verify against test pattern (PSNR check)
//!   cargo run --example frame_dump -- watch --ticket <TICKET> --out /tmp/frames --verify

use std::{path::PathBuf, time::Instant};

use clap::{Parser, Subcommand};
use iroh::Endpoint;
use moq_media::{test_util::TestVideoSource, traits::VideoSource as _};

#[derive(Parser)]
#[command(about = "Dump or verify video frames from an iroh-live broadcast")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Subscribe to a broadcast and save decoded frames as PNGs.
    Watch(WatchOpts),
    /// Publish a test pattern for loopback testing.
    Publish(PublishOpts),
    /// Generate reference test-pattern PNGs without any network round-trip.
    Reference(ReferenceOpts),
}

#[derive(Parser)]
struct WatchOpts {
    /// Connection ticket.
    ticket: iroh_live::ticket::LiveTicket,
    /// Output directory for PNGs.
    #[arg(long, default_value = "test-results/frames")]
    out: PathBuf,
    /// Number of frames to capture before exiting.
    #[arg(long, default_value_t = 10)]
    frames: u32,
    /// Compare received frames against the SMPTE test pattern and report PSNR.
    #[arg(long)]
    verify: bool,
    /// Timeout per frame in seconds.
    #[arg(long, default_value_t = 15)]
    timeout: u64,
}

#[derive(Parser)]
struct PublishOpts {
    /// Broadcast name.
    #[arg(long, default_value = "test-pattern")]
    name: String,
    /// Resolution width.
    #[arg(long, default_value_t = 640)]
    width: u32,
    /// Resolution height.
    #[arg(long, default_value_t = 360)]
    height: u32,
    /// Framerate.
    #[arg(long, default_value_t = 30.0)]
    fps: f64,
}

#[derive(Parser)]
struct ReferenceOpts {
    /// Output directory for reference PNGs.
    #[arg(long, default_value = "test-results/reference")]
    out: PathBuf,
    /// Number of frames to generate.
    #[arg(long, default_value_t = 5)]
    frames: u32,
    /// Resolution width.
    #[arg(long, default_value_t = 640)]
    width: u32,
    /// Resolution height.
    #[arg(long, default_value_t = 360)]
    height: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::Watch(opts) => cmd_watch(opts).await,
        Command::Publish(opts) => cmd_publish(opts).await,
        Command::Reference(opts) => cmd_reference(opts),
    }
}

async fn cmd_watch(opts: WatchOpts) -> anyhow::Result<()> {
    std::fs::create_dir_all(&opts.out)?;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .bind()
        .await?;
    let live = iroh_live::Live::builder(endpoint).spawn_with_router();

    println!("connecting to {} ...", opts.ticket);

    let (_session, broadcast) = live
        .subscribe(opts.ticket.endpoint, &opts.ticket.broadcast_name)
        .await?;

    println!("subscribed, waiting for video track");
    let mut track = broadcast.video_ready().await?;

    let mut received = 0u32;
    let start = Instant::now();

    while received < opts.frames {
        let timeout = std::time::Duration::from_secs(opts.timeout);
        match tokio::time::timeout(timeout, track.next_frame()).await {
            Ok(Some(frame)) => {
                let w = frame.width();
                let h = frame.height();
                let rgba = frame.rgba_image().clone();

                let path = opts.out.join(format!("frame_{:04}.png", received));
                rgba.save(&path)?;

                let elapsed = start.elapsed();
                let mut line = format!(
                    "frame {:3}: {}x{} ts={:.3}s saved={}",
                    received,
                    w,
                    h,
                    elapsed.as_secs_f64(),
                    path.display(),
                );

                if opts.verify {
                    // Compare only the static region (top 70% = SMPTE color bars)
                    // to avoid false negatives from the animated bouncing line.
                    let bar_psnr = compute_color_bar_psnr(&rgba);
                    line.push_str(&format!("  bars_PSNR={:.1}dB", bar_psnr));

                    if bar_psnr < 20.0 {
                        line.push_str(" ** LOW QUALITY **");
                    } else if bar_psnr < 25.0 {
                        line.push_str(" (degraded)");
                    }
                }

                println!("{line}");
                received += 1;
            }
            Ok(None) => {
                anyhow::bail!(
                    "video track ended after {received} frames (expected {})",
                    opts.frames
                );
            }
            Err(_) => {
                anyhow::bail!("timeout waiting for frame {} ({}s)", received, opts.timeout);
            }
        }
    }

    println!(
        "\ndone: {received} frames captured in {:.1}s",
        start.elapsed().as_secs_f64()
    );
    println!("frames saved to {}", opts.out.display());

    if opts.verify {
        println!("\nverification complete (check PSNR values above)");
        println!("  > 30 dB: good (codec artifacts within normal range)");
        println!("  20-30 dB: degraded (heavy compression or color shift)");
        println!("  < 20 dB: broken (wrong content, corrupted decode, or color space issue)");
    }

    live.shutdown().await;
    Ok(())
}

async fn cmd_publish(opts: PublishOpts) -> anyhow::Result<()> {
    use moq_media::publish::LocalBroadcast;

    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .bind()
        .await?;
    let live = iroh_live::Live::builder(endpoint).spawn_with_router();

    let broadcast = LocalBroadcast::new();

    let source = TestVideoSource::new(opts.width, opts.height).with_fps(opts.fps);
    let codec = moq_media::codec::VideoCodec::H264;
    let preset = match opts.height {
        0..=180 => moq_media::format::VideoPreset::P180,
        181..=360 => moq_media::format::VideoPreset::P360,
        361..=720 => moq_media::format::VideoPreset::P720,
        _ => moq_media::format::VideoPreset::P1080,
    };
    broadcast.video().set(moq_media::publish::VideoInput::new(
        source,
        codec,
        vec![preset],
    ))?;

    live.publish(&opts.name, &broadcast).await?;

    let ticket = iroh_live::ticket::LiveTicket::new(live.endpoint().addr(), &opts.name);
    println!("publishing test pattern at:");
    println!("  {ticket}");
    println!("\nwatch with:");
    println!("  cargo run --example frame_dump -- watch {ticket} --verify");

    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}

fn cmd_reference(opts: ReferenceOpts) -> anyhow::Result<()> {
    std::fs::create_dir_all(&opts.out)?;
    let frames = generate_reference_frames(opts.width, opts.height, opts.frames);
    for (i, frame) in frames.iter().enumerate() {
        let path = opts.out.join(format!("ref_{:04}.png", i));
        frame.save(&path)?;
        println!("saved {}", path.display());
    }
    println!(
        "\n{} reference frames saved to {}",
        frames.len(),
        opts.out.display()
    );
    Ok(())
}

/// Generates test pattern frames matching what TestVideoSource produces.
fn generate_reference_frames(width: u32, height: u32, count: u32) -> Vec<image::RgbaImage> {
    let mut source = TestVideoSource::new(width, height).with_fps(30.0);
    let _ = source.start();
    let mut frames = Vec::new();
    for _ in 0..count {
        match source.pop_frame() {
            Ok(Some(frame)) => frames.push(frame.rgba_image().clone()),
            _ => break,
        }
    }
    frames
}

/// Computes PSNR of the color bar region against the expected SMPTE bar colors.
///
/// The test pattern has seven vertical color bars in the top 70% of the frame.
/// We sample a horizontal strip at 35% height (middle of the bars) and compare
/// each pixel against the expected bar color. This avoids the animated line and
/// beep indicator, giving a stable metric that only degrades from actual codec
/// issues (color shift, blocking artifacts, wrong decode).
fn compute_color_bar_psnr(frame: &image::RgbaImage) -> f64 {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    if w == 0 || h == 0 {
        return 0.0;
    }

    // SMPTE bar colors (RGB), same as in test_sources.rs.
    let bars: [[u8; 3]; 7] = [
        [255, 255, 255], // white
        [255, 255, 0],   // yellow
        [0, 255, 255],   // cyan
        [0, 255, 0],     // green
        [255, 0, 255],   // magenta
        [255, 0, 0],     // red
        [0, 0, 255],     // blue
    ];

    let bar_width = w / 7;
    // Sample a horizontal band at 25-35% height (well within the bar region,
    // away from edges that might have interpolation artifacts).
    let y_start = h * 25 / 100;
    let y_end = h * 35 / 100;

    let pixels = frame.as_raw();
    let mut mse_sum: f64 = 0.0;
    let mut count = 0u64;

    for y in y_start..y_end {
        for (bar_idx, &expected) in bars.iter().enumerate().take(7) {
            // Sample the center third of each bar to avoid edge blending.
            let x_start = bar_idx * bar_width + bar_width / 3;
            let x_end = bar_idx * bar_width + bar_width * 2 / 3;
            for x in x_start..x_end {
                let base = (y * w + x) * 4;
                if base + 2 >= pixels.len() {
                    continue;
                }
                for c in 0..3 {
                    let diff = pixels[base + c] as f64 - expected[c] as f64;
                    mse_sum += diff * diff;
                }
                count += 1;
            }
        }
    }

    if count == 0 {
        return 0.0;
    }
    let mse = mse_sum / (count as f64 * 3.0);
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}
