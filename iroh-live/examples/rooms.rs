use std::time::Duration;

use clap::Parser;
use ed25519_dalek::SecretKey;
use eframe::egui::{self, Color32, Id, Vec2};
use iroh::{Endpoint, EndpointId, protocol::Router};
use iroh_gossip::{Gossip, TopicId};
use iroh_live::{
    audio::AudioBackend,
    capture::{CameraCapturer, ScreenCapturer},
    ffmpeg::{FfmpegAudioDecoder, FfmpegVideoDecoder, H264Encoder, OpusEncoder, ffmpeg_log_init},
};
use iroh_moq::{
    Live, LiveSession, LiveTicket,
    av::{AudioPreset, VideoPreset},
    publish::{AudioRenditions, PublishBroadcast, VideoRenditions},
    subscribe::{AudioTrack, SubscribeBroadcast, WatchTrack},
};
use moq_lite::ietf::Subscribe;
use n0_error::{Result, StackResultExt, anyerr};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, error::TryRecvError};
use tracing::{info, warn};

const BROADCAST_NAME: &str = "cam";

#[derive(Debug, Serialize, Deserialize, Clone)]
struct RoomTicket {
    endpoint: EndpointId,
    topic: TopicId,
}

impl iroh_tickets::Ticket for RoomTicket {
    const KIND: &'static str = "room";

    fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).unwrap()
    }

    fn from_bytes(bytes: &[u8]) -> std::result::Result<Self, iroh_tickets::ParseError> {
        let ticket = postcard::from_bytes(bytes)?;
        Ok(ticket)
    }
}

#[derive(Debug, Parser)]
struct Cli {
    // room: String,
    join: Option<String>,
    #[clap(long)]
    screen: bool,
    #[clap(long)]
    no_audio: bool,
}

#[derive(Debug, Serialize, Deserialize)]
enum Message {
    Announce(EndpointId),
}

struct Track {
    video: WatchTrack,
    session: LiveSession,
    audio: AudioTrack,
    broadcast: SubscribeBroadcast,
}

impl Track {
    pub async fn connect(
        live: &Live,
        audio_ctx: &AudioBackend,
        endpoint_id: EndpointId,
    ) -> Result<Self> {
        let mut session = live.connect(endpoint_id).await?;
        info!(id=%session.conn().remote_id(), "new peer connected");
        let broadcast = session.subscribe(BROADCAST_NAME).await?;
        info!(id=%session.conn().remote_id(), "subscribed");
        let audio_out = audio_ctx.default_speaker().await?;
        let audio = broadcast.listen::<FfmpegAudioDecoder>(audio_out)?;
        let video = broadcast.watch::<FfmpegVideoDecoder>()?;

        Ok(Track {
            video,
            session,
            audio,
            broadcast,
        })
    }
}

impl Track {
    fn closed(&self) -> bool {
        self.session.conn().close_reason().is_some()
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    ffmpeg_log_init();
    let cli = Cli::parse();

    let audio_ctx = AudioBackend::new();
    let _audio_ctx = audio_ctx.clone();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let (router, track_rx, broadcast) = rt.block_on({
        let audio_ctx = audio_ctx.clone();
        async move {
            let secret_key = secret_key_from_env()?;
            let endpoint = Endpoint::builder().secret_key(secret_key).bind().await?;
            info!(endpoint_id=%endpoint.id(), "endpoint bound");
            let gossip = Gossip::builder().spawn(endpoint.clone());
            let live = Live::new(endpoint.clone());
            // let signing_key =
            //     ed25519_dalek::SigningKey::from_bytes(&endpoint.secret_key().to_bytes());
            let router = Router::builder(endpoint)
                .accept(iroh_gossip::ALPN, gossip.clone())
                .accept(iroh_moq::ALPN, live.protocol_handler())
                .spawn();

            let mut broadcast = PublishBroadcast::new(BROADCAST_NAME);

            // Audio: default microphone + Opus encoder with preset
            if !cli.no_audio {
                let mic = audio_ctx.default_microphone().await?;
                let audio = AudioRenditions::new::<OpusEncoder>(mic, [AudioPreset::Hq]);
                broadcast.set_audio(audio)?;
            }

            // Video: camera capture + encoders by backend (fps 30)
            let video = if cli.screen {
                let screen = ScreenCapturer::new()?;
                VideoRenditions::new::<H264Encoder>(screen, VideoPreset::all())
            } else {
                let camera = CameraCapturer::new()?;
                VideoRenditions::new::<H264Encoder>(camera, VideoPreset::all())
            };
            broadcast.set_video(video)?;
            live.publish(&broadcast).await?;

            // let initial_secret = b"my-initial-secret".to_vec();
            // let record_publisher = distributed_topic_tracker::RecordPublisher::new(
            //     topic_id.clone(),
            //     signing_key.verifying_key(),
            //     signing_key.clone(),
            //     None,
            //     initial_secret,
            // );

            // let topic = gossip
            //     .subscribe_and_join_with_auto_discovery_no_wait(record_publisher)
            //     .await?;
            //
            let ticket: Option<RoomTicket> = match cli.join {
                None => None,
                Some(ticket) => Some(iroh_tickets::Ticket::deserialize(&ticket)?),
            };
            let topic_id = match &ticket {
                None => TopicId::from_bytes(rand::random()),
                Some(ticket) => ticket.topic,
            };
            let bootstrap = match &ticket {
                None => vec![],
                Some(ticket) => vec![ticket.endpoint],
            };
            let topic = gossip.subscribe(topic_id, bootstrap).await?;
            info!("subscribed");
            let (gossip_send, mut gossip_recv) = topic.split();

            let new_ticket = RoomTicket {
                topic: topic_id,
                endpoint: router.endpoint().id(),
            };

            println!(
                "room ticket: {}",
                iroh_tickets::Ticket::serialize(&new_ticket)
            );

            let my_id = router.endpoint().id();
            tokio::task::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    // TODO: sign
                    let message = Message::Announce(my_id);
                    let message = postcard::to_stdvec(&message).unwrap();
                    if let Err(err) = gossip_send.broadcast(message.into()).await {
                        warn!("failed to broadcast on gossip: {err:?}");
                        break;
                    }
                }
            });

            let (announce_tx, mut announce_rx) = mpsc::channel(16);
            tokio::task::spawn(async move {
                while let Some(event) = gossip_recv.next().await {
                    let event = event?;
                    match event {
                        iroh_gossip::api::Event::Received(message) => {
                            let Ok(message) = postcard::from_bytes::<Message>(&message.content)
                            else {
                                continue;
                            };
                            match message {
                                Message::Announce(endpoint_id) => {
                                    if let Err(_) = announce_tx.send(endpoint_id).await {
                                        break;
                                    }
                                }
                            }
                        }
                        iroh_gossip::api::Event::NeighborUp(neighbor) => {
                            info!("gossip neighbor up: {neighbor}")
                        }
                        iroh_gossip::api::Event::NeighborDown(neighbor) => {
                            info!("gossip neighbor down: {neighbor}")
                        }
                        _ => {}
                    }
                }
                n0_error::Ok(())
            });

            let (track_tx, track_rx) = mpsc::channel(16);
            tokio::task::spawn(async move {
                while let Some(endpoint_id) = announce_rx.recv().await {
                    match Track::connect(&live, &audio_ctx, endpoint_id).await {
                        Err(err) => {
                            warn!(endpoint=%endpoint_id.fmt_short(), ?err, "failed to connect");
                        }
                        Ok(track) => {
                            if let Err(err) = track_tx.send(track).await {
                                warn!(?err, "failed to forward track, abort conect loop");
                                break;
                            } else {
                                info!("forwarded track");
                            }
                        }
                    }
                }
            });

            n0_error::Ok((router, track_rx, broadcast))
        }
    })?;

    let _guard = rt.enter();
    eframe::run_native(
        "IrohLive",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            let app = App {
                rt,
                track_rx,
                videos: vec![],
                router,
                broadcast,
                _audio_ctx,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

struct App {
    track_rx: mpsc::Receiver<Track>,
    videos: Vec<VideoView>,
    router: Router,
    broadcast: PublishBroadcast,
    _audio_ctx: AudioBackend,
    rt: tokio::runtime::Runtime,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30)); // min 30 fps
        self.videos.retain(|v| !v.track.closed());
        match self.track_rx.try_recv() {
            Ok(track) => {
                info!("adding new track");
                self.videos.push(VideoView::new(ctx, track));
            }
            Err(TryRecvError::Disconnected) => warn!("track receiver disconnected!"),
            Err(TryRecvError::Empty) => {}
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                ui.label(format!("videos: {}", self.videos.len()));
                // render video
                // let avail = ui.available_size();
                show_video_grid(ctx, ui, &mut self.videos);
                // ui.add_sized(avail, self.video.render(ctx, avail));

                // render overlay.
                // egui::Area::new(Id::new("overlay"))
                //     .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -8.0])
                //     .show(ctx, |ui| {
                //         egui::Frame::new()
                //             .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128))
                //             .corner_radius(3.0)
                //             .show(ui, |ui| {
                //                 ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                //                 ui.set_min_width(100.);
                //                 self.render_overlay(ctx, ui);
                //             })
                //     })
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

impl App {
    // fn render_overlay(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
    //     ui.vertical(|ui| {
    //         let selected = self.video.track.rendition().to_owned();
    //         egui::ComboBox::from_label("")
    //             .selected_text(selected.clone())
    //             .show_ui(ui, |ui| {
    //                 for name in self.broadcast.video_renditions() {
    //                     if ui.selectable_label(&selected == name, name).clicked() {
    //                         if let Ok(track) = self
    //                             .broadcast
    //                             .watch_rendition::<FfmpegVideoDecoder>(&Default::default(), &name)
    //                         {
    //                             self.video = VideoView::new(ctx, track);
    //                         }
    //                     }
    //                 }
    //             });

    //         let (rtt, bw) = self.stats.smoothed(|| self.session.stats());
    //         ui.label(format!("BW:  {bw}"));
    //         ui.label(format!("RTT: {}ms", rtt.as_millis()));
    //     });
    // }
}

struct VideoView {
    track: Track,
    texture: egui::TextureHandle,
    size: egui::Vec2,
}

impl VideoView {
    fn new(ctx: &egui::Context, track: Track) -> Self {
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
            self.track.video.set_viewport(w, h);
        }
        if let Some(frame) = self.track.video.current_frame() {
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

/// Show `textures` as squares in a compact auto grid that fills the parent as much as
/// possible without breaking square aspect.
fn show_video_grid(ctx: &egui::Context, ui: &mut egui::Ui, videos: &mut [VideoView]) {
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
                            ui.add_sized(cell_size, videos[i].render(ctx, cell_size.into()));
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

mod util {
    use byte_unit::{Bit, UnitType};
    use iroh::endpoint::ConnectionStats;
    use std::time::{Duration, Instant};

    pub struct StatsSmoother {
        last_bytes: u64,
        last_update: Instant,
        rate: String,
        rtt: Duration,
    }

    impl StatsSmoother {
        pub fn new() -> Self {
            Self {
                last_bytes: 0,
                last_update: Instant::now(),
                rate: "0.00 bit/s".into(),
                rtt: Duration::from_secs(0),
            }
        }
        pub fn smoothed(&mut self, total: impl FnOnce() -> ConnectionStats) -> (Duration, &str) {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last_update);
            if elapsed >= Duration::from_secs(1) {
                let stats = (total)();
                let total = stats.udp_rx.bytes;
                let delta = total.saturating_sub(self.last_bytes);
                let secs = elapsed.as_secs_f64();
                let bps = if secs > 0.0 && delta > 0 {
                    (delta as f64 * 8.0) / secs
                } else {
                    0.0
                };
                let bit = Bit::from_f64(bps).unwrap();
                let adjusted = bit.get_appropriate_unit(UnitType::Decimal);
                self.rate = format!("{adjusted:.2}/s");
                self.last_update = now;
                self.last_bytes = total;
                self.rtt = stats.path.rtt;
            }
            (self.rtt, &self.rate)
        }
    }
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
