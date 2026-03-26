//! `irl play` — subscribe and render a remote broadcast.

use std::time::Duration;

use eframe::egui;
use iroh_live::{
    Live, Subscription,
    media::{
        AudioBackend,
        format::{DecodeConfig, PlaybackConfig},
    },
};
use moq_media_egui::{create_egui_wgpu_config, overlay::StatCategory};
use n0_error::anyerr;
use tracing::info;

use crate::{
    args::PlayArgs,
    transport::setup_live,
    ui::{MouseHide, RemoteControls},
};

/// Entry point — does async setup inside block_on, then runs eframe outside it.
pub fn run(args: PlayArgs, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    let ticket = args.ticket()?;
    let backend = args.decoder_backend()?;
    let no_video = args.no_video;

    let (live, sub, track, audio_ctx, broadcast_name) = rt.block_on(async {
        let audio_ctx = AudioBackend::default();

        println!("connecting to {ticket} ...");
        let live = setup_live(false).await?;
        let playback_config = PlaybackConfig {
            decode_config: DecodeConfig {
                backend,
                ..Default::default()
            },
            ..Default::default()
        };

        let sub = live
            .subscribe_with_stats(ticket.endpoint, &ticket.broadcast_name)
            .await?;
        info!("session established");
        let track = sub.broadcast().media(&audio_ctx, playback_config).await?;
        info!("media tracks subscribed");

        n0_error::Ok((live, sub, track, audio_ctx, ticket.broadcast_name))
    })?;

    if no_video {
        // Audio-only: block on ctrl-c, then clean up.
        rt.block_on(async {
            let _audio = track.audio;
            let _broadcast = track.broadcast;
            tokio::signal::ctrl_c().await?;
            sub.session().close(0, b"bye");
            live.shutdown().await;
            n0_error::Ok(())
        })
    } else {
        // GUI: eframe runs on the main thread, tokio workers stay alive via rt.enter().
        let _guard = rt.enter();
        run_egui(live, sub, track, audio_ctx, broadcast_name)
    }
}

fn run_egui(
    live: Live,
    sub: Subscription,
    track: iroh_live::media::subscribe::MediaTracks,
    audio_ctx: AudioBackend,
    broadcast_name: String,
) -> n0_error::Result {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: create_egui_wgpu_config(),
        ..Default::default()
    };

    eframe::run_native(
        "irl — play",
        native_options,
        Box::new(move |cc| {
            crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);

            let signals = sub.signals().clone();
            let remote = RemoteControls::new(
                track.broadcast,
                track.video,
                track.audio,
                audio_ctx,
                signals,
                &cc.egui_ctx,
                "video",
                &[StatCategory::Net, StatCategory::Render, StatCategory::Time],
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
                        .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -8.0])
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
        self.sub.session().close(0, b"bye");
        crate::ui::shutdown_live_blocking(&self.live);
    }
}
