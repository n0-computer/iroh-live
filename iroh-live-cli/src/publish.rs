//! `irl publish` — publish capture or file sources over iroh.
//!
//! Sources are specified via `--video` and `--audio` flags. Capture sources
//! (cam, screen, test) go through the encode pipeline; file sources
//! (`file:<path>`) go through the fmp4/avc3 import pipeline.
//!
//! Transport: serve by default, `--relay` pushes to relay, `--room` publishes
//! into a room, `--no-serve` disables incoming connections.

use iroh_live::media::publish::LocalBroadcast;
use moq_lite::BroadcastProducer;

use crate::{
    args::PublishArgs,
    import::{init_import, open_input, run_import},
    transport::{publish_producer, setup_live},
};

pub fn run(args: PublishArgs, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    let file_path = args
        .capture
        .file_video_source()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if let Some(path) = file_path {
        run_file(path, &args, rt)
    } else {
        run_capture(&args, rt)
    }
}

fn run_capture(args: &PublishArgs, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    let capture = &args.capture;
    let (live, broadcast, audio_ctx, _room) = rt.block_on(async {
        let video_sources = capture
            .video_sources()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let audio_sources = capture
            .audio_sources()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let codec = capture.video_codec()?;
        let presets = capture.presets()?;
        let audio_preset = capture.audio_preset_parsed()?;

        let live = setup_live(!args.transport.no_serve).await?;
        let broadcast = LocalBroadcast::new();
        let audio_ctx = iroh_live::media::AudioBackend::default();

        crate::source::setup_video(&broadcast, &video_sources, codec, &presets)?;
        crate::source::setup_audio(&broadcast, &audio_sources, &audio_ctx, audio_preset).await?;
        let room = crate::transport::publish_broadcast(&live, &broadcast, &args.transport).await?;

        anyhow::Ok((live, broadcast, audio_ctx, room))
    })?;

    if args.preview {
        #[cfg(feature = "wgpu")]
        {
            let ticket =
                iroh_live::ticket::LiveTicket::new(live.endpoint().addr(), &args.transport.name);
            let _guard = rt.enter();
            run_capture_preview(live, broadcast, audio_ctx, _room, ticket.to_string())
        }
        #[cfg(not(feature = "wgpu"))]
        {
            let _ = (live, broadcast, audio_ctx, _room);
            n0_error::bail_any!("--preview requires the 'wgpu' feature");
        }
    } else {
        println!("press Ctrl+C to stop");
        rt.block_on(async {
            // Keep room alive until shutdown so the room actor keeps running.
            let _room = _room;
            tokio::signal::ctrl_c().await?;
            live.shutdown().await;
            n0_error::Ok(())
        })
    }
}

fn run_file(
    path: std::path::PathBuf,
    args: &PublishArgs,
    rt: &tokio::runtime::Runtime,
) -> n0_error::Result {
    let (live, decoder, input, preview_consumer, _room) = rt.block_on(async {
        let mut input = open_input(&Some(path), args.transcode, args.format).await?;
        let live = setup_live(!args.transport.no_serve).await?;
        let mut broadcast = BroadcastProducer::default();
        let preview_consumer = broadcast.consume();
        let decoder = init_import(&mut broadcast, args.format, &mut input).await?;
        let room = publish_producer(&live, broadcast, &args.transport).await?;
        anyhow::Ok((live, decoder, input, preview_consumer, room))
    })?;

    if args.preview {
        #[cfg(feature = "wgpu")]
        {
            let (tracks, import_task, ticket_str) = rt.block_on(async {
                let audio_ctx = iroh_live::media::AudioBackend::default();
                let tracks = iroh_live::media::subscribe::subscribe_preview(
                    preview_consumer,
                    &audio_ctx,
                    iroh_live::media::format::PlaybackConfig::default(),
                )
                .await?;
                let import_task = tokio::spawn(run_import(decoder, input));
                let ticket = iroh_live::ticket::LiveTicket::new(
                    live.endpoint().addr(),
                    &args.transport.name,
                );
                anyhow::Ok((tracks, import_task, ticket.to_string()))
            })?;

            let _guard = rt.enter();
            run_file_preview(live, _room, tracks, import_task, ticket_str)
        }
        #[cfg(not(feature = "wgpu"))]
        {
            let _ = (live, decoder, input, preview_consumer, _room);
            n0_error::bail_any!("--preview requires the 'wgpu' feature");
        }
    } else {
        println!("press Ctrl+C to stop");
        rt.block_on(async move {
            // Keep room alive until shutdown so the room actor keeps running.
            let _room = _room;
            tokio::select! {
                res = run_import(decoder, input) => res?,
                _ = tokio::signal::ctrl_c() => {}
            }
            live.shutdown().await;
            n0_error::Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Preview windows (wgpu only)
// ---------------------------------------------------------------------------

#[cfg(feature = "wgpu")]
mod preview {
    use std::time::Duration;

    use eframe::egui;
    use iroh_live::{
        Live,
        media::{AudioBackend, publish::LocalBroadcast, subscribe::MediaTracks},
        rooms::Room,
    };
    use moq_media_egui::{
        VideoTrackView,
        overlay::{DebugOverlay, StatCategory, fit_to_aspect},
    };
    use n0_error::anyerr;
    use tracing::info;

    // -- File preview --

    pub(super) fn run_file_preview(
        live: Live,
        room: Option<Room>,
        tracks: MediaTracks,
        import_task: tokio::task::JoinHandle<anyhow::Result<()>>,
        ticket_str: String,
    ) -> n0_error::Result {
        let rt = tokio::runtime::Handle::current();

        eframe::run_native(
            "irl — file preview",
            eframe::NativeOptions::default(),
            Box::new(move |cc| {
                let egui_ctx = cc.egui_ctx.clone();
                let task = import_task;
                rt.spawn(async move {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        res = task => {
                            if let Ok(Err(e)) = res {
                                tracing::warn!("import error: {e:#}");
                            }
                        }
                    }
                    egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                });

                let audio_ctx = AudioBackend::default();
                let remote = crate::ui::RemoteControls::new(
                    tracks.broadcast,
                    tracks.video,
                    tracks.audio,
                    audio_ctx,
                    None,
                    &cc.egui_ctx,
                    "file-preview",
                    &[StatCategory::Render, StatCategory::Time],
                    cc.wgpu_render_state.clone(),
                );

                Ok(Box::new(FilePreviewApp {
                    live,
                    _room: room,
                    remote,
                    ticket_str,
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    struct FilePreviewApp {
        live: Live,
        #[allow(dead_code, reason = "kept alive so the room actor keeps running")]
        _room: Option<Room>,
        remote: crate::ui::RemoteControls,
        ticket_str: String,
    }

    impl eframe::App for FilePreviewApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));

            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                    crate::ui::top_bar(ui, ctx, &self.ticket_str);

                    let avail = ui.available_size();
                    let video_rect = egui::Rect::from_min_size(ui.cursor().min, avail);
                    if let Some(video) = self.remote.video.as_mut() {
                        let (img, _) = video.render(ctx, avail);
                        ui.add_sized(avail, img);
                    }

                    self.remote.update_overlay();
                    self.remote
                        .overlay
                        .show(ui, video_rect, self.remote.broadcast.stats());
                });
        }

        fn on_exit(&mut self) {
            info!("exit");
            self.remote.broadcast.shutdown();
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }

    // -- Capture preview --

    pub(super) fn run_capture_preview(
        live: Live,
        broadcast: LocalBroadcast,
        audio_ctx: AudioBackend,
        room: Option<Room>,
        ticket_str: String,
    ) -> n0_error::Result {
        eframe::run_native(
            "irl — publish preview",
            eframe::NativeOptions::default(),
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);

                let preview = broadcast
                    .preview()
                    .map(|track| VideoTrackView::new(&cc.egui_ctx, "preview", track));

                Ok(Box::new(CapturePreviewApp {
                    live,
                    _room: room,
                    broadcast,
                    audio_ctx,
                    preview,
                    devices: crate::ui::DeviceSelectors::new(),
                    overlay: DebugOverlay::new(&[StatCategory::Capture, StatCategory::Net]),
                    ticket_str,
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    struct CapturePreviewApp {
        live: Live,
        #[allow(dead_code, reason = "kept alive so the room actor keeps running")]
        _room: Option<Room>,
        broadcast: LocalBroadcast,
        audio_ctx: AudioBackend,
        preview: Option<VideoTrackView>,
        devices: crate::ui::DeviceSelectors,
        overlay: DebugOverlay,
        ticket_str: String,
    }

    impl eframe::App for CapturePreviewApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));

            if self.devices.is_dirty() {
                self.devices.apply(&self.broadcast, &self.audio_ctx);
                self.preview = None;
            }

            if self.preview.is_none()
                && let Some(track) = self.broadcast.preview()
            {
                self.preview = Some(VideoTrackView::new(ctx, "preview", track));
            }

            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                    crate::ui::top_bar(ui, ctx, &self.ticket_str);

                    let avail = ui.available_size();
                    let video_rect = egui::Rect::from_min_size(ui.cursor().min, avail);

                    if let Some(view) = self.preview.as_mut() {
                        let video_size = fit_to_aspect(avail, 16.0 / 9.0);
                        let (img, _) = view.render(ctx, video_size);
                        ui.centered_and_justified(|ui| ui.add_sized(video_size, img));
                    } else {
                        ui.allocate_space(avail);
                    }

                    self.overlay
                        .show_publish(ui, video_rect, self.broadcast.stats());

                    egui::Area::new(egui::Id::new("publish-controls"))
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
                                        self.devices.ui(ui, "preview");
                                    });
                                });
                        });
                });
        }

        fn on_exit(&mut self) {
            info!("exit");
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }
}

#[cfg(feature = "wgpu")]
use preview::{run_capture_preview, run_file_preview};
