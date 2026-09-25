//! Publish command: streams the camera's hardware H.264 over iroh.
//!
//! The encoder output is read through `rpicam-vid`.

use std::time::Duration;

use clap::Parser;
use iroh::EndpointId;
use iroh_live::{Live, LocalBroadcast, moq::RelayConfig};
use iroh_live_media::{Bitrate, EncodedVideoSource, RpicamConfig, video::Size};
use tracing::{debug, info, warn};

use crate::epaper;

/// How often to redraw the QR code.
///
/// The datasheet asks for a refresh at least every 24 h and at most every 180 s.
const EPAPER_REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Parser, Debug)]
pub(crate) struct PublishOpts {
    /// Show the connection ticket as a QR code on the e-paper HAT.
    #[clap(long)]
    epaper: bool,

    /// Relay endpoint ID to also push to, so browsers can watch there.
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
    // The ticket carries only the endpoint id. A viewer on the same network
    // finds the Pi over mDNS, a viewer elsewhere over pkarr and DNS.
    let live = Live::builder(iroh_live::EndpointOptions::from_env()?.bind().await?)
        .with_router()
        .spawn();

    let broadcast = LocalBroadcast::new();

    // Keeps the default of one keyframe per second.
    let config = RpicamConfig {
        bitrate: Bitrate::from_bps(u64::from(opts.bitrate)),
        ..RpicamConfig::new(Size::new(opts.width, opts.height), opts.fps)
    };
    info!(
        width = opts.width,
        height = opts.height,
        fps = opts.fps,
        bitrate = opts.bitrate,
        "using pre-encoded H.264 from rpicam-vid"
    );
    broadcast.set_encoded_video(EncodedVideoSource::rpicam(config).await?)?;
    live.publish(opts.name.as_str(), &broadcast)?;

    // The link redials the relay if the session drops. It only pushes: the
    // relay's routes stay out of the Pi's route table.
    let _relay = match opts.relay {
        Some(relay) => {
            info!(%relay, "pushing to relay");
            Some(live.moq().attach_relay(RelayConfig {
                consume: false,
                ..RelayConfig::iroh(relay)
            })?)
        }
        None => None,
    };

    let ticket = live.ticket(&opts.name);
    let ticket_str = ticket.to_string();
    println!("publishing at {ticket_str}");

    // The e-paper is optional: a failure only logs a warning.
    let has_epaper = if opts.epaper {
        match epaper::display_qr(&ticket_str) {
            Ok(()) => {
                info!("QR code displayed on e-paper");
                true
            }
            Err(e) => {
                warn!(
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

    let refresh_ticket = ticket_str.clone();
    let refresh_handle = if has_epaper {
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(EPAPER_REFRESH_INTERVAL).await;
                match epaper::display_qr(&refresh_ticket) {
                    Ok(()) => debug!("periodic e-paper refresh complete"),
                    Err(e) => {
                        warn!(error = format!("{e:#}"), "periodic e-paper refresh failed")
                    }
                }
            }
        }))
    } else {
        None
    };

    tokio::signal::ctrl_c().await?;

    if let Some(handle) = refresh_handle {
        handle.abort();
    }

    // The datasheet asks to clear the display before storage.
    if has_epaper {
        match epaper::clear_display() {
            Ok(()) => info!("e-paper cleared for storage"),
            Err(e) => warn!(error = format!("{e:#}"), "could not clear e-paper on exit"),
        }
    }

    broadcast.close();
    broadcast.closed().await;
    live.shutdown().await;

    Ok(())
}
