//! `irl play` — subscribe and render a remote broadcast.

use std::time::Duration;

use eframe::egui;
use iroh_live::{
    Live, Subscription,
    media::{AudioBackend, format::PlaybackConfig},
};
use moq_media_egui::{create_egui_wgpu_config, overlay::StatCategory};
use n0_error::anyerr;
use tracing::info;

use crate::{
    args::{PlayArgs, SubscribeSource},
    transport::setup_live,
    ui::{MouseHide, RemoteControls},
};

/// Entry point — does async setup inside block_on, then runs eframe outside it.
pub fn run(args: PlayArgs, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    let source = args.source()?;
    let backend = args.decoder_backend()?;
    let render_mode = args.render_mode()?;
    let no_video = args.no_video;

    let (live, sub, track, audio_ctx, broadcast_name, signals) = rt.block_on(async {
        let audio_ctx = AudioBackend::default();
        let live = setup_live(false).await?;
        let playback_config = PlaybackConfig {
            backend,
            ..Default::default()
        };

        let (sub, broadcast_name) = match source {
            SubscribeSource::Direct(ticket) => {
                println!("connecting to {ticket} ...");
                let sub = live.subscribe_ticket(&ticket);
                (sub, ticket.broadcast_name)
            }
            SubscribeSource::Relay {
                target,
                broadcast_name,
            } => {
                println!(
                    "connecting via relay {} ({}) ...",
                    target.endpoint(),
                    broadcast_name
                );
                let sub = live.subscribe(target, broadcast_name.clone());
                (sub, broadcast_name)
            }
        };
        let active = sub.ready().await?;
        info!("session established");
        let track = active
            .broadcast()
            .media(&audio_ctx, playback_config)
            .await?;
        info!("media tracks subscribed");

        let signals = active.signals().clone();
        n0_error::Ok((live, sub, track, audio_ctx, broadcast_name, signals))
    })?;

    if no_video {
        // Audio-only: block on ctrl-c, then clean up.
        rt.block_on(async {
            let _audio = track.audio;
            let _broadcast = track.broadcast;
            tokio::signal::ctrl_c().await?;
            if let Some(active) = sub.active().await {
                active.session().close(0, b"bye");
            }
            live.shutdown().await;
            n0_error::Ok(())
        })
    } else {
        // GUI: eframe runs on the main thread, tokio workers stay alive via rt.enter().
        let _guard = rt.enter();
        run_egui(
            live,
            sub,
            track,
            audio_ctx,
            broadcast_name,
            render_mode,
            signals,
        )
    }
}

fn run_egui(
    live: Live,
    sub: Subscription,
    track: iroh_live::media::subscribe::MediaTracks,
    audio_ctx: AudioBackend,
    broadcast_name: String,
    render_mode: crate::args::RenderMode,
    signals: tokio::sync::watch::Receiver<moq_media::net::NetworkSignals>,
) -> n0_error::Result {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: create_egui_wgpu_config(),
        ..Default::default()
    };

    eframe::run_native(
        "irl - play",
        native_options,
        Box::new(move |cc| {
            crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);

            let signals = Some(signals.clone());
            let use_wgpu = render_mode == crate::args::RenderMode::Auto;
            let remote = RemoteControls::with_render_mode(
                track.broadcast,
                track.video,
                track.audio,
                audio_ctx,
                signals,
                &cc.egui_ctx,
                "video",
                &[StatCategory::Net, StatCategory::Render, StatCategory::Time],
                cc.wgpu_render_state.clone(),
                use_wgpu,
            );

            Ok(Box::new(PlayApp {
                remote,
                sub,
                live,
                mouse: MouseHide::default(),
                broadcast_name,
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

struct PlayApp {
    remote: RemoteControls,
    sub: Subscription,
    live: Live,
    mouse: MouseHide,
    broadcast_name: String,
}

impl eframe::App for PlayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        let overlay_expanded = self.remote.overlay.any_expanded();
        let show_overlay = self.mouse.update(ctx, overlay_expanded);

        if self.remote.video_closed() {
            info!("video track closed, resubscribing");
            self.remote.resubscribe(ctx, "video");
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                let avail = ui.available_size();
                let video_rect = egui::Rect::from_min_size(ui.cursor().min, avail);
                if let Some(video) = self.remote.video.as_mut() {
                    let (img, _) = video.render(ctx, avail);
                    ui.add_sized(avail, img);
                }

                if show_overlay {
                    crate::ui::top_bar(ui, ctx, &self.broadcast_name);

                    self.remote.update_overlay();
                    self.remote
                        .overlay
                        .show(ui, video_rect, self.remote.broadcast.stats());

                    egui::Area::new(egui::Id::new("play-controls"))
                        .anchor(egui::Align2::LEFT_TOP, [8.0, 28.0])
                        .order(egui::Order::Foreground)
                        .show(ctx, |ui| {
                            egui::Frame::new()
                                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180))
                                .corner_radius(3.0)
                                .inner_margin(6.0)
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        self.remote.controls_ui(ui, ctx, "play");
                                    });
                                });
                        });
                }
            });
    }

    fn on_exit(&mut self) {
        info!("exit");
        self.remote.broadcast.shutdown();
        let sub = self.sub.clone();
        tokio::spawn(async move {
            if let Some(active) = sub.active().await {
                active.session().close(0, b"bye");
            }
        });
        crate::ui::shutdown_live_blocking(&self.live);
    }
}
