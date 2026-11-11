//! The `publish` command, which streams the camera's hardware H.264 over iroh.
//!
//! The encoder output is read through `rpicam-vid`.

use std::time::Duration;

use clap::Parser;
use iroh::EndpointId;
use iroh_live::{
    BoundNode, EndpointConfig, RelayConfig,
    layout::LiveLayout,
    media::{EncodedVideoSource, LocalBroadcast, Rate, RpicamConfig, video::Size},
};
use n0_error::StdResultExt;
use n0_future::task::AbortOnDropHandle;
use tracing::{debug, info, warn};

use crate::epaper;

/// How often to redraw the QR code.
///
/// The datasheet asks for a refresh at least every 24 h and at most every 180 s.
const EPAPER_REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

/// Options of the `publish` subcommand.
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
    // The router in it accepts the viewers that dial the Pi directly.
    let bound = BoundNode::bind(
        EndpointConfig::from_env()?,
        LiveLayout::default().config(&[]),
    )
    .await?;
    let node = bound.node();

    // Published while `broadcast` lives.
    let broadcast: LocalBroadcast = LiveLayout::default().publish(node, opts.name.as_str())?;

    // Keeps the default of one keyframe per second.
    let config = RpicamConfig {
        bitrate: Rate::from_bps(u64::from(opts.bitrate)),
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

    // The link redials the relay if the session drops. It only pushes: the
    // relay's routes stay out of the Pi's route table.
    let _relay = match opts.relay {
        Some(relay) => {
            info!(%relay, "pushing to relay");
            Some(node.attach_relay(RelayConfig {
                consume: false,
                ..RelayConfig::iroh(relay)
            })?)
        }
        None => None,
    };

    let ticket = LiveLayout::default().ticket(node.id(), &opts.name)?;
    let ticket_str = ticket.to_string();
    println!("publishing at {ticket_str}");

    // The e-paper is optional: a failure only logs a warning. Its SPI and
    // BUSY waits block for seconds, so they run off the async workers.
    let has_epaper = if opts.epaper {
        let ticket = ticket_str.clone();
        match tokio::task::spawn_blocking(move || epaper::display_qr(&ticket))
            .await
            .std_context("the e-paper task panicked")?
        {
            Ok(()) => {
                info!("QR code displayed on e-paper");
                true
            }
            Err(e) => {
                warn!(
                    error = format_args!("{e:#}"),
                    "could not display QR on e-paper - is the HAT attached and SPI enabled? \
                     (the stream is publishing normally, use the ticket above to connect)"
                );
                false
            }
        }
    } else {
        false
    };

    let refresh = has_epaper.then(|| {
        let ticket = ticket_str.clone();
        AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                tokio::time::sleep(EPAPER_REFRESH_INTERVAL).await;
                let ticket = ticket.clone();
                match tokio::task::spawn_blocking(move || epaper::display_qr(&ticket)).await {
                    Ok(Ok(())) => debug!("periodic e-paper refresh complete"),
                    Ok(Err(e)) => {
                        warn!(
                            error = format_args!("{e:#}"),
                            "periodic e-paper refresh failed"
                        )
                    }
                    Err(e) => warn!(error = %e, "periodic e-paper refresh panicked"),
                }
            }
        }))
    });

    tokio::signal::ctrl_c().await?;

    // A refresh that already runs on the blocking pool finishes on its own.
    drop(refresh);

    // The datasheet asks to clear the display before storage.
    if has_epaper {
        match tokio::task::spawn_blocking(epaper::clear_display).await {
            Ok(Ok(())) => info!("e-paper cleared for storage"),
            Ok(Err(e)) => warn!(
                error = format_args!("{e:#}"),
                "could not clear e-paper on exit"
            ),
            Err(e) => warn!(error = %e, "clearing the e-paper panicked"),
        }
    }

    broadcast.close();
    broadcast.closed().await;
    // Also closes the endpoint.
    bound.shutdown().await;
    Ok(())
}
