//! Publishes a Raspberry Pi camera, using the camera's own hardware H.264.
//!
//! `rpicam-vid` drives the libcamera ISP and the Pi's hardware encoder, so the
//! bytes it writes are published unchanged and the Pi never software-encodes.
//! Prints a ticket for viewers to connect with `irl watch TICKET`.
//!
//! Needs the `rpicam` feature, and `rpicam-vid` on the Pi:
//!
//! ```sh
//! cargo make cross-build-aarch64 -- -p iroh-live --example publish-pi --features rpicam --release
//! scp target/aarch64-unknown-linux-gnu/release/examples/publish-pi pi@<host>:~/
//! ```
//!
//! No flags. It publishes 640x360 at 30 fps under the path `pi-cam`. Set
//! `IROH_SECRET` to keep the same endpoint id, and so the same ticket, across
//! restarts; the first run logs a value to reuse.

/// The Raspberry Pi camera is reached through `rpicam-vid`, which exists on
/// Linux and nowhere else, so on any other platform this example is a message
/// rather than a program. The feature alone is not enough to gate on: a
/// `--all-features` check on macOS turns `rpicam` on and the module it names
/// still does not exist there.
#[cfg(not(all(target_os = "linux", feature = "rpicam")))]
fn main() {
    eprintln!("publish-pi runs on Linux with the `rpicam` feature: it drives rpicam-vid");
    std::process::exit(2);
}

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::{Live, media::rpicam, ticket::LiveTicket};

/// The path the camera publishes on. Viewers subscribe to the same one.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const BROADCAST: &str = "pi-cam";

#[cfg(all(target_os = "linux", feature = "rpicam"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let live = Live::from_env().await?.with_router().spawn();
    let broadcast = live.publish(BROADCAST)?;
    broadcast.video().set(rpicam::open(rpicam::Config::new(
        640,
        360,
        30,
        rpicam::Output::H264 {
            bitrate: rpicam::DEFAULT_BITRATE,
            // A keyframe a second, which is how long a viewer waits for a first
            // picture after scanning the ticket.
            keyframe_interval: 30,
        },
    ))?)?;

    let ticket = LiveTicket::new(live.endpoint().id(), BROADCAST);
    println!("{ticket}");

    tracing::info!("publishing, press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
