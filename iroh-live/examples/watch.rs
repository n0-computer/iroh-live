use std::time::Duration;

use clap::Parser;
use eframe::egui;
use iroh::{Endpoint, EndpointId};
use iroh_live::{
    Live,
    media::{
        audio_backend::AudioBackend,
        codec::DefaultDecoders,
        format::{DecodeConfig, DecoderBackend, PlaybackConfig},
        subscribe::{AudioTrack, RemoteBroadcast},
    },
    moq::MoqSession,
    ticket::LiveTicket,
};
use moq_media_egui::{
    VideoTrackView, create_egui_wgpu_config,
    overlay::{DebugOverlay, StatCategory},
};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

#[derive(Debug, Parser)]
struct Cli {
    #[clap(long, conflicts_with = "endpoint-id")]
    ticket: Option<LiveTicket>,
    #[clap(long, conflicts_with = "ticket", requires = "name")]
    endpoint_id: Option<EndpointId>,
    #[clap(long, conflicts_with = "ticket", requires = "endpoint-id")]
    name: Option<String>,
    /// Use wgpu for hardware-accelerated rendering
    #[clap(long)]
    wgpu: bool,
    /// Decoder: "auto" (try HW then SW) or "software" (force SW)
    #[clap(long, default_value = "auto")]
    decoder: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let ticket = match (&cli.ticket, &cli.endpoint_id, &cli.name) {
        (Some(ticket), None, None) => ticket.clone(),
        (None, Some(endpoint_id), Some(name)) => LiveTicket::new(*endpoint_id, name.clone()),
        _ => {
            eprintln!("Invalid arguments: Use either --ticket, or --endpoint and --name");
            std::process::exit(1);
        }
    };

    let use_wgpu = cli.wgpu;
    let backend = match cli.decoder.as_str() {
        "auto" => DecoderBackend::Auto,
        "software" | "sw" => DecoderBackend::Software,
        other => {
            eprintln!("Unknown decoder: {other}. Use 'auto' or 'software'");
            std::process::exit(1);
        }
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let audio_ctx = AudioBackend::default();

    println!("connecting to {ticket} ...");
    let (endpoint, session, track) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let endpoint = Endpoint::bind(iroh::endpoint::presets::N0).await?;
            let live = Live::new(endpoint.clone());
            let playback_config = PlaybackConfig {
                decode_config: DecodeConfig {
                    backend,
                    ..Default::default()
                },
                ..Default::default()
            };
            let (session, broadcast) = live
                .subscribe(ticket.endpoint, &ticket.broadcast_name)
                .await?;
            iroh_live::util::spawn_stats_recorder(
                session.conn(),
                broadcast.stats().net.clone(),
                broadcast.shutdown_token(),
            );
            let track = broadcast
                .media::<DefaultDecoders>(&audio_ctx, playback_config)
                .await?;

            println!("connected!");
            n0_error::Ok((endpoint, session, track))
        }
    })?;

    let _guard = rt.enter();

    let native_options = if use_wgpu {
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: create_egui_wgpu_config(),
            ..Default::default()
        }
    } else {
        eframe::NativeOptions::default()
    };

    eframe::run_native(
        "IrohLive",
        native_options,
        Box::new(move |cc| {
            let egui_ctx = cc.egui_ctx.clone();
            rt.spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                egui_ctx.request_repaint();
            });

            let video = track.video.map(|video| {
                if use_wgpu {
                    VideoTrackView::new_wgpu(
                        &cc.egui_ctx,
                        "video",
                        video,
                        cc.wgpu_render_state.as_ref(),
                    )
                } else {
                    VideoTrackView::new(&cc.egui_ctx, "video", video)
                }
            });

            let app = App {
                video,
                audio_ctx,
                audio: track.audio,
                broadcast: track.broadcast,
                session,
                overlay: DebugOverlay::new(&[
                    StatCategory::Net,
                    StatCategory::Render,
                    StatCategory::Time,
                ]),
                endpoint,
                rt,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

struct App {
    video: Option<VideoTrackView>,
    audio: Option<AudioTrack>,
    audio_ctx: AudioBackend,
    endpoint: Endpoint,
    session: MoqSession,
    broadcast: RemoteBroadcast,
    overlay: DebugOverlay,
    rt: tokio::runtime::Runtime,
}

impl App {
    fn resubscribe(&mut self, ctx: &egui::Context) {
        match self.broadcast.video() {
            Ok(track) => {
                info!(rendition = track.rendition(), "resubscribed to video");
                self.video = Some(VideoTrackView::new(ctx, "video", track));
            }
            Err(e) => {
                warn!("video resubscribe failed: {e:#}");
                self.video = None;
            }
        }

        let handle = tokio::runtime::Handle::current();
        match handle.block_on(self.broadcast.audio(&self.audio_ctx)) {
            Ok(track) => {
                info!("resubscribed to audio");
                self.audio = Some(track);
            }
            Err(e) => {
                warn!("audio resubscribe failed: {e:#}");
                self.audio = None;
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30));

        let video_closed = self.video.as_ref().is_some_and(|v| v.track().is_closed());
        if video_closed {
            info!("video track closed, resubscribing from current catalog");
            self.resubscribe(ctx);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                let avail = ui.available_size();
                let video_rect = egui::Rect::from_min_size(ui.cursor().min, avail);
                if let Some(video) = self.video.as_mut() {
                    let (img, _) = video.render(ctx, avail);
                    ui.add_sized(avail, img);
                }

                // Update labels from live state.
                let stats = self.broadcast.stats();
                if let Some(v) = &self.video {
                    self.overlay.update_from_track(stats, v.track());
                    stats.render.renderer.set(if v.is_wgpu() {
                        v.render_path_name()
                    } else {
                        "cpu"
                    });
                }

                self.overlay.show(ui, video_rect, stats);
            });
    }

    fn on_exit(&mut self) {
        info!("exit");
        self.broadcast.shutdown();
        self.session.close(0, b"bye");
        let endpoint = self.endpoint.clone();
        self.rt.block_on(async move {
            endpoint.close().await;
            info!("endpoint closed");
        });
    }
}
