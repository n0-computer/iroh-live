use std::time::Duration;

use clap::Parser;
use eframe::egui::{self, Id, Vec2};
use iroh::{Endpoint, Watcher};
use iroh_gossip::TopicId;
use iroh_live::{
    Live,
    media::stats::MetricsCollector,
    media::{
        audio_backend::AudioBackend,
        capture::{CameraCapturer, ScreenCapturer},
        codec::{AudioCodec, DefaultDecoders, DynamicVideoDecoder, VideoCodec},
        format::{AudioPreset, VideoPreset},
        publish::{AudioRenditions, LocalBroadcast, VideoRenditions},
        subscribe::{AudioTrack, MediaTracks, RemoteBroadcast},
    },
    moq::MoqSession,
    rooms::{Room, RoomEvent, RoomTicket},
};
use moq_media_egui::VideoTrackView;
use n0_error::{Result, StdResultExt, anyerr};
use tracing::{info, warn};

mod common;

const BROADCAST_NAME: &str = "cam";

#[derive(Debug, Parser)]
struct Cli {
    join: Option<RoomTicket>,
    #[arg(long, default_value_t=VideoCodec::best_available().expect("no video codec available"), value_parser = clap_enum_variants!(VideoCodec))]
    codec: VideoCodec,
    #[clap(long)]
    screen: bool,
    #[clap(long)]
    no_audio: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let audio_ctx = AudioBackend::default();
    let (live, broadcast, room) = rt.block_on(setup(cli, audio_ctx.clone()))?;

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
                    .preview(Default::default())
                    .map(|track| VideoTrackView::new(&cc.egui_ctx, "self-video", track)),
                live,
                _broadcast: broadcast,
                audio_ctx,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

async fn setup(cli: Cli, audio_ctx: AudioBackend) -> Result<(Live, LocalBroadcast, Room)> {
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key_from_env()?)
        .bind()
        .await?;
    info!(endpoint_id=%endpoint.id(), "endpoint bound");

    let live = Live::builder(endpoint).enable_gossip().spawn_with_router();

    // Publish ourselves.
    let broadcast = {
        let broadcast = LocalBroadcast::new();
        if !cli.no_audio {
            let mic = audio_ctx.default_input().await?;
            let audio = AudioRenditions::new(mic, AudioCodec::Opus, [AudioPreset::Hq]);
            broadcast.audio().set_renditions(audio)?;
        }
        let video = if cli.screen {
            let screen = ScreenCapturer::new()?;
            VideoRenditions::new(screen, cli.codec, VideoPreset::all())
        } else {
            let camera = CameraCapturer::new()?;
            VideoRenditions::new(camera, cli.codec, VideoPreset::all())
        };
        broadcast.video().set_renditions(video)?;
        broadcast
    };
    let ticket = match cli.join {
        None => RoomTicket::new(topic_id_from_env()?, vec![]),
        Some(ticket) => ticket,
    };

    let room = live.join_room(ticket).await?;
    room.publish(BROADCAST_NAME, broadcast.producer()).await?;

    println!("room ticket: {}", room.ticket());

    Ok((live, broadcast, room))
}

struct App {
    room: Room,
    peers: Vec<RemoteTrackView>,
    self_video: Option<VideoTrackView>,
    live: Live,
    _broadcast: LocalBroadcast,
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
                        broadcast
                            .media::<DefaultDecoders>(&self.audio_ctx, Default::default())
                            .await
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
                                    let (img, _) = self_view.render(ctx, size.into());
                                    ui.add_sized(size, img);
                                });
                        });
                }
            });
    }

    fn on_exit(&mut self) {
        let live = self.live.clone();
        self.rt.block_on(async move { live.shutdown().await });
    }
}

struct RemoteTrackView {
    id: usize,
    video: Option<VideoTrackView>,
    _audio_track: Option<AudioTrack>,
    session: MoqSession,
    broadcast: RemoteBroadcast,
    metrics: MetricsCollector,
    overlay: moq_media_egui::overlay::DebugOverlay,
}

impl RemoteTrackView {
    fn new(ctx: &egui::Context, session: MoqSession, mut track: MediaTracks, id: usize) -> Self {
        let metrics = MetricsCollector::new();
        metrics.register_defaults();
        track.broadcast.set_metrics(metrics.clone());
        iroh_live::util::spawn_stats_recorder(
            session.conn(),
            metrics.clone(),
            track.broadcast.shutdown_token(),
        );
        Self {
            video: track
                .video
                .map(|video| VideoTrackView::new(ctx, &format!("video-{id}"), video)),
            metrics,
            overlay: moq_media_egui::overlay::DebugOverlay::new(&[
                moq_media_egui::overlay::StatCategory::Net,
                moq_media_egui::overlay::StatCategory::Render,
            ]),
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
            .map(|video| video.render(ctx, available_size).0)
    }

    fn render_overlay_in_rect(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let snap = self.metrics.snapshot();
        self.overlay.show(ui, rect, &snap);
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
    let rows = n.div_ceil(cols);

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
    iroh_live::util::secret_key_from_env()
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
