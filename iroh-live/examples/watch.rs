use std::time::Duration;

use clap::Parser;
use eframe::egui::{self, Color32, Id, Vec2};
use iroh::{Endpoint, EndpointId};
use iroh_live::{
    Live,
    media::{
        audio::AudioBackend,
        codec::{DefaultDecoders, DynamicVideoDecoder, codec_init},
        subscribe::{AudioTrack, SubscribeBroadcast, WatchTrack},
    },
    moq::MoqSession,
    ticket::LiveTicket,
    util::StatsSmoother,
};
use n0_error::{Result, anyerr};
use tracing::info;

#[derive(Debug, Parser)]
struct Cli {
    #[clap(long, conflicts_with = "endpoint-id")]
    ticket: Option<LiveTicket>,
    #[clap(long, conflicts_with = "ticket", requires = "name")]
    endpoint_id: Option<EndpointId>,
    #[clap(long, conflicts_with = "ticket", requires = "endpoint-id")]
    name: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ticket = match (cli.ticket, cli.endpoint_id, cli.name) {
        (Some(ticket), None, None) => ticket,
        (None, Some(endpoint_id), Some(name)) => LiveTicket::new(endpoint_id, name),
        _ => {
            eprintln!("Invalid arguments: Use either --ticket, or --endpoint and --name");
            std::process::exit(1);
        }
    };

    tracing_subscriber::fmt::init();
    codec_init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let audio_ctx = AudioBackend::new(None, None);

    println!("connecting to {ticket} ...");
    let (endpoint, session, track) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let endpoint = Endpoint::bind().await?;
            let live = Live::new(endpoint.clone());
            let audio_out = audio_ctx.default_output().await?;
            let (session, track) = live
                .watch_and_listen::<DefaultDecoders>(
                    ticket.endpoint,
                    &ticket.broadcast_name,
                    audio_out,
                    Default::default(),
                )
                .await?;
            println!("connected!");
            n0_error::Ok((endpoint, session, track))
        }
    })?;

    let _guard = rt.enter();

    eframe::run_native(
        "IrohLive",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            let egui_ctx = cc.egui_ctx.clone();
            rt.spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                // TODO: When the app is not visible, this will not trigger `update` immediately.
                // See https://github.com/emilk/egui/issues/5112
                egui_ctx.request_repaint();
            });
            let app = App {
                video: track.video.map(|video| VideoView::new(&cc.egui_ctx, video)),
                _audio_ctx: audio_ctx,
                _audio: track.audio,
                broadcast: track.broadcast,
                session: session,
                stats: StatsSmoother::new(),
                endpoint,
                rt,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

struct App {
    video: Option<VideoView>,
    _audio: Option<AudioTrack>,
    _audio_ctx: AudioBackend,
    endpoint: Endpoint,
    session: MoqSession,
    broadcast: SubscribeBroadcast,
    stats: StatsSmoother,
    rt: tokio::runtime::Runtime,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30)); // min 30 fps
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                let avail = ui.available_size();
                if let Some(video) = self.video.as_mut() {
                    ui.add_sized(avail, video.render(ctx, avail));
                }

                egui::Area::new(Id::new("overlay"))
                    .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -8.0])
                    .show(ctx, |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128))
                            .corner_radius(3.0)
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                                ui.set_min_width(100.);
                                self.render_overlay(ctx, ui);
                            })
                    })
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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

impl App {
    fn render_overlay(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            let selected = self
                .video
                .as_ref()
                .map(|video| video.track.rendition().to_owned());
            egui::ComboBox::from_label("")
                .selected_text(selected.clone().unwrap_or_default())
                .show_ui(ui, |ui| {
                    for name in self.broadcast.catalog().video_renditions() {
                        if ui
                            .selectable_label(selected.as_deref() == Some(name), name)
                            .clicked()
                        {
                            if let Ok(track) = self
                                .broadcast
                                .watch_rendition::<DynamicVideoDecoder>(&Default::default(), name)
                            {
                                self.video = Some(VideoView::new(ctx, track));
                            }
                        }
                    }
                });

            let stats = self.stats.smoothed(|| self.session.conn().stats());
            ui.label(format!(
                "peer:   {}",
                self.session.conn().remote_id().fmt_short()
            ));
            ui.label(format!("BW up:   {}", stats.up.rate_str));
            ui.label(format!("BW down: {}", stats.down.rate_str));
            ui.label(format!("RTT:     {}ms", stats.rtt.as_millis()));
        });
    }
}

struct VideoView {
    track: WatchTrack,
    texture: egui::TextureHandle,
    size: egui::Vec2,
}

impl VideoView {
    fn new(ctx: &egui::Context, track: WatchTrack) -> Self {
        let size = egui::vec2(100., 100.);
        let color_image =
            egui::ColorImage::filled([size.x as usize, size.y as usize], Color32::BLACK);
        let texture = ctx.load_texture("video", color_image, egui::TextureOptions::default());
        Self {
            size,
            texture,
            track,
        }
    }

    fn render(&mut self, ctx: &egui::Context, available_size: Vec2) -> egui::Image<'_> {
        let available_size = available_size.into();
        if available_size != self.size {
            self.size = available_size;
            let ppp = ctx.pixels_per_point();
            let w = (available_size.x * ppp) as u32;
            let h = (available_size.y * ppp) as u32;
            self.track.set_viewport(w, h);
        }
        if let Some(frame) = self.track.current_frame() {
            let (w, h) = frame.img().dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                frame.img().as_raw(),
            );
            self.texture.set(image, Default::default());
        }
        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}
