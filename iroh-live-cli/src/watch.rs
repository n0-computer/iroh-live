//! `irl watch` — subscribe to a remote broadcast and play it.
//!
//! The window draws the decoded video and the playback engine takes the audio.
//! Unless `--rendition` pins one, the video track follows the downlink: the
//! subscription's transport signals drive `moq-media`'s adaptation, which swaps
//! renditions without the picture going blank.

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
        window::run(live, sub, tracks, args.fullscreen)
    }
    #[cfg(not(feature = "render"))]
    {
        Err(n0_error::anyerr!(
            "watching video needs the 'render' feature; use --no-video for audio only"
        ))
    }
}

/// Connects, opens the tracks this run will actually play, and decides how the
/// video track picks its rendition.
async fn subscribe(
    ticket: &iroh_live::ticket::LiveTicket,
    args: &WatchArgs,
) -> Result<(Live, Subscription, MediaTracks)> {
    println!("connecting to {ticket} ...");
    let live = setup_live(false).await?;
    let sub = live
        .subscribe(ticket.endpoint.clone(), &ticket.broadcast_name)
        .await?;
    info!("session established");

    // `--no-video` opens audio alone rather than opening video and discarding
    // it: a decoder nobody draws from still costs a core.
    let tracks = match args.no_video {
        true => audio_only(&sub).await,
        false => sub.media().await,
    };

    if let Some(video) = &tracks.video {
        match &args.rendition {
            Some(name) => {
                video.set_rendition(name.clone());
                info!(rendition = %name, "video pinned");
            }
            None => {
                video.enable_adaptation(sub.signals().clone());
                info!(
                    rendition = video.rendition(),
                    "video following the downlink"
                );
            }
        }
    }
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
    use iroh_live::{
        Live, Subscription,
        media::subscribe::{AudioTrack, MediaTracks, RemoteBroadcast},
    };
    use moq_media_egui::{
        VideoTrackView,
        overlay::{DebugOverlay, StatCategory},
    };
    use n0_error::{Result, anyerr};
    use tracing::info;

    use crate::ui::CursorIdle;

    /// The rendition the user asked for, as distinct from the one currently
    /// decoding.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RenditionChoice {
        /// Follow the downlink.
        Auto,
        /// Hold one rendition whatever the downlink does.
        Pinned(String),
    }

    /// Opens the player window and runs it until it closes.
    pub(super) fn run(
        live: Live,
        sub: Subscription,
        tracks: MediaTracks,
        fullscreen: bool,
    ) -> Result {
        let options = eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: moq_media_egui::create_egui_wgpu_config(),
            viewport: egui::ViewportBuilder::default().with_fullscreen(fullscreen),
            ..Default::default()
        };

        eframe::run_native(
            "irl watch",
            options,
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);
                let broadcast = sub.broadcast().clone();
                let MediaTracks { video, audio } = tracks;
                let view = video.map(|track| {
                    VideoTrackView::new_wgpu(
                        &cc.egui_ctx,
                        "video",
                        track,
                        cc.wgpu_render_state.as_ref(),
                    )
                });
                Ok(Box::new(WatchApp {
                    title: broadcast.name().to_string(),
                    live,
                    sub,
                    broadcast,
                    view,
                    audio,
                    overlay: DebugOverlay::new(&[
                        StatCategory::Net,
                        StatCategory::Render,
                        StatCategory::Time,
                    ]),
                    cursor: CursorIdle::default(),
                    choice: RenditionChoice::Auto,
                    #[cfg(feature = "playback")]
                    volume: 1.0,
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    struct WatchApp {
        title: String,
        live: Live,
        sub: Subscription,
        broadcast: RemoteBroadcast,
        view: Option<VideoTrackView>,
        audio: Option<AudioTrack>,
        overlay: DebugOverlay,
        cursor: CursorIdle,
        choice: RenditionChoice,
        /// The output gain the slider last set. Only the playback build has a
        /// sink to apply it to.
        #[cfg(feature = "playback")]
        volume: f32,
    }

    impl eframe::App for WatchApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            ctx.request_repaint_after(Duration::from_millis(16));
            let show_overlay = self.cursor.update(&ctx, self.overlay.any_expanded());

            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let available = ui.available_size();
            let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            if let Some(view) = self.view.as_mut() {
                let (image, _) = view.render(&ctx, available);
                ui.add_sized(available, image);
            }

            if !show_overlay {
                return;
            }
            crate::ui::top_bar(ui, &ctx, &self.title);
            if let Some(view) = self.view.as_ref() {
                self.overlay
                    .update_from_track(self.broadcast.stats(), view.track());
            }
            self.overlay.show(ui, video_rect, self.broadcast.stats());
            crate::ui::control_panel(&ctx, "watch-controls", |ui| self.controls(ui));
        }

        fn on_exit(&mut self) {
            info!("exit");
            self.view = None;
            self.audio = None;
            self.broadcast.shutdown();
            self.sub.session().close(moq_net::Error::Cancel);
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }

    impl WatchApp {
        /// Draws the rendition picker and the volume slider.
        fn controls(&mut self, ui: &mut egui::Ui) {
            let Some(view) = self.view.as_ref() else {
                ui.label("no video");
                return;
            };
            let track = view.track();

            ui.label("Rendition");
            let label = match &self.choice {
                RenditionChoice::Auto => format!("Auto ({})", track.rendition()),
                RenditionChoice::Pinned(name) => name.clone(),
            };
            egui::ComboBox::from_id_salt("watch-rendition")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.choice == RenditionChoice::Auto, "Auto")
                        .clicked()
                    {
                        self.choice = RenditionChoice::Auto;
                        track.enable_adaptation(self.sub.signals().clone());
                        info!("following the downlink");
                    }
                    for name in self.broadcast.catalog().video().keys() {
                        let pinned = self.choice == RenditionChoice::Pinned(name.clone());
                        if ui.selectable_label(pinned, name).clicked() {
                            self.choice = RenditionChoice::Pinned(name.clone());
                            track.disable_adaptation();
                            track.set_rendition(name.clone());
                            info!(rendition = %name, "rendition pinned");
                        }
                    }
                });

            #[cfg(feature = "playback")]
            if let Some(audio) = self.audio.as_ref() {
                ui.label("Volume");
                if ui
                    .add(egui::Slider::new(&mut self.volume, 0.0..=2.0).show_value(false))
                    .changed()
                {
                    audio.set_volume(self.volume);
                }
            }
        }
    }
}
