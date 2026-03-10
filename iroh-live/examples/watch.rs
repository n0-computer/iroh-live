use std::time::{Duration, Instant};

use clap::Parser;
use eframe::egui::{self, Color32, Id, Vec2};
use iroh::{Endpoint, EndpointId, Watcher};
use iroh_live::{
    Live,
    media::{
        audio_backend::AudioBackend,
        codec::{DefaultDecoders, DynamicVideoDecoder},
        format::{DecodeConfig, DecoderBackend, PlaybackConfig},
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
    /// Use wgpu for hardware-accelerated rendering
    #[clap(long)]
    wgpu: bool,
    /// Decoder: "auto" (try HW then SW) or "software" (force SW)
    #[clap(long, default_value = "auto")]
    decoder: String,
}

fn main() -> Result<()> {
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

    tracing_subscriber::fmt::init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let audio_ctx = AudioBackend::default();

    println!("connecting to {ticket} ...");
    let (endpoint, session, track) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let endpoint = Endpoint::bind().await?;
            let live = Live::new(endpoint.clone());
            let playback_config = PlaybackConfig {
                decode_config: DecodeConfig {
                    backend,
                    ..Default::default()
                },
                ..Default::default()
            };
            let (session, track) = live
                .watch_and_listen::<DefaultDecoders>(
                    ticket.endpoint,
                    &ticket.broadcast_name,
                    &audio_ctx,
                    playback_config,
                )
                .await?;
            println!("connected!");
            n0_error::Ok((endpoint, session, track))
        }
    })?;

    let _guard = rt.enter();

    let native_options = if use_wgpu {
        #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
        let wgpu_config = {
            // Create a wgpu device with VK_EXT_image_drm_format_modifier
            // for zero-copy DMA-BUF import from VAAPI decoder.
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..Default::default()
            });
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    ..Default::default()
                }))
                .expect("no suitable wgpu adapter");

            let (device, queue) = moq_media::render::create_device_with_dmabuf_extensions(&adapter)
                .unwrap_or_else(|e| {
                    eprintln!("DMA-BUF device creation failed ({e}), using default");
                    pollster::block_on(adapter.request_device(&Default::default()))
                        .expect("wgpu device creation failed")
                });

            egui_wgpu::WgpuConfiguration {
                wgpu_setup: egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
                    instance,
                    adapter,
                    device,
                    queue,
                }),
                ..Default::default()
            }
        };

        #[cfg(not(all(target_os = "linux", feature = "dmabuf-import")))]
        let wgpu_config = egui_wgpu::WgpuConfiguration::default();

        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: wgpu_config,
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
                #[cfg(feature = "wgpu")]
                if use_wgpu {
                    return VideoView::new_wgpu(cc, video);
                }
                VideoView::new(&cc.egui_ctx, video)
            });

            let app = App {
                video,
                _audio_ctx: audio_ctx,
                _audio: track.audio,
                broadcast: track.broadcast,
                session,
                stats: StatsSmoother::new(),
                endpoint,
                rt,
                use_wgpu,
                frame_count: 0,
                fps_last_update: Instant::now(),
                fps: 0.0,
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
    use_wgpu: bool,
    frame_count: u64,
    fps_last_update: Instant,
    fps: f32,
}

impl App {
    fn update_fps(&mut self) {
        self.frame_count += 1;
        let elapsed = self.fps_last_update.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.frame_count as f32 / elapsed.as_secs_f32();
            self.frame_count = 0;
            self.fps_last_update = Instant::now();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30)); // min 30 fps
        self.update_fps();
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
            // Rendition selector
            let selected = self
                .video
                .as_ref()
                .map(|video| video.track.rendition().to_owned());
            egui::ComboBox::from_id_salt("rendition")
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

            let decoder_name = self
                .video
                .as_ref()
                .map(|v| v.track.decoder_name().to_owned())
                .unwrap_or_default();
            let renderer = if self.use_wgpu { "wgpu" } else { "cpu" };

            let stats = self.stats.smoothed(|| {
                let conn = self.session.conn();
                (conn.stats(), conn.paths().get())
            });
            ui.label(format!(
                "peer:    {}",
                self.session.conn().remote_id().fmt_short()
            ));
            ui.label(format!("decoder: {decoder_name}"));
            ui.label(format!("render:  {renderer}"));
            ui.label(format!("fps:     {:.0}", self.fps));
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
    #[cfg(feature = "wgpu")]
    wgpu_state: Option<WgpuState>,
}

#[cfg(feature = "wgpu")]
struct WgpuState {
    renderer: moq_media::render::WgpuVideoRenderer,
    render_state: egui_wgpu::RenderState,
    texture_id: Option<egui::TextureId>,
    last_frame_size: Option<(u32, u32)>,
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
            #[cfg(feature = "wgpu")]
            wgpu_state: None,
        }
    }

    #[cfg(feature = "wgpu")]
    fn new_wgpu(cc: &eframe::CreationContext<'_>, track: WatchTrack) -> Self {
        let size = egui::vec2(100., 100.);
        let color_image =
            egui::ColorImage::filled([size.x as usize, size.y as usize], Color32::BLACK);
        let texture =
            cc.egui_ctx
                .load_texture("video", color_image, egui::TextureOptions::default());

        let wgpu_state = cc.wgpu_render_state.as_ref().map(|rs| {
            let renderer =
                moq_media::render::WgpuVideoRenderer::new(rs.device.clone(), rs.queue.clone());
            WgpuState {
                renderer,
                render_state: rs.clone(),
                texture_id: None,
                last_frame_size: None,
            }
        });

        Self {
            size,
            texture,
            track,
            wgpu_state,
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
            #[cfg(feature = "wgpu")]
            if let Some(ref mut wgpu) = self.wgpu_state {
                // wgpu rendering path
                let view = wgpu.renderer.render(&frame);
                let mut egui_renderer = wgpu.render_state.renderer.write();
                if let Some(id) = wgpu.texture_id {
                    egui_renderer.update_egui_texture_from_wgpu_texture(
                        &wgpu.render_state.device,
                        view,
                        wgpu::FilterMode::Linear,
                        id,
                    );
                } else {
                    let id = egui_renderer.register_native_texture(
                        &wgpu.render_state.device,
                        view,
                        wgpu::FilterMode::Linear,
                    );
                    wgpu.texture_id = Some(id);
                }
                wgpu.last_frame_size = Some(frame.dimensions());
                drop(egui_renderer);
                if let Some(id) = wgpu.texture_id {
                    let (w, h) = frame.dimensions();
                    return egui::Image::from_texture(egui::load::SizedTexture::new(
                        id,
                        [w as f32, h as f32],
                    ))
                    .shrink_to_fit();
                }
            }

            // CPU fallback path
            let (w, h) = frame.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                frame.img().as_raw(),
            );
            self.texture.set(image, Default::default());
        }

        // When using wgpu, return the last wgpu texture even if no new frame arrived.
        #[cfg(feature = "wgpu")]
        if let Some(ref wgpu) = self.wgpu_state {
            if let (Some(id), Some((w, h))) = (wgpu.texture_id, wgpu.last_frame_size) {
                return egui::Image::from_texture(egui::load::SizedTexture::new(
                    id,
                    [w as f32, h as f32],
                ))
                .shrink_to_fit();
            }
        }

        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}
