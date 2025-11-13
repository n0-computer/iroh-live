use std::time::Duration;

use eframe::egui::{self, Color32, Vec2};
use iroh::Endpoint;
use iroh_live::{Live, LiveTicket, WatchTrack, audio::AudioBackend, ffmpeg_log_init};
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
    let (endpoint, _broadcast, video) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let endpoint = Endpoint::bind().await?;
            let live = Live::new(endpoint.clone());
            let mut session = live.connect(ticket.endpoint_id).await?;
            println!("connected!");
            let broadcast = session.consume(&ticket.broadcast_name).await?;
            let _audio = broadcast.listen(audio_ctx).await?;
            let video = broadcast.watch(&Default::default())?;
            n0_error::Ok((endpoint, broadcast, video))
        }
    })?;

    eframe::run_native(
        "IrohLive",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            let app = App {
                video: VideoView::new(&cc.egui_ctx, video),
                endpoint,
                rt,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

struct App {
    video: VideoView,
    endpoint: Endpoint,
    rt: tokio::runtime::Runtime,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30)); // min 30 fps
        egui::CentralPanel::default()
            .frame(egui::Frame::none().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                let avail = ui.available_size();
                ui.add_sized(avail, self.video.render(ctx, avail));
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

    // fn is_closed(&self) -> bool {
    //     self.track.video_frames.is_closed()
    // }
}
