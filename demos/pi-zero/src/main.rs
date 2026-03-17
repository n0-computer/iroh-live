/// Raspberry Pi Zero 2 demo: publish a camera stream over iroh and display
/// the connection ticket as a QR code on a Waveshare 2.13" e-paper HAT.
/// Also supports watching a remote stream with EGL/GLES2 rendering.
use std::time::Duration;

use clap::{Parser, Subcommand};
use iroh::{Endpoint, EndpointId};
use iroh_live::{
    Live,
    media::{
        codec::{DefaultDecoders, VideoCodec},
        format::{DecodeConfig, DecoderBackend, PlaybackConfig, VideoPreset},
        publish::LocalBroadcast,
    },
    ticket::LiveTicket,
};

mod epaper;
mod epd_v4;
mod libcamera;
mod watch;

/// Per the datasheet, e-paper must be refreshed at least once every 24 h.
/// We re-display the QR every 12 h to stay well within that limit while
/// respecting the minimum 180 s interval between refreshes.
const EPAPER_REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Parser)]
#[command(about = "Pi Zero 2 demo: camera streaming + e-paper QR ticket")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Test the e-paper display with a "hello" label and a bogus QR code.
    EpaperDemo,
    /// Publish the camera stream and show the ticket QR on e-paper.
    Publish(PublishOpts),
    /// Watch a remote stream, rendering with EGL/GLES2.
    Watch(WatchOpts),
    /// Render a test pattern directly to HDMI (no network, no window system).
    FbDemo,
}

#[derive(Parser, Debug)]
struct PublishOpts {
    #[clap(long)]
    epaper: bool,
    /// Encoder: "hardware" (V4L2/VAAPI/VTB), "software" (openh264), or "ffmpeg".
    #[clap(long, default_value = "hardware")]
    encoder: String,
    /// Relay's iroh endpoint ID — additionally publishes to the relay so
    /// browser and non-P2P clients can subscribe.
    #[clap(long)]
    relay: Option<EndpointId>,
}

#[derive(Parser, Debug)]
struct WatchOpts {
    /// Connection ticket (alternative to --endpoint-id + --name).
    #[clap(long, conflicts_with = "endpoint_id")]
    ticket: Option<LiveTicket>,
    /// Remote endpoint ID (requires --name).
    #[clap(long, conflicts_with = "ticket", requires = "name")]
    endpoint_id: Option<EndpointId>,
    /// Broadcast name.
    #[clap(long, conflicts_with = "ticket", requires = "endpoint_id")]
    name: Option<String>,
    /// Render direct to HDMI framebuffer via DRM/KMS (no window system).
    #[clap(long)]
    fb: bool,
    /// Start in fullscreen mode (windowed mode only, ignored with --fb).
    #[clap(long)]
    fullscreen: bool,
    /// Decoder: "auto" (try HW then SW), "software", or "ffmpeg".
    #[clap(long, default_value = "auto")]
    decoder: String,
}

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::EpaperDemo => cmd_epaper_demo(),
        Command::Publish(opts) => cmd_publish(opts).await,
        Command::Watch(opts) => cmd_watch(opts).await,
        Command::FbDemo => cmd_fb_demo(),
    }
}

/// Runs a hardware test sequence on the e-paper HAT.
///
/// 1. Checkerboard test pattern (tests basic SPI + display wiring)
/// 2. QR code with dummy data (tests rendering pipeline)
/// 3. Clear to white
fn cmd_epaper_demo() -> n0_error::Result {
    println!("step 1/3: checkerboard test pattern");
    epaper::display_test_pattern()?;
    println!("  displayed — you should see a checkerboard now");
    wait_for_enter();

    println!("step 2/3: QR code with dummy data");
    epaper::display_qr("https://iroh.computer/hello-from-pi-zero")?;
    println!("  displayed — you should see a QR code now");
    wait_for_enter();

    println!("step 3/3: clearing display");
    epaper::clear_display()?;
    println!("  done — display should be white");

    Ok(())
}

fn wait_for_enter() {
    println!("  press Enter to continue...");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
}

/// Publishes the camera stream and shows the ticket QR on e-paper.
async fn cmd_publish(opts: PublishOpts) -> n0_error::Result {
    // --- iroh endpoint ---
    let secret_key = iroh_live::util::secret_key_from_env()?;
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key)
        .bind()
        .await?;
    let live = Live::builder(endpoint).spawn_with_router();

    // --- media broadcast ---
    let broadcast = LocalBroadcast::new();

    // Capture camera via rpicam-vid (libcamera). On Pi OS Bookworm the CSI
    // camera is only accessible through libcamera — direct V4L2 gives raw
    // Bayer data from the Unicam sensor, not usable video frames.
    let camera = libcamera::LibcameraSource::new(640, 360, 30);

    let codec = match opts.encoder.as_str() {
        "hardware" | "hw" => VideoCodec::best_available().expect("no video codec compiled in"),
        "software" | "sw" => VideoCodec::H264,
        #[cfg(feature = "ffmpeg")]
        "ffmpeg" => VideoCodec::FfmpegH264,
        #[cfg(not(feature = "ffmpeg"))]
        "ffmpeg" => {
            eprintln!("ffmpeg encoder not compiled in — build with --features ffmpeg");
            std::process::exit(1);
        }
        other => {
            eprintln!("Unknown encoder: {other}. Use 'hardware', 'software', or 'ffmpeg'");
            std::process::exit(1);
        }
    };
    tracing::info!(%codec, "selected video codec");

    broadcast.video().set(camera, codec, [VideoPreset::P360])?;

    // Publish under a fixed name.
    let name = "pi-zero";
    live.publish(name, &broadcast).await?;

    // --- relay (optional) ---
    if let Some(relay_id) = opts.relay {
        let session = live.transport().connect(relay_id).await?;
        session.publish(name.to_string(), broadcast.producer().consume());
        tracing::info!(%relay_id, "published to relay");
    }

    // --- ticket (always printed, regardless of e-paper) ---
    let ticket = LiveTicket::new(live.endpoint().addr(), name);
    let ticket_str = ticket.to_string();
    println!("publishing at {ticket_str}");

    // --- QR code on e-paper (optional, non-fatal) ---
    let has_epaper = if opts.epaper {
        match epaper::display_qr(&ticket_str) {
            Ok(()) => {
                tracing::info!("QR code displayed on e-paper");
                true
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "could not display QR on e-paper — is the HAT attached and SPI enabled? \
                 (the stream is publishing normally, use the ticket above to connect)"
                );
                false
            }
        }
    } else {
        false
    };

    // Datasheet requires a refresh at least every 24 h. Re-display the QR
    // periodically if the initial display succeeded. If the HAT isn't present
    // we skip this entirely — no point retrying in a loop.
    let refresh_ticket = ticket_str.clone();
    let refresh_handle = if has_epaper {
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(EPAPER_REFRESH_INTERVAL).await;
                match epaper::display_qr(&refresh_ticket) {
                    Ok(()) => tracing::debug!("periodic e-paper refresh complete"),
                    Err(e) => {
                        tracing::warn!(error = format!("{e:#}"), "periodic e-paper refresh failed")
                    }
                }
            }
        }))
    } else {
        None
    };

    // Wait for ctrl-c and then shutdown.
    tokio::signal::ctrl_c().await?;

    // Cancel the periodic refresh task.
    if let Some(handle) = refresh_handle {
        handle.abort();
    }

    // Clear the e-paper before exit (datasheet: clear before storage).
    if has_epaper {
        match epaper::clear_display() {
            Ok(()) => tracing::info!("e-paper cleared for storage"),
            Err(e) => tracing::warn!(error = format!("{e:#}"), "could not clear e-paper on exit"),
        }
    }

    live.shutdown().await;

    Ok(())
}

/// Renders a test pattern directly to HDMI — no network, no window system.
fn cmd_fb_demo() -> n0_error::Result {
    use moq_media::format::DecodeConfig;
    use moq_media::subscribe::VideoTrack;
    use moq_media::test_util::TestVideoSource;
    use tokio_util::sync::CancellationToken;

    let source = TestVideoSource::new(640, 480).with_fps(30.0);
    let shutdown = CancellationToken::new();
    let video_track =
        VideoTrack::from_video_source("test".into(), shutdown, source, DecodeConfig::default());

    watch::run_fb_demo(video_track)?;
    Ok(())
}

/// Watches a remote broadcast, rendering with EGL/GLES2.
async fn cmd_watch(opts: WatchOpts) -> n0_error::Result {
    let ticket = match (&opts.ticket, &opts.endpoint_id, &opts.name) {
        (Some(t), None, None) => t.clone(),
        (None, Some(id), Some(name)) => LiveTicket::new(*id, name.clone()),
        _ => {
            eprintln!("Usage: watch --ticket <TICKET> or --endpoint-id <ID> --name <NAME>");
            std::process::exit(1);
        }
    };
    let backend = match opts.decoder.as_str() {
        "auto" | "hardware" | "hw" => DecoderBackend::Auto,
        "software" | "sw" => DecoderBackend::Software,
        "ffmpeg" => {
            // DecoderBackend::Auto will try ffmpeg if compiled in (after
            // platform HW decoders). Force software for now unless a dedicated
            // ffmpeg-only backend is added.
            #[cfg(feature = "ffmpeg")]
            {
                DecoderBackend::Auto
            }
            #[cfg(not(feature = "ffmpeg"))]
            {
                eprintln!("ffmpeg decoder not compiled in — build with --features ffmpeg");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("Unknown decoder: {other}. Use 'auto', 'software', or 'ffmpeg'");
            std::process::exit(1);
        }
    };

    // Pi Zero has no PipeWire/PulseAudio — use a null audio backend
    // to avoid panicking on missing audio devices.
    let audio_ctx = moq_media::test_util::NullAudioBackend;

    println!("connecting to {ticket} ...");
    let endpoint = Endpoint::bind(iroh::endpoint::presets::N0).await?;
    let live = Live::new(endpoint);
    let playback_config = PlaybackConfig {
        decode_config: DecodeConfig {
            backend,
            ..Default::default()
        },
        ..Default::default()
    };
    let (session, track) = live
        .subscribe_media_track::<DefaultDecoders>(
            ticket.endpoint,
            &ticket.broadcast_name,
            &audio_ctx,
            playback_config,
        )
        .await?;
    println!("connected!");

    let video_track = track.video.expect("no video track in broadcast");

    if opts.fb {
        watch::run_drm(video_track, session).await?;
    } else {
        #[cfg(feature = "windowed")]
        watch::run_windowed(video_track, session, opts.fullscreen)?;
        #[cfg(not(feature = "windowed"))]
        {
            eprintln!("windowed mode not compiled in — use --fb or build with --features windowed");
            std::process::exit(1);
        }
    }

    Ok(())
}
