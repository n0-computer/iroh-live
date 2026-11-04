//! Publishes a Raspberry Pi camera with the Pi's hardware H.264 encoder.
//!
//! `rpicam-vid` captures and encodes, and the example publishes its output
//! unchanged. It publishes 640x360 at 30 fps as `pi-cam` and prints a ticket
//! for `irl watch TICKET`. Set `IROH_SECRET` to keep the same ticket across
//! restarts.
//!
//! Needs the `rpicam` feature, and `rpicam-vid` on the Pi:
//!
//! ```sh
//! cargo make cross-build-aarch64 -- -p iroh-live --example publish-pi --features rpicam --release
//! scp target/aarch64-unknown-linux-gnu/release/examples/publish-pi pi@<host>:~/
//! ```

/// Prints that the example needs Linux and the `rpicam` feature.
///
/// Gated on the OS as well as the feature, because `--all-features` on macOS
/// enables `rpicam` without the Linux-only module behind it.
#[cfg(not(all(target_os = "linux", feature = "rpicam")))]
fn main() {
    eprintln!("publish-pi runs on Linux with the `rpicam` feature: it drives rpicam-vid");
    std::process::exit(2);
}

#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh::protocol::Router;
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::{
    EndpointOptions, Live,
    media::{EncodedVideoSource, LocalBroadcast, RpicamConfig, video::Size},
};
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use n0_error::StdResultExt;
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use tracing::info;

/// The broadcast name.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
const BROADCAST: &str = "pi-cam";

#[cfg(all(target_os = "linux", feature = "rpicam"))]
#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();

    let endpoint = EndpointOptions::from_env()?.bind().await?;
    let live = Live::new(endpoint.clone());
    let router = live.mount(Router::builder(endpoint)).spawn();
    // Published while `broadcast` lives, or until the node shuts down.
    let broadcast: LocalBroadcast = live.publish(BROADCAST)?;
    // The default keyframe interval is one second, so a new viewer waits up
    // to a second for a picture.
    let config = RpicamConfig::new(Size::new(640, 360), 30);
    broadcast.set_encoded_video(EncodedVideoSource::rpicam(config).await?)?;
    println!("{}", live.ticket(BROADCAST));

    info!("publishing, press Ctrl-C to stop");
    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    // Also closes the endpoint.
    router
        .shutdown()
        .await
        .std_context("the router failed to shut down")?;
    Ok(())
}
