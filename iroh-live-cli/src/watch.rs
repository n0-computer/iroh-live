//! `irl watch` — subscribe to a remote broadcast and play it.
//!
//! The window draws the decoded video and the playback engine takes the audio.
//! Unless `--rendition` pins one, the video track follows the downlink: the
//! subscription's transport signals drive `moq-media`'s adaptation, which swaps
//! renditions without the picture going blank.
//!
//! Everything decoded here is drawn and nothing here reads a pixel, so the
//! subscription asks for GPU-resident frames: a hardware decoder that can share
//! its decode surface hands one over and the renderer imports it, which is a
//! full-frame copy per frame that never happens. `irl record` is the other side
//! of that choice and does not ask, since it never decodes at all.

use iroh_live::{Live, Subscription, media::subscribe::MediaTracks};
use n0_error::Result;
use tracing::info;

use crate::{args::WatchArgs, transport::setup_live};

/// Runs the `watch` command.
pub fn run(args: WatchArgs, rt: &tokio::runtime::Runtime) -> Result {
    let ticket = args.ticket()?;

    let (live, sub, tracks) = rt.block_on(subscribe(&ticket, &args))?;

    if args.no_video {
        return wait_for_ctrl_c(rt, live, sub, tracks);
    }

    #[cfg(feature = "render")]
    {
        // eframe takes the main thread from here on, so the runtime keeps its
        // workers only for as long as this guard lives.
        let _guard = rt.enter();
        window::run(live, sub, tracks, &args)
    }
    #[cfg(not(feature = "render"))]
    {
        Err(n0_error::anyerr!(
            "watching video needs the 'render' feature; use --no-video for audio only"
        ))
    }
}

/// Connects and opens the tracks this run will actually play.
async fn subscribe(
    ticket: &iroh_live::ticket::LiveTicket,
    args: &WatchArgs,
) -> Result<(Live, Subscription, MediaTracks)> {
    // Opening the engine before the first sink is what makes `--audio-output`
    // take effect: a sink built against the default device would already be
    // playing there by the time a switch could move it.
    #[cfg(feature = "playback")]
    if let Some(device) = args.audio_output.clone() {
        let mut config = iroh_live::media::audio::playback::Config::default();
        config.device = Some(device.clone());
        iroh_live::media::playback::open(config)
            .await
            .map_err(|err| n0_error::anyerr!("audio output {device:?}: {err}"))?;
        info!(device = %device, "audio output selected");
    }

    println!("connecting to {ticket} ...");
    let live = setup_live(false).await?;
    let sub = live
        .subscribe(ticket.endpoint.clone(), &ticket.broadcast_name)
        .await?;
    info!("session established");

    // Before the video track opens, since the policy is read when a decoder is
    // built rather than watched afterwards.
    if !args.no_video {
        let broadcast = sub.broadcast();
        broadcast.set_playback_policy(broadcast.playback_policy().with_gpu_frames(true));
    }

    // `--no-video` opens audio alone rather than opening video and discarding
    // it: a decoder nobody draws from still costs a core.
    let tracks = match args.no_video {
        true => audio_only(&sub).await,
        false => sub.media().await,
    };
    Ok((live, sub, tracks))
}

/// Opens the audio track alone, for `--no-video`.
// A build without `playback` has no sink to open, so nothing here awaits.
#[allow(
    clippy::unused_async,
    reason = "one arm of a feature-gated body awaits"
)]
async fn audio_only(sub: &Subscription) -> MediaTracks {
    #[cfg(feature = "playback")]
    {
        let broadcast = sub.broadcast();
        if !broadcast.has_audio() {
            return MediaTracks::default();
        }
        let audio = broadcast
            .audio()
            .await
            .inspect_err(|err| tracing::warn!(error = %err, "audio track failed to open"))
            .ok();
        MediaTracks { video: None, audio }
    }
    #[cfg(not(feature = "playback"))]
    {
        let _ = sub;
        MediaTracks::default()
    }
}

/// Plays until the user interrupts, with no window.
fn wait_for_ctrl_c(
    rt: &tokio::runtime::Runtime,
    live: Live,
    sub: Subscription,
    tracks: MediaTracks,
) -> Result {
    println!("playing, press Ctrl+C to stop");
    rt.block_on(async move {
        tokio::signal::ctrl_c().await?;
        drop(tracks);
        sub.broadcast().shutdown();
        sub.session().close(moq_net::Error::Cancel);
        live.shutdown().await;
        Ok(())
    })
}

#[cfg(feature = "render")]
mod window {
    //! The player window: the picture, the stats overlay, and the controls that
    //! choose a rendition and set the volume.

    use std::time::Duration;

    use eframe::egui;
    use iroh_live::{Live, Subscription, media::subscribe::MediaTracks};
    use n0_error::{Result, anyerr};
    use tracing::info;

    use crate::{
        args::WatchArgs,
        ui::{CursorIdle, RemoteView, RenditionChoice},
    };

    /// Opens the player window and runs it until it closes.
    pub(super) fn run(
        live: Live,
        sub: Subscription,
        tracks: MediaTracks,
        args: &WatchArgs,
    ) -> Result {
        let rendition = args.rendition.clone();
        eframe::run_native(
            "irl watch",
            crate::ui::native_options(args.fullscreen),
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);
                let broadcast = sub.broadcast().clone();
                let title = broadcast.name().to_string();
                let mut remote = RemoteView::new(
                    &cc.egui_ctx,
                    "video",
                    broadcast,
                    tracks,
                    sub.signals().clone(),
                    cc.wgpu_render_state.as_ref(),
                );
                if let Some(name) = rendition {
                    remote.set_rendition(RenditionChoice::Pinned(name));
                }
                Ok(Box::new(WatchApp {
                    title,
                    live,
                    sub,
                    remote,
                    cursor: CursorIdle::default(),
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    struct WatchApp {
        title: String,
        live: Live,
        sub: Subscription,
        remote: RemoteView,
        cursor: CursorIdle,
    }

    impl eframe::App for WatchApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            ctx.request_repaint_after(Duration::from_millis(16));
            let show_overlay = self.cursor.update(&ctx, self.remote.overlay_expanded());

            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let available = ui.available_size();
            let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            self.remote.draw(ui, available);

            if !show_overlay {
                return;
            }
            crate::ui::top_bar(ui, &ctx, &self.title);
            self.remote.draw_overlay(ui, video_rect);
            crate::ui::control_panel(&ctx, "watch-controls", |ui| {
                self.remote.controls(ui, "watch");
            });
        }

        fn on_exit(&mut self) {
            info!("exit");
            self.remote.shutdown();
            self.sub.session().close(moq_net::Error::Cancel);
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }
}
