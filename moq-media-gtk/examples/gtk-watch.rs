use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::Parser;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow};
use iroh::Endpoint;
use iroh_live::{
    Live,
    media::{audio_backend::AudioBackend, format::PlaybackConfig, subscribe::VideoTrack},
    ticket::LiveTicket,
};
use moq_media_gtk::GlVideoWidget;

#[derive(Debug, Parser)]
#[clap(about = "GTK4 video viewer for iroh-live (GPU-accelerated)")]
struct Cli {
    ticket: LiveTicket,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let audio_ctx = AudioBackend::default();

    println!("connecting to {} ...", cli.ticket);
    // Keep `live` alive — it owns the iroh Endpoint. Dropping it tears
    // down the QUIC connection and all subscriptions.
    let (_live, video_track) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        let ticket = cli.ticket.clone();
        async move {
            let endpoint = Endpoint::bind(iroh::endpoint::presets::N0).await?;
            let live = Live::new(endpoint);
            let (_session, track) = live
                .subscribe_media(
                    ticket.endpoint,
                    &ticket.broadcast_name,
                    &audio_ctx,
                    PlaybackConfig::default(),
                )
                .await?;
            let video = track.video.context("broadcast has no video track")?;
            println!("connected");
            anyhow::Ok((live, video))
        }
    })?;

    let _guard = rt.enter();

    let app = Application::builder()
        .application_id("live.iroh.gtk-watch")
        .build();

    // VideoTrack is not Clone; move it into the activate callback (runs once).
    let video_track = std::cell::RefCell::new(Some(video_track));
    app.connect_activate(move |app| {
        let track = video_track
            .borrow_mut()
            .take()
            .expect("activate called twice");
        build_ui(app, track);
    });

    // GTK takes over the main loop. Pass empty args since clap already parsed.
    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn build_ui(app: &Application, mut video_track: VideoTrack) {
    let widget = GlVideoWidget::new();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("iroh-live GTK viewer")
        .default_width(960)
        .default_height(540)
        .child(widget.gl_area())
        .build();

    window.present();

    // Poll for new frames at ~60 fps using non-blocking try_recv.
    let mut last_ts: Option<Duration> = None;
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let frame = video_track.try_recv();
        let is_new = frame
            .as_ref()
            .is_some_and(|f| last_ts.is_none_or(|prev| f.timestamp != prev));

        if is_new && let Some(f) = &frame {
            last_ts = Some(f.timestamp);
            widget.upload_frame(f);
        }

        glib::ControlFlow::Continue
    });
}
