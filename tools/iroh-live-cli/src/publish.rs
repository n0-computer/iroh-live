//! `irl publish` — publish capture or file over iroh.
//!
//! Subcommands: `capture` (default), `file`.
//! Transport: serve by default, `--relay` pushes to relay, `--room` publishes
//! into a room, `--no-serve` disables incoming connections.

use std::time::Duration;

use eframe::egui;
use iroh_live::{
    Live,
    media::{
        AudioBackend,
        codec::DefaultDecoders,
        format::PlaybackConfig,
        publish::{LocalBroadcast, subscribe_preview_from_consumer},
    },
    ticket::LiveTicket,
};
use moq_lite::BroadcastProducer;
use moq_media_egui::{
    VideoTrackView,
    overlay::{DebugOverlay, StatCategory, fit_to_aspect},
};
use n0_error::anyerr;
use tracing::info;

use crate::{
    args::{PublishArgs, PublishInput},
    import::{init_import, open_input, run_import},
    transport::{publish_producer, setup_live},
};

pub fn run(args: PublishArgs, rt: &tokio::runtime::Runtime) -> n0_error::Result {
    match args.input {
        Some(PublishInput::File(ref file_args)) => run_file_cmd(&args, file_args, rt),
        Some(PublishInput::Capture(ref capture_args)) => run_capture_cmd(&args, capture_args, rt),
        None => {
            let default_capture = crate::args::CaptureArgs {
                video: vec![],
                audio: vec![],
                test_source: false,
                codec: None,
                video_presets: None,
                audio_preset: "hq".to_string(),
            };
            run_capture_cmd(&args, &default_capture, rt)
        }
    }
}

fn run_capture_cmd(
    args: &PublishArgs,
    capture: &crate::args::CaptureArgs,
    rt: &tokio::runtime::Runtime,
) -> n0_error::Result {
    let (live, broadcast, audio_ctx) = rt.block_on(async {
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
        let audio_ctx = AudioBackend::default();

        crate::source::setup_video(&broadcast, &video_sources, codec, &presets)?;
        crate::source::setup_audio(&broadcast, &audio_sources, &audio_ctx, audio_preset).await?;
        crate::transport::publish_broadcast(&live, &broadcast, &args.transport).await?;

        anyhow::Ok((live, broadcast, audio_ctx))
    })?;

    if args.preview {
        let ticket = LiveTicket::new(live.endpoint().addr(), &args.transport.name);
        let _guard = rt.enter();
        run_capture_preview(live, broadcast, audio_ctx, ticket.to_string())
    } else {
        println!("press Ctrl+C to stop");
        rt.block_on(async {
            tokio::signal::ctrl_c().await?;
            live.shutdown().await;
            n0_error::Ok(())
        })
    }
}

fn run_file_cmd(
    args: &PublishArgs,
    file_args: &crate::args::FileInputArgs,
    rt: &tokio::runtime::Runtime,
) -> n0_error::Result {
    // Common setup: open input, init catalog, then publish.
    // Publishing must happen after init so the catalog is available to subscribers.
    let (live, decoder, input, preview_consumer) = rt.block_on(async {
        let mut input = open_input(&file_args.file, file_args.transcode, file_args.format).await?;
        let live = setup_live(!args.transport.no_serve).await?;
        let mut broadcast = BroadcastProducer::default();
        let preview_consumer = broadcast.consume();
        let decoder = init_import(&mut broadcast, file_args.format, &mut input).await?;
        publish_producer(&live, broadcast, &args.transport).await?;
        anyhow::Ok((live, decoder, input, preview_consumer))
    })?;

    if args.preview {
        let (tracks, import_task, ticket_str) = rt.block_on(async {
            let audio_ctx = AudioBackend::default();
            let tracks = subscribe_preview_from_consumer::<DefaultDecoders>(
                preview_consumer,
                &audio_ctx,
                PlaybackConfig::default(),
            )
            .await?;
            let import_task = tokio::spawn(run_import(decoder, input));
            let ticket = LiveTicket::new(live.endpoint().addr(), &args.transport.name);
            anyhow::Ok((tracks, import_task, ticket.to_string()))
        })?;

        let _guard = rt.enter();
        run_file_preview(live, tracks, import_task, ticket_str)
    } else {
        println!("press Ctrl+C to stop");
        rt.block_on(async move {
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
// File preview window
// ---------------------------------------------------------------------------

fn run_file_preview(
    live: Live,
    tracks: iroh_live::media::subscribe::MediaTracks,
    import_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    ticket_str: String,
) -> n0_error::Result {
    let rt = tokio::runtime::Handle::current();
    let signals = tokio::sync::watch::channel(iroh_live::media::net::NetworkSignals::default()).1;

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
                signals,
                &cc.egui_ctx,
                "file-preview",
                &[StatCategory::Render, StatCategory::Time],
            );

            Ok(Box::new(FilePreviewApp {
                live,
                remote,
                ticket_str,
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

struct FilePreviewApp {
    live: Live,
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
        let live = self.live.clone();
        tokio::runtime::Handle::current().block_on(async move {
            live.shutdown().await;
        });
    }
}

// ---------------------------------------------------------------------------
// Capture preview window
// ---------------------------------------------------------------------------

fn run_capture_preview(
    live: Live,
    broadcast: LocalBroadcast,
    audio_ctx: AudioBackend,
    ticket_str: String,
) -> n0_error::Result {
    let rt = tokio::runtime::Handle::current();

    eframe::run_native(
        "irl — publish preview",
        eframe::NativeOptions::default(),
        Box::new(move |cc| {
            let egui_ctx = cc.egui_ctx.clone();
            rt.spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            });

            let preview = broadcast
                .preview()
                .map(|track| VideoTrackView::new(&cc.egui_ctx, "preview", track));

            Ok(Box::new(CapturePreviewApp {
                live,
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
                let controls_h = 30.0;
                let video_avail = egui::vec2(avail.x, avail.y - controls_h);
                let video_rect = egui::Rect::from_min_size(ui.cursor().min, video_avail);

                if let Some(view) = self.preview.as_mut() {
                    let video_size = fit_to_aspect(video_avail, 16.0 / 9.0);
                    let (img, _) = view.render(ctx, video_size);
                    ui.centered_and_justified(|ui| ui.add_sized(video_size, img));
                } else {
                    ui.allocate_space(video_avail);
                }

                self.overlay
                    .show_publish(ui, video_rect, self.broadcast.stats());

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    self.devices.ui(ui, "preview");
                });
            });
    }

    fn on_exit(&mut self) {
        info!("exit");
        let live = self.live.clone();
        tokio::runtime::Handle::current().block_on(async move {
            live.shutdown().await;
        });
    }
}
