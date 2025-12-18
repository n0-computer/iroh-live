use std::time::Duration;

use clap::Parser;
use eframe::egui::{self, Color32, Id, Vec2};
use iroh::{Endpoint, protocol::Router};
use iroh_gossip::{Gossip, TopicId};
use iroh_live::{
    Live,
    audio::AudioBackend,
    av::{AudioPreset, VideoPreset},
    capture::{CameraCapturer, ScreenCapturer},
    ffmpeg::{FfmpegDecoders, FfmpegVideoDecoder, H264Encoder, OpusEncoder, ffmpeg_log_init},
    moq::MoqSession,
    publish::{AudioRenditions, PublishBroadcast, VideoRenditions},
    rooms::{Room, RoomEvent, RoomTicket},
    subscribe::{AudioTrack, AvRemoteTrack, SubscribeBroadcast, WatchTrack},
    util::StatsSmoother,
};
use n0_error::{Result, StdResultExt, anyerr};
use tracing::{info, warn};

const BROADCAST_NAME: &str = "cam";

#[derive(Debug, Parser)]
struct Cli {
    join: Option<RoomTicket>,
    #[clap(long)]
    screen: bool,
    #[clap(long)]
    no_audio: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    ffmpeg_log_init();
    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let audio_ctx = AudioBackend::new();
    let (router, broadcast, room) = rt.block_on(setup(cli, audio_ctx.clone()))?;

    let _guard = rt.enter();
    eframe::run_native(
        "IrohLive",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            let app = App {
                rt,
                room,
                peers: vec![],
                self_video: broadcast
                    .watch_local(Default::default())
                    .map(|track| VideoView::new(&cc.egui_ctx, track, usize::MAX)),
                router,
                _broadcast: broadcast,
                audio_ctx,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

async fn setup(cli: Cli, audio_ctx: AudioBackend) -> Result<(Router, PublishBroadcast, Room)> {
    let endpoint = Endpoint::builder()
        .secret_key(secret_key_from_env()?)
        .bind()
        .await?;
    info!(endpoint_id=%endpoint.id(), "endpoint bound");

    let gossip = Gossip::builder().spawn(endpoint.clone());
    let live = Live::new(endpoint.clone());

    let router = Router::builder(endpoint)
        .accept(iroh_gossip::ALPN, gossip.clone())
        .accept(iroh_moq::ALPN, live.protocol_handler())
        .spawn();

    // Publish ourselves.
    let broadcast = {
        let mut broadcast = PublishBroadcast::new();
        if !cli.no_audio {
            let mic = audio_ctx.default_input().await?;
            let audio = AudioRenditions::new::<OpusEncoder>(mic, [AudioPreset::Hq]);
            broadcast.set_audio(Some(audio))?;
        }
        let video = if cli.screen {
            let screen = ScreenCapturer::new()?;
            VideoRenditions::new::<H264Encoder>(screen, VideoPreset::all())
        } else {
            let camera = CameraCapturer::new()?;
            VideoRenditions::new::<H264Encoder>(camera, VideoPreset::all())
        };
        broadcast.set_video(Some(video))?;
        broadcast
    };
    let ticket = match cli.join {
        None => RoomTicket::new(topic_id_from_env()?, vec![]),
        Some(ticket) => ticket,
    };

    let room = Room::new(router.endpoint(), gossip, live, ticket).await?;
    room.publish(BROADCAST_NAME, broadcast.producer()).await?;

    println!("room ticket: {}", room.ticket());

    Ok((router, broadcast, room))
}

struct App {
    room: Room,
    peers: Vec<RemoteTrackView>,
    self_video: Option<VideoView>,
    router: Router,
    _broadcast: PublishBroadcast,
    audio_ctx: AudioBackend,
    rt: tokio::runtime::Runtime,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30)); // min 30 fps

        // Remove closed peers.
        self.peers.retain(|track| !track.is_closed());

        // Add newly subscribed peers.
        while let Ok(event) = self.room.try_recv() {
            match event {
                RoomEvent::RemoteAnnounced { remote, broadcasts } => {
                    info!(
                        "peer announced: {} with broadcasts {broadcasts:?}",
                        remote.fmt_short(),
                    );
                }
                RoomEvent::RemoteConnected { session } => {
                    info!("peer connected: {}", session.conn().remote_id().fmt_short());
                }
                RoomEvent::BroadcastSubscribed { session, broadcast } => {
                    info!(
                        "subscribing to {}:{}",
                        session.remote_id(),
                        broadcast.broadcast_name()
                    );
                    let track = match self.rt.block_on(async {
                        let audio_out = self.audio_ctx.default_output().await?;
                        broadcast.watch_and_listen::<FfmpegDecoders>(audio_out, Default::default())
                    }) {
                        Ok(track) => track,
                        Err(err) => {
                            warn!("failed to add track: {err}");
                            continue;
                        }
                    };

                    self.peers
                        .push(RemoteTrackView::new(ctx, session, track, self.peers.len()));
                }
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                show_video_grid(ctx, ui, &mut self.peers);

                // Render video preview of self
                if let Some(self_view) = self.self_video.as_mut() {
                    let size = (200., 200.);
                    egui::Area::new(Id::new("self-video"))
                        .anchor(egui::Align2::RIGHT_BOTTOM, [-10.0, -10.0]) // 10px from the bottom-right edge
                        .order(egui::Order::Foreground)
                        .show(ui.ctx(), |ui| {
                            egui::Frame::new()
                                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128))
                                .corner_radius(8.0)
                                .show(ui, |ui| {
                                    ui.set_width(size.0);
                                    ui.set_height(size.1);
                                    ui.add_sized(size, self_view.render_image(ctx, size.into()));
                                });
                        });
                }
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let router = self.router.clone();
        self.rt.block_on(async move {
            if let Err(err) = router.shutdown().await {
                warn!("shutdown error: {err:?}");
            }
        });
    }
}

struct RemoteTrackView {
    id: usize,
    video: Option<VideoView>,
    _audio_track: Option<AudioTrack>,
    session: MoqSession,
    broadcast: SubscribeBroadcast,
    stats: StatsSmoother,
}

impl RemoteTrackView {
    fn new(ctx: &egui::Context, session: MoqSession, track: AvRemoteTrack, id: usize) -> Self {
        Self {
            video: track.video.map(|video| VideoView::new(ctx, video, id)),
            stats: StatsSmoother::new(),
            broadcast: track.broadcast,
            id,
            _audio_track: track.audio,
            session,
        }
    }

    fn is_closed(&self) -> bool {
        self.session.conn().close_reason().is_some()
    }

    fn render_image(
        &mut self,
        ctx: &egui::Context,
        available_size: Vec2,
    ) -> Option<egui::Image<'_>> {
        self.video
            .as_mut()
            .map(|video| video.render_image(ctx, available_size))
    }

    fn render_overlay_in_rect(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let pos = rect.left_bottom() + egui::vec2(8.0, -8.0);
        let overlay_id = egui::Id::new(("overlay", self.id));

        egui::Area::new(overlay_id)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128))
                    .corner_radius(3.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                        ui.set_min_width(100.);
                        self.render_overlay(ui);
                    });
            });
    }

    fn render_overlay(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            let selected = self.video.as_ref().map(|v| v.track.rendition().to_owned());
            egui::ComboBox::from_id_salt(format!("video{}", self.id))
                .selected_text(selected.clone().unwrap_or_default())
                .show_ui(ui, |ui| {
                    for name in self.broadcast.catalog().video_renditions() {
                        if ui
                            .selectable_label(selected.as_deref() == Some(name), name)
                            .clicked()
                        {
                            if let Ok(track) = self
                                .broadcast
                                .watch_rendition::<FfmpegVideoDecoder>(&Default::default(), name)
                            {
                                if let Some(video) = self.video.as_mut() {
                                    video.set_track(track);
                                } else {
                                    self.video = Some(VideoView::new(ui.ctx(), track, self.id))
                                }
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
    size: egui::Vec2,
    texture: egui::TextureHandle,
}

impl VideoView {
    fn new(ctx: &egui::Context, track: WatchTrack, id: usize) -> Self {
        let texture_name = format!("video-texture-{}", id);
        let size = egui::vec2(100., 100.);
        let color_image =
            egui::ColorImage::filled([size.x as usize, size.y as usize], Color32::BLACK);
        let texture = ctx.load_texture(&texture_name, color_image, egui::TextureOptions::default());
        Self {
            size,
            texture,
            track,
        }
    }

    fn set_track(&mut self, track: WatchTrack) {
        self.track = track;
    }

    fn render_image(&mut self, ctx: &egui::Context, available_size: Vec2) -> egui::Image<'_> {
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

/// Show `textures` as squares in a compact auto grid that fills the parent as much as
/// possible without breaking square aspect.
fn show_video_grid(ctx: &egui::Context, ui: &mut egui::Ui, videos: &mut [RemoteTrackView]) {
    let n = videos.len();
    if n == 0 {
        return;
    }

    // Parent size we’re allowed to use
    let avail = ui.available_size(); // egui docs recommend this for filling containers
    // Choose columns ≈ ceil(sqrt(n)), rows to fit the rest
    let cols = (n as f32).sqrt().ceil() as usize;
    let rows = (n + cols - 1) / cols;

    // Side length of each square in points (fill the limiting axis)
    let cell = (avail.x / cols as f32).min(avail.y / rows as f32).floor();
    let cell_size = [cell, cell];

    // Compute the grid’s actual pixel footprint
    let grid_w = cell * cols as f32;
    let grid_h = cell * rows as f32;

    // Center the grid in any leftover space
    let pad_x = ((avail.x - grid_w) * 0.5).max(0.0);
    let pad_y = ((avail.y - grid_h) * 0.5).max(0.0);

    ui.add_space(pad_y);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);

        egui::Grid::new("image_grid")
            .spacing(Vec2::ZERO) // no gaps; tiles butt together
            .show(ui, |ui| {
                let mut i = 0;
                for _r in 0..rows {
                    for _c in 0..cols {
                        if i < n {
                            // Force exact square size for each image
                            if let Some(image) = videos[i].render_image(ctx, cell_size.into()) {
                                let response = ui.add_sized(cell_size, image);
                                let rect = response.rect;
                                videos[i].render_overlay_in_rect(ui, rect);
                            }
                            i += 1;
                        } else {
                            // Keep the grid rectangular when N isn’t a multiple of cols
                            ui.allocate_exact_size(Vec2::splat(cell), egui::Sense::hover());
                        }
                    }
                    ui.end_row();
                }
            });
    });
}

fn secret_key_from_env() -> n0_error::Result<iroh::SecretKey> {
    Ok(match std::env::var("IROH_SECRET") {
        Ok(key) => key.parse()?,
        Err(_) => {
            let key = iroh::SecretKey::generate(&mut rand::rng());
            println!(
                "Created new secret. Reuse with IROH_SECRET={}",
                data_encoding::HEXLOWER.encode(&key.to_bytes())
            );
            key
        }
    })
}

fn topic_id_from_env() -> n0_error::Result<TopicId> {
    Ok(match std::env::var("IROH_TOPIC") {
        Ok(topic) => TopicId::from_bytes(
            data_encoding::HEXLOWER
                .decode(topic.as_bytes())
                .std_context("invalid hex")?
                .as_slice()
                .try_into()
                .std_context("invalid length")?,
        ),
        Err(_) => {
            let topic = TopicId::from_bytes(rand::random());
            println!(
                "Created new topic. Reuse with IROH_TOPIC={}",
                data_encoding::HEXLOWER.encode(topic.as_bytes())
            );
            topic
        }
    })
}
