use std::time::{Duration, SystemTime};

use clap::{Parser, ValueEnum};
use eframe::egui::{self, Color32, Id, Vec2};
use iroh::Endpoint;
use iroh_live::{
    Live, LiveSession, LiveTicket, Quality, VideoRendition, WatchTrack, audio::AudioBackend,
    av::Backend,
};
use n0_error::{Result, StackResultExt, anyerr};

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let ticket_str = cli.ticket;
    let ticket = LiveTicket::deserialize(&ticket_str)?;

    let audio_ctx = AudioBackend::new();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    println!("connecting to {ticket} ...");
    let (endpoint, session, broadcast, video, renditions) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let endpoint = Endpoint::bind().await?;
            let live = Live::new(endpoint.clone());
            let mut session = live.connect(ticket.endpoint_id).await?;
            println!("connected!");
            let broadcast = session.consume(&ticket.broadcast_name).await?;
            let _audio = broadcast.listen_with(audio_ctx, cli.quality.into()).await?;
            let video =
                broadcast.watch_with(&Default::default(), cli.quality.into(), Backend::Native)?;
            let renditions = broadcast.video_renditions();
            n0_error::Ok((endpoint, session, broadcast, video, renditions))
        }
    })?;

    eframe::run_native(
        "IrohLive",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            let app = App {
                video: VideoView::new(&cc.egui_ctx, video),
                endpoint,
                session,
                broadcast,
                rt,
                renditions,
                selected: 0,
                backend: Backend::Native,
                prev_bytes: 0,
                prev_instant: std::time::Instant::now(),
                bitrate_mbps: 0.0,
                rtt_ms: 0.0,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

#[derive(Copy, Clone, ValueEnum, Debug)]
enum CliQuality {
    Highest,
    High,
    Mid,
    Low,
}

impl From<CliQuality> for Quality {
    fn from(v: CliQuality) -> Self {
        match v {
            CliQuality::Highest => Quality::Highest,
            CliQuality::High => Quality::High,
            CliQuality::Mid => Quality::Mid,
            CliQuality::Low => Quality::Low,
        }
    }
}

#[derive(Parser, Debug)]
struct Cli {
    ticket: String,
    #[arg(long, value_enum, default_value_t=CliQuality::Highest)]
    quality: CliQuality,
}

struct App {
    video: VideoView,
    endpoint: Endpoint,
    session: LiveSession,
    broadcast: iroh_live::ConsumeBroadcast,
    rt: tokio::runtime::Runtime,
    renditions: Vec<VideoRendition>,
    selected: usize,
    backend: Backend,
    prev_bytes: u64,
    prev_instant: std::time::Instant,
    bitrate_mbps: f32,
    rtt_ms: f32,
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
                // Overlay: rendition + backend selector
                egui::Area::new(Id::new("rendition_overlay"))
                    .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -8.0])
                    .show(ctx, |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128))
                            .corner_radius(3.0)
                            .show(ui, |ui| {
                                let names: Vec<_> =
                                    self.renditions.iter().map(|r| r.name.as_str()).collect();
                                egui::ComboBox::from_label("")
                                    .selected_text(
                                        names.get(self.selected).unwrap_or(&"-").to_string(),
                                    )
                                    .show_ui(ui, |ui| {
                                        for (i, name) in names.iter().enumerate() {
                                            if ui
                                                .selectable_label(self.selected == i, *name)
                                                .clicked()
                                            {
                                                self.selected = i;
                                                let name = name.to_string();
                                                if let Ok(new_track) = self.rt.block_on(async {
                                                    self.broadcast.watch_rendition(
                                                        &Default::default(),
                                                        &name,
                                                        self.backend,
                                                    )
                                                }) {
                                                    self.video = VideoView::new(ctx, new_track);
                                                }
                                            }
                                        }
                                    });
                                ui.separator();
                                egui::ComboBox::from_label("Backend")
                                    .selected_text(match self.backend {
                                        Backend::Native => "Native",
                                        Backend::Ffmpeg => "FFmpeg",
                                    })
                                    .show_ui(ui, |ui| {
                                        let mut change = None;
                                        if ui
                                            .selectable_label(
                                                matches!(self.backend, Backend::Native),
                                                "Native",
                                            )
                                            .clicked()
                                        {
                                            change = Some(Backend::Native);
                                        }
                                        if ui
                                            .selectable_label(
                                                matches!(self.backend, Backend::Ffmpeg),
                                                "FFmpeg",
                                            )
                                            .clicked()
                                        {
                                            change = Some(Backend::Ffmpeg);
                                        }
                                        if let Some(new_backend) = change {
                                            if new_backend != self.backend {
                                                self.backend = new_backend;
                                                // Re-subscribe current rendition with new backend
                                                let name = names
                                                    .get(self.selected)
                                                    .copied()
                                                    .unwrap_or("720p")
                                                    .to_string();
                                                if let Ok(new_track) = self.rt.block_on(async {
                                                    self.broadcast.watch_rendition(
                                                        &Default::default(),
                                                        &name,
                                                        self.backend,
                                                    )
                                                }) {
                                                    self.video = VideoView::new(ctx, new_track);
                                                }
                                            }
                                        }
                                    });
                                ui.separator();
                                // Stats: bitrate and RTT; simple smoothing per frame
                                let now = std::time::Instant::now();
                                let stats = self.session.stats();
                                let bytes = stats.udp_rx.bytes;
                                let dt = now
                                    .saturating_duration_since(self.prev_instant)
                                    .as_secs_f32()
                                    .max(0.001);
                                let sample_bps =
                                    (bytes.saturating_sub(self.prev_bytes)) as f32 * 8.0 / dt; // bits/s
                                self.prev_bytes = bytes;
                                self.prev_instant = now;
                                let alpha = 0.2f32;
                                self.bitrate_mbps = alpha * (sample_bps / 1_000_000.0)
                                    + (1.0 - alpha) * self.bitrate_mbps;
                                let sample_rtt = stats.path.rtt.as_secs_f32() * 1000.0;
                                self.rtt_ms = alpha * sample_rtt + (1.0 - alpha) * self.rtt_ms;
                                ui.label(format!("BW: {:.2} Mbps", self.bitrate_mbps));
                                ui.label(format!("RTT: {:.0} ms", self.rtt_ms));
                                ui.label(format!("FPS: {:.1}", self.video.fps()));
                                ui.label(format!(
                                    "Latency: {:.0} ms",
                                    self.video.last_latency_ms()
                                ));
                                // jitter controls removed
                            });
                    });
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let endpoint = self.endpoint.clone();
        self.rt.block_on(async move {
            endpoint.close().await;
        });
    }
}

struct VideoView {
    track: WatchTrack,
    texture: egui::TextureHandle,
    size: egui::Vec2,
    buffer: std::collections::VecDeque<iroh_live::video::DecodedFrame>,
    last_present: Option<SystemTime>,
    frame_counter: u32,
    fps_value: f32,
    last_latency_ms: f32,
    start_origin: Option<SystemTime>,
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
            buffer: Default::default(),
            last_present: None,
            frame_counter: 0,
            fps_value: 0.0,
            last_latency_ms: 0.0,
            start_origin: None,
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
            self.buffer.push_back(frame);
        }
        let now = std::time::SystemTime::now();
        let mut display: Option<iroh_live::video::DecodedFrame> = None;
        while let Some(f) = self.buffer.front() {
            let intended = std::time::UNIX_EPOCH + f.timestamp;
            if now >= intended {
                display = self.buffer.pop_front();
            } else {
                break;
            }
        }
        if let Some(frame) = display {
            let (w, h) = frame.img().dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                frame.img().as_raw(),
            );
            self.texture = ctx.load_texture("video", image, Default::default());
            self.frame_counter += 1;
            if let Some(last) = self.last_present {
                let dt = now.duration_since(last).unwrap_or(Duration::ZERO);
                if dt.as_secs_f32() >= 0.5 {
                    self.fps_value = self.frame_counter as f32 / dt.as_secs_f32();
                    self.frame_counter = 0;
                    self.last_present = Some(now);
                }
            } else {
                self.last_present = Some(now);
            }
            // Estimate sender/receiver clock offset from first frame arrival
            if self.start_origin.is_none() {
                self.start_origin = Some(now - frame.timestamp);
            }
            if let Some(origin) = self.start_origin {
                if let Ok(intended) = origin.checked_add(frame.timestamp).ok_or(()) {
                    if let Ok(lat) = now.duration_since(intended) {
                        // round to full ms
                        self.last_latency_ms = (lat.as_millis() as f32).round();
                    }
                }
            }
        }
        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }

    fn fps(&self) -> f32 {
        self.fps_value
    }
    fn last_latency_ms(&self) -> f32 {
        self.last_latency_ms
    }

    // fn is_closed(&self) -> bool {
    //     self.track.video_frames.is_closed()
    // }
}
