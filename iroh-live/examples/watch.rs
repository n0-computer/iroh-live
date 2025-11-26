use std::time::Duration;

use eframe::egui::{self, Color32, Id, Vec2};
use iroh::Endpoint;
use iroh_live::{
    Live, LiveSession,
    audio::AudioBackend,
    ffmpeg::{FfmpegAudioDecoder, FfmpegVideoDecoder, ffmpeg_log_init},
    subscribe::{AudioTrack, SubscribeBroadcast, WatchTrack},
    ticket::LiveTicket,
    util::StatsSmoother,
};
use n0_error::{Result, StackResultExt, anyerr};

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    ffmpeg_log_init();
    let ticket_str = std::env::args()
        .into_iter()
        .nth(1)
        .context("missing ticket")?;
    let ticket = LiveTicket::deserialize(&ticket_str)?;

    let audio_ctx = AudioBackend::new();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    println!("connecting to {ticket} ...");
    let (endpoint, session, broadcast, video, audio) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let endpoint = Endpoint::bind().await?;
            let live = Live::new(endpoint.clone());
            let mut session = live.connect(ticket.endpoint_id).await?;
            println!("connected!");
            let consumer = session.subscribe(&ticket.broadcast_name).await?;
            let broadcast = SubscribeBroadcast::new(consumer).await?;
            let audio_out = audio_ctx.default_speaker().await?;
            let audio = broadcast.listen::<FfmpegAudioDecoder>(audio_out)?;
            let video = broadcast.watch::<FfmpegVideoDecoder>()?;
            n0_error::Ok((endpoint, session, broadcast, video, audio))
        }
    })?;

    let _guard = rt.enter();
    eframe::run_native(
        "IrohLive",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            let app = App {
                video: VideoView::new(&cc.egui_ctx, video),
                _audio_ctx: audio_ctx,
                _audio: audio,
                broadcast,
                stats: StatsSmoother::new(),
                endpoint,
                session,
                rt,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

struct App {
    video: VideoView,
    _audio: AudioTrack,
    _audio_ctx: AudioBackend,
    endpoint: Endpoint,
    session: LiveSession,
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
                ui.add_sized(avail, self.video.render(ctx, avail));

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
        let endpoint = self.endpoint.clone();
        self.rt.block_on(async move {
            endpoint.close().await;
        });
    }
}

impl App {
    fn render_overlay(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            let selected = self.video.track.rendition().to_owned();
            egui::ComboBox::from_label("")
                .selected_text(selected.clone())
                .show_ui(ui, |ui| {
                    for name in self.broadcast.video_renditions() {
                        if ui.selectable_label(&selected == name, name).clicked() {
                            if let Ok(track) = self
                                .broadcast
                                .watch_rendition::<FfmpegVideoDecoder>(&Default::default(), &name)
                            {
                                self.video = VideoView::new(ctx, track);
                            }
                        }
                    }
                });

            let (rtt, bw) = self.stats.smoothed(|| self.session.conn().stats());
            ui.label(format!("BW:  {bw}"));
            ui.label(format!("RTT: {}ms", rtt.as_millis()));
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
