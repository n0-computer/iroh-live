/// Publish command: run the camera's hardware H.264 encoder through
/// `rpicam-vid` and stream the result over iroh.
use std::time::Duration;

use clap::Parser;
use iroh::EndpointId;
use iroh_live::{Live, ticket::LiveTicket};
use moq_media::rpicam;

use crate::epaper;

/// Per the datasheet, e-paper must be refreshed at least once every 24 h.
/// We re-display the QR every 12 h to stay well within that limit while
/// respecting the minimum 180 s interval between refreshes.
const EPAPER_REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Parser, Debug)]
pub(crate) struct PublishOpts {
    /// Show the connection ticket as a QR code on the e-paper HAT.
    #[clap(long)]
    epaper: bool,

    /// Relay's iroh endpoint ID - additionally connects to the relay so
    /// browser and non-P2P clients can subscribe there. Publishing is
    /// node-wide, so nothing further has to be done once connected: the relay
    /// sees the same announced broadcasts as every other peer.
    #[clap(long)]
    pub relay: Option<EndpointId>,

    /// Broadcast name.
    #[clap(long, default_value = "pi-zero")]
    pub name: String,

    /// Capture width in pixels.
    #[clap(long, default_value = "640")]
    pub width: u32,

    /// Capture height in pixels.
    #[clap(long, default_value = "360")]
    pub height: u32,

    /// Target bitrate (bits/s).
    #[clap(long, default_value = "500000")]
    pub bitrate: u32,

    /// Capture framerate.
    #[clap(long, default_value = "30")]
    pub fps: u32,
}

/// Publishes the camera stream and shows the ticket QR on e-paper.
pub(crate) async fn cmd_publish(opts: PublishOpts) -> n0_error::Result {
    // --- iroh endpoint ---
    // `from_env` binds under `IROH_SECRET` with the n0 preset and mDNS on top
    // of it. The Pi's ticket carries an endpoint id and nothing else, so a
    // viewer on the same network resolves it over mDNS with no internet at all,
    // and a viewer elsewhere resolves it over pkarr and DNS.
    let live = Live::from_env().await?.with_router().spawn();

    // --- media broadcast ---
    let broadcast = live.publish(opts.name.as_str())?;

    let output = rpicam::Output::H264 {
        bitrate: opts.bitrate,
        // A keyframe a second, which is how long a subscriber waits before the
        // picture starts.
        keyframe_interval: opts.fps,
    };
    let config = rpicam::Config::new(opts.width, opts.height, opts.fps, output);
    tracing::info!(
        width = opts.width,
        height = opts.height,
        fps = opts.fps,
        bitrate = opts.bitrate,
        "using pre-encoded H.264 from rpicam-vid"
    );
    broadcast.video().set(rpicam::open(config)?)?;

    // --- relay (optional) ---
    if let Some(relay_id) = opts.relay {
        live.transport().connect(relay_id).await?;
        tracing::info!(%relay_id, "connected to relay");
    }

    // --- ticket (always printed, regardless of e-paper) ---
    let ticket = LiveTicket::new(live.endpoint().id(), &opts.name);
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
                    "could not display QR on e-paper - is the HAT attached and SPI enabled? \
                     (the stream is publishing normally, use the ticket above to connect)"
                );
                false
            }
        }
    } else {
        false
    };

    // Datasheet requires a refresh at least every 24 h. Re-display the QR
    // periodically if the initial display succeeded.
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
