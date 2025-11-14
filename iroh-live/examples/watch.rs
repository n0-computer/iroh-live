use std::time::{Duration, Instant};

use byte_unit::{Bit, UnitType};
use eframe::egui::{self, Color32, Id, Vec2};
use iroh::Endpoint;
use iroh_live::{
    AudioTrack, ConsumeBroadcast, Live, LiveSession, LiveTicket, WatchTrack, audio::AudioBackend,
    ffmpeg::ffmpeg_log_init,
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
            let broadcast = session.consume(&ticket.broadcast_name).await?;
            let audio_out = audio_ctx.default_speaker().await?;
            let audio = broadcast.listen(audio_out)?;
            let video = broadcast.watch()?;
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
                bw: BwSmoother::new(),
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
    broadcast: ConsumeBroadcast,
    bw: BwSmoother,
    rt: tokio::runtime::Runtime,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30)); // min 30 fps
        self.bw.set_total(self.session.stats().udp_rx.bytes as u32);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                // render video
                let avail = ui.available_size();
                ui.add_sized(avail, self.video.render(ctx, avail));

                // render overlay.
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
                            if let Ok(track) = self.broadcast.watch_rendition(
                                &Default::default(),
                                &name,
                                iroh_live::av::Backend::Ffmpeg,
                            ) {
                                self.video = VideoView::new(ctx, track);
                            }
                        }
                    }
                });

            // let selected = self
            //     .audio
            //     .as_ref()
            //     .map(|a| a.rendition().to_owned())
            //     .unwrap_or_else(|| "none".to_owned());
            // egui::ComboBox::from_label("audio")
            //     .selected_text(selected.clone())
            //     .show_ui(ui, |ui| {
            //         if ui.selectable_label(&selected == "none", "none").clicked() {
            //             self.audio = None;
            //         }
            //         for name in self.broadcast.audio_renditions() {
            //             if ui.selectable_label(&selected == name, name).clicked() {
            //                 self.rt
            //                     .block_on(async {
            //                         let audio_out = self.audio_ctx.default_speaker().await?;
            //                         let track = self.broadcast.listen_rendition(name, audio_out)?;
            //                         self.audio = Some(track);
            //                         n0_error::Ok(())
            //                     })
            //                     .ok();
            //             }
            //         }
            //     });

            let bw = self.bw.get_rate();
            ui.label(format!("BW: {bw}"));
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
            self.texture = ctx.load_texture("video", image, Default::default());
        }
        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}

struct BwSmoother {
    // Last total byte counter we used when we updated the rate
    last_bytes_for_rate: u64,
    // Most recent total byte counter provided via set_total()
    total_bytes: u64,
    // When we last updated `cached_rate`
    last_rate_update: Instant,
    // Cached human-readable rate string
    cached_rate: String,
    // Have we ever seen data yet?
    initialized: bool,
}

impl BwSmoother {
    pub fn new() -> Self {
        Self {
            last_bytes_for_rate: 0,
            total_bytes: 0,
            last_rate_update: Instant::now(),
            cached_rate: "0.00 bit/s".to_string(),
            initialized: false,
        }
    }

    /// Update the total number of bytes received so far.
    /// (Your counter, e.g. from the NIC / connection.)
    pub fn set_total(&mut self, bytes: u32) {
        self.total_bytes = bytes as u64;
    }

    /// Get a smoothed bandwidth string, e.g. "8.32 Mbit/s".
    /// The value is only recomputed at most ~once per second.
    pub fn get_rate(&mut self) -> String {
        let now = Instant::now();

        // First call: just initialise timing and return the default string.
        if !self.initialized {
            self.initialized = true;
            self.last_rate_update = now;
            self.last_bytes_for_rate = self.total_bytes;
            return self.cached_rate.clone();
        }

        let elapsed = now.duration_since(self.last_rate_update);

        // Only recompute if >= 1 second has passed
        if elapsed >= Duration::from_secs(1) {
            let elapsed_secs = elapsed.as_secs_f64();
            let delta_bytes = self.total_bytes.saturating_sub(self.last_bytes_for_rate);

            // Handle wraparound or no progress
            let bits_per_sec = if elapsed_secs <= 0.0 || delta_bytes == 0 {
                0.0
            } else {
                (delta_bytes as f64 * 8.0) / elapsed_secs
            };

            let bit = Bit::from_f64(bits_per_sec).unwrap();
            let adjusted = bit.get_appropriate_unit(UnitType::Decimal); // Kbit, Mbit, Gbit, ...

            // Example output: "8.32 Mbit/s"
            self.cached_rate = format!("{adjusted:.2}/s");

            // Update state for next round
            self.last_rate_update = now;
            self.last_bytes_for_rate = self.total_bytes;
        }

        self.cached_rate.clone()
    }
}

impl Default for BwSmoother {
    fn default() -> Self {
        Self::new()
    }
}
