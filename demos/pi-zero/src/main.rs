/// Raspberry Pi Zero 2 demo: publish a camera stream over iroh and display
/// the connection ticket as a QR code on a Waveshare 2.13" e-paper HAT.
use std::time::Duration;

use clap::{Parser, Subcommand};
use iroh::Endpoint;
use iroh_live::{
    Live,
    media::{codec::VideoCodec, format::VideoPreset, publish::LocalBroadcast},
    ticket::LiveTicket,
};

mod epaper;
mod epd_v4;
mod libcamera;

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
}

#[derive(Parser, Debug)]
struct PublishOpts {
    #[clap(long)]
    epaper: bool,
}

#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::EpaperDemo => cmd_epaper_demo(),
        Command::Publish(opts) => cmd_publish(opts).await,
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
    let camera = libcamera::LibcameraSource::new(1280, 720, 30);

    // Use the best available H.264 encoder. With the v4l2 feature enabled this
    // picks the VideoCore hardware encoder; without it, falls back to openh264.
    let codec = VideoCodec::best_available().expect("no video codec compiled in");
    tracing::info!(%codec, "selected video codec");

    broadcast.video().set(camera, codec, [VideoPreset::P720])?;

    // Publish under a fixed name.
    let name = "pi-zero";
    live.publish(name, &broadcast).await?;

    // --- ticket (always printed, regardless of e-paper) ---
    let ticket = LiveTicket::new(live.endpoint().id(), name);
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
