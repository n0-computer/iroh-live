/// Publish command: capture camera, encode, and stream over iroh.
use std::time::Duration;

use clap::Parser;
use iroh::{Endpoint, EndpointId};
use iroh_live::{
    Live,
    media::{
        codec::VideoCodec,
        format::VideoPreset,
        publish::{LocalBroadcast, VideoInput},
    },
    ticket::LiveTicket,
};

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

    /// Encoder backend.
    ///
    /// - "libcamera" (default): rpicam-vid's internal H.264 HW encoder
    ///   (pre-encoded path, lowest CPU, ~300 KB/s at 360p)
    /// - "software": raw YUV capture + openh264 software encoder
    /// - "v4l2": raw YUV capture + V4L2 M2M hardware encoder
    /// - "test": SMPTE test pattern (no camera needed, for e2e verification)
    #[clap(long, default_value = "libcamera")]
    pub encoder: String,

    /// Video rendition presets (comma-separated).
    ///
    /// Only used with software/v4l2 encoders (not libcamera, which
    /// produces a single pre-encoded stream at the configured resolution).
    #[clap(long, value_delimiter = ',', default_values_t = [VideoPreset::P360])]
    pub video_presets: Vec<VideoPreset>,

    /// Relay's iroh endpoint ID — additionally publishes to the relay so
    /// browser and non-P2P clients can subscribe.
    #[clap(long)]
    pub relay: Option<EndpointId>,

    /// Broadcast name.
    #[clap(long, default_value = "pi-zero")]
    pub name: String,

    /// Target bitrate for libcamera encoder (bits/s).
    #[clap(long, default_value = "500000")]
    pub bitrate: u32,

    /// Capture framerate.
    #[clap(long, default_value = "30")]
    pub fps: u32,
}

/// Publishes the camera stream and shows the ticket QR on e-paper.
pub(crate) async fn cmd_publish(opts: PublishOpts) -> n0_error::Result {
    // --- iroh endpoint ---
    let secret_key = iroh_live::util::secret_key_from_env()?;
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key)
        .bind()
        .await?;
    let live = Live::builder(endpoint).with_router().spawn();

    // --- media broadcast ---
    let broadcast = LocalBroadcast::new();

    // Resolve the capture resolution from the highest requested preset.
    let max_preset = opts
        .video_presets
        .iter()
        .copied()
        .max()
        .unwrap_or(VideoPreset::P360);
    let (capture_w, capture_h) = preset_resolution(max_preset);

    setup_encoder(&opts, &broadcast, capture_w, capture_h)?;

    // Publish under the chosen name.
    live.publish(&opts.name, &broadcast).await?;

    // --- relay (optional) ---
    if let Some(relay_id) = opts.relay {
        let session = live.transport().connect(relay_id).await?;
        session.publish(&opts.name, broadcast.producer().consume());
        tracing::info!(%relay_id, "published to relay");
    }

    // --- ticket (always printed, regardless of e-paper) ---
    let ticket = LiveTicket::new(live.endpoint().addr(), &opts.name);
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

/// Configures the video encoder on `broadcast` based on CLI options.
fn setup_encoder(
    opts: &PublishOpts,
    broadcast: &LocalBroadcast,
    capture_w: u32,
    capture_h: u32,
) -> n0_error::Result {
    match opts.encoder.as_str() {
        "libcamera" | "hw" | "hardware" => {
            use rusty_capture::{LibcameraH264Config, LibcameraH264Source};

            let config =
                LibcameraH264Config::new(capture_w, capture_h, opts.fps).with_bitrate(opts.bitrate);
            let track_name = config.track_name();
            let video_config = config.video_config();
            tracing::info!(
                width = capture_w,
                height = capture_h,
                fps = opts.fps,
                bitrate = opts.bitrate,
                "using pre-encoded H.264 from rpicam-vid"
            );
            broadcast.video().set(VideoInput::pre_encoded(
                track_name,
                video_config,
                move || Ok(Box::new(LibcameraH264Source::new(config.clone()))),
            ))?;
        }
        "software" | "sw" => {
            use rusty_capture::{LibcameraCapturer, LibcameraConfig};

            let camera = LibcameraCapturer::new(LibcameraConfig {
                width: capture_w,
                height: capture_h,
                framerate: opts.fps,
            });
            let codec = VideoCodec::H264;
            tracing::info!(%codec, presets = ?opts.video_presets, "software encoder + raw YUV capture");
            broadcast
                .video()
                .set(VideoInput::new(camera, codec, opts.video_presets.clone()))?;
        }
        "v4l2" => {
            use rusty_capture::{LibcameraCapturer, LibcameraConfig};

            let camera = LibcameraCapturer::new(LibcameraConfig {
                width: capture_w,
                height: capture_h,
                framerate: opts.fps,
            });
            let codec = VideoCodec::V4l2H264;
            tracing::info!(%codec, presets = ?opts.video_presets, "V4L2 M2M encoder + raw YUV capture");
            broadcast
                .video()
                .set(VideoInput::new(camera, codec, opts.video_presets.clone()))?;
        }
        "ffmpeg" => {
            eprintln!("ffmpeg encoder has been removed from rusty-codecs");
            std::process::exit(1);
        }
        "test" | "test-pattern" => {
            use moq_media::test_util::TestVideoSource;

            let source = TestVideoSource::new(capture_w, capture_h).with_fps(opts.fps as f64);
            let codec = VideoCodec::H264;
            tracing::info!(%codec, presets = ?opts.video_presets, "test pattern + software encoder");
            broadcast
                .video()
                .set(VideoInput::new(source, codec, opts.video_presets.clone()))?;
        }
        "test-v4l2" => {
            use moq_media::test_util::TestVideoSource;

            let source = TestVideoSource::new(capture_w, capture_h).with_fps(opts.fps as f64);
            let codec = VideoCodec::V4l2H264;
            tracing::info!(%codec, presets = ?opts.video_presets, "test pattern + V4L2 HW encoder");
            broadcast
                .video()
                .set(VideoInput::new(source, codec, opts.video_presets.clone()))?;
        }
        other => {
            eprintln!(
                "Unknown encoder: {other}. \
                 Use 'libcamera' (default), 'software', 'v4l2', 'test', 'test-v4l2', or 'ffmpeg'"
            );
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Returns (width, height) for a given `VideoPreset`.
fn preset_resolution(preset: VideoPreset) -> (u32, u32) {
    match preset {
        VideoPreset::P180 => (320, 180),
        VideoPreset::P360 => (640, 360),
        VideoPreset::P720 => (1280, 720),
        VideoPreset::P1080 => (1920, 1080),
    }
}
