//! `irl room` — multi-party room with video grid and text chat.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use eframe::egui::{self, Id, Vec2};
use iroh::EndpointId;
use iroh_gossip::TopicId;
use iroh_live::{
    Live,
    media::{
        AudioBackend,
        publish::LocalBroadcast,
        subscribe::{AudioTrack, MediaTracks, RemoteBroadcast},
    },
    moq::MoqSession,
    rooms::{Room, RoomEvent, RoomHandle, RoomTicket},
};
use moq_media_egui::VideoTrackView;
use n0_error::{Result, StdResultExt, anyerr};
use tracing::{info, warn};

use crate::args::RoomArgs;

const BROADCAST_NAME: &str = "cam";

/// Maximum number of chat messages retained in the UI scrollback.
const MAX_CHAT_HISTORY: usize = 200;

pub fn run(args: RoomArgs, rt: &tokio::runtime::Runtime) -> Result<()> {
    let (live, broadcast, room, audio_ctx) = rt.block_on(async {
        let audio_ctx = AudioBackend::default();
        let (live, broadcast, room) = setup(&args, audio_ctx.clone()).await?;
        n0_error::Ok((live, broadcast, room, audio_ctx))
    })?;

    let (events, handle) = room.split();

    // eframe runs outside block_on.
    let _guard = rt.enter();
    eframe::run_native(
        "irl room",
        eframe::NativeOptions::default(),
        Box::new(|cc| {
            crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);

            Ok(Box::new(RoomApp {
                events,
                handle,
                peers: vec![],
                pending: tokio::task::JoinSet::new(),
                self_video: broadcast
                    .preview()
                    .map(|track| VideoTrackView::new(&cc.egui_ctx, "self-video", track)),
                live,
                _broadcast: broadcast,
                audio_ctx,
                self_view_mode: SelfViewMode::Grid,
                wgpu_render_state: cc.wgpu_render_state.clone(),
                chat: ChatState::default(),
            }))
        }),
    )
    .map_err(|err| anyerr!("eframe failed: {err:#}"))
}

async fn setup(args: &RoomArgs, audio_ctx: AudioBackend) -> Result<(Live, LocalBroadcast, Room)> {
    let live = Live::from_env().await?.with_router().with_gossip().spawn();

    let mut broadcast = LocalBroadcast::new();
    args.capture.setup_broadcast(&broadcast, &audio_ctx).await?;

    // Enable chat on the broadcast so subscribers can read our messages.
    let chat_publisher = broadcast.enable_chat()?;

    let ticket = match &args.ticket {
        Some(t) => t.clone(),
        None => RoomTicket::new(topic_id_from_env()?, vec![]),
    };
    let room = live.join_room(ticket).await?;
    room.publish(BROADCAST_NAME, &broadcast).await?;
    room.set_chat_publisher(chat_publisher).await?;
    println!("room ticket: {}", room.ticket());

    Ok((live, broadcast, room))
}

// ---------------------------------------------------------------------------
// Chat state
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ChatState {
    messages: VecDeque<ChatEntry>,
    input: String,
    /// Whether the text input currently has focus (used to scope Enter key).
    input_focused: bool,
}

struct ChatEntry {
    sender: String,
    text: String,
    _received_at: Instant,
    is_system: bool,
}

impl ChatState {
    fn push_message(&mut self, sender: impl Into<String>, text: impl Into<String>) {
        self.messages.push_back(ChatEntry {
            sender: sender.into(),
            text: text.into(),
            _received_at: Instant::now(),
            is_system: false,
        });
        if self.messages.len() > MAX_CHAT_HISTORY {
            self.messages.pop_front();
        }
    }

    fn push_system(&mut self, text: impl Into<String>) {
        self.messages.push_back(ChatEntry {
            sender: String::new(),
            text: text.into(),
            _received_at: Instant::now(),
            is_system: true,
        });
        if self.messages.len() > MAX_CHAT_HISTORY {
            self.messages.pop_front();
        }
    }
}

// ---------------------------------------------------------------------------
// Self-view toggle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum SelfViewMode {
    Grid,
    Pip,
}

// ---------------------------------------------------------------------------
// Remote peer track
// ---------------------------------------------------------------------------

struct RemoteTrackView {
    _id: usize,
    video: Option<VideoTrackView>,
    _audio_track: Option<AudioTrack>,
    session: MoqSession,
    broadcast: RemoteBroadcast,
    overlay: moq_media_egui::overlay::DebugOverlay,
}

impl RemoteTrackView {
    fn new(
        ctx: &egui::Context,
        session: MoqSession,
        track: MediaTracks,
        id: usize,
        wgpu_render_state: Option<&moq_media_egui::egui_wgpu::RenderState>,
    ) -> Self {
        // TODO(ER2): Room events deliver session + broadcast separately, so we
        // can't use Subscription here yet. Manual stats wiring stays until Room
        // events are updated to emit Subscriptions directly.
        iroh_live::util::spawn_stats_recorder(
            session.conn(),
            track.broadcast.stats().net.clone(),
            track.broadcast.shutdown_token(),
        );
        Self {
            video: track.video.map(|v| {
                VideoTrackView::new_wgpu(ctx, &format!("video-{id}"), v, wgpu_render_state)
            }),
            overlay: moq_media_egui::overlay::DebugOverlay::new(&[
                moq_media_egui::overlay::StatCategory::Net,
                moq_media_egui::overlay::StatCategory::Render,
            ]),
            broadcast: track.broadcast,
            _id: id,
            _audio_track: track.audio,
            session,
        }
    }

    fn is_closed(&self) -> bool {
        self.session.conn().close_reason().is_some()
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct RoomApp {
    events: iroh_live::rooms::RoomEvents,
    handle: RoomHandle,
    peers: Vec<RemoteTrackView>,
    /// Pending async media subscriptions for newly joined peers.
    pending: tokio::task::JoinSet<anyhow::Result<(MoqSession, MediaTracks)>>,
    self_video: Option<VideoTrackView>,
    live: Live,
    _broadcast: LocalBroadcast,
    audio_ctx: AudioBackend,
    self_view_mode: SelfViewMode,
    wgpu_render_state: Option<moq_media_egui::egui_wgpu::RenderState>,
    chat: ChatState,
}

impl RoomApp {
    fn send_chat(&mut self) {
        let text = self.chat.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.chat.input.clear();
        self.chat.push_message("me", &text);
        let handle = self.handle.clone();
        tokio::spawn(async move {
            if let Err(err) = handle.send_chat(text).await {
                warn!("failed to send chat: {err:#}");
            }
        });
    }
}

fn peer_short(remote: EndpointId) -> String {
    remote.fmt_short().to_string()
}

impl eframe::App for RoomApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30));
        self.peers.retain(|t| !t.is_closed());

        // Collect completed peer subscriptions from the JoinSet.
        while let Some(result) = self.pending.try_join_next() {
            match result {
                Ok(Ok((session, tracks))) => {
                    self.peers.push(RemoteTrackView::new(
                        ctx,
                        session,
                        tracks,
                        self.peers.len(),
                        self.wgpu_render_state.as_ref(),
                    ));
                }
                Ok(Err(e)) => warn!("failed to subscribe to peer: {e:#}"),
                Err(e) => warn!("peer subscribe task panicked: {e}"),
            }
        }

        // Dispatch new room events.
        while let Ok(event) = self.events.try_recv() {
            match event {
                RoomEvent::RemoteAnnounced {
                    remote, broadcasts, ..
                } => {
                    info!("peer announced: {} with {broadcasts:?}", remote.fmt_short());
                }
                RoomEvent::BroadcastSubscribed { session, broadcast } => {
                    info!(
                        "subscribing to {}:{}",
                        session.remote_id(),
                        broadcast.broadcast_name()
                    );
                    let audio_ctx = self.audio_ctx.clone();
                    self.pending.spawn(async move {
                        let tracks = broadcast.media(&audio_ctx, Default::default()).await?;
                        Ok((*session, tracks))
                    });
                }
                RoomEvent::PeerJoined {
                    remote,
                    display_name,
                } => {
                    let name = display_name.unwrap_or_else(|| peer_short(remote));
                    self.chat.push_system(format!("{name} joined"));
                }
                RoomEvent::PeerLeft { remote } => {
                    self.chat
                        .push_system(format!("{} left", peer_short(remote)));
                }
                RoomEvent::ChatReceived { remote, message } => {
                    self.chat.push_message(peer_short(remote), message.text);
                }
                _ => {}
            }
        }

        // Layout: video grid on top, chat panel on bottom.
        let chat_panel_h = 160.0;
        let bar_h = 28.0;

        egui::TopBottomPanel::bottom("chat_panel")
            .exact_height(chat_panel_h)
            .show(ctx, |ui| {
                show_chat(ui, &mut self.chat);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(0.0).outer_margin(0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                let avail = ui.available_size();
                let grid_avail = egui::vec2(avail.x, avail.y - bar_h);

                if self.self_view_mode == SelfViewMode::Grid {
                    show_grid_with_self(ctx, ui, &mut self.peers, &mut self.self_video, grid_avail);
                } else {
                    show_grid(ctx, ui, &mut self.peers, grid_avail);
                    if let Some(sv) = self.self_video.as_mut() {
                        let pip = egui::vec2(200.0, 200.0);
                        egui::Area::new(Id::new("self-video"))
                            .anchor(egui::Align2::RIGHT_BOTTOM, [-10.0, -bar_h - 10.0])
                            .order(egui::Order::Foreground)
                            .show(ctx, |ui| {
                                egui::Frame::new()
                                    .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128))
                                    .corner_radius(8.0)
                                    .show(ui, |ui| {
                                        ui.set_width(pip.x);
                                        ui.set_height(pip.y);
                                        let (img, _) = sv.render(ctx, pip);
                                        ui.add_sized(pip, img);
                                    });
                            });
                    }
                }

                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label("Self view:");
                    if ui
                        .selectable_label(self.self_view_mode == SelfViewMode::Grid, "grid")
                        .clicked()
                    {
                        self.self_view_mode = SelfViewMode::Grid;
                    }
                    if ui
                        .selectable_label(self.self_view_mode == SelfViewMode::Pip, "pip")
                        .clicked()
                    {
                        self.self_view_mode = SelfViewMode::Pip;
                    }
                });
            });

        // Handle Enter key for chat send — only when the text input has focus.
        if self.chat.input_focused && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.send_chat();
        }
    }

    fn on_exit(&mut self) {
        info!("exit");
        crate::ui::shutdown_live_blocking(&self.live);
    }
}

// ---------------------------------------------------------------------------
// Chat panel
// ---------------------------------------------------------------------------

fn show_chat(ui: &mut egui::Ui, chat: &mut ChatState) {
    ui.vertical(|ui| {
        // Message history: scrollable area.
        let input_h = 24.0;
        let avail = ui.available_height() - input_h - 4.0;
        egui::ScrollArea::vertical()
            .max_height(avail)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for entry in &chat.messages {
                    if entry.is_system {
                        ui.label(
                            egui::RichText::new(&entry.text)
                                .italics()
                                .color(egui::Color32::GRAY),
                        );
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{}:", entry.sender)).strong());
                            ui.label(&entry.text);
                        });
                    }
                }
            });

        // Input field.
        ui.horizontal(|ui| {
            let response = ui.add_sized(
                [ui.available_width() - 60.0, input_h],
                egui::TextEdit::singleline(&mut chat.input).hint_text("Type a message..."),
            );
            chat.input_focused = response.has_focus();
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                // Re-focus so the user can keep typing after sending.
                response.request_focus();
                chat.input_focused = true;
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Grid layout helpers
// ---------------------------------------------------------------------------

fn grid_layout(n: usize, avail: Vec2) -> (usize, usize, f32) {
    if n == 0 {
        return (0, 0, 0.0);
    }
    let cols = (n as f32).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let cell = (avail.x / cols as f32).min(avail.y / rows as f32).floor();
    (cols, rows, cell)
}

fn show_grid(ctx: &egui::Context, ui: &mut egui::Ui, peers: &mut [RemoteTrackView], avail: Vec2) {
    let n = peers.len();
    let (cols, rows, cell) = grid_layout(n, avail);
    if n == 0 {
        return;
    }
    let cell_size: Vec2 = [cell, cell].into();
    let pad_x = ((avail.x - cell * cols as f32) * 0.5).max(0.0);
    let pad_y = ((avail.y - cell * rows as f32) * 0.5).max(0.0);

    ui.add_space(pad_y);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        egui::Grid::new("room_grid")
            .spacing(Vec2::ZERO)
            .show(ui, |ui| {
                let mut i = 0;
                for _r in 0..rows {
                    for _c in 0..cols {
                        if i < n {
                            if let Some(img) =
                                peers[i].video.as_mut().map(|v| v.render(ctx, cell_size).0)
                            {
                                let resp = ui.add_sized(cell_size, img);
                                peers[i]
                                    .overlay
                                    .show(ui, resp.rect, peers[i].broadcast.stats());
                            }
                        } else {
                            ui.allocate_exact_size(cell_size, egui::Sense::hover());
                        }
                        i += 1;
                    }
                    ui.end_row();
                }
            });
    });
}

fn show_grid_with_self(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    peers: &mut [RemoteTrackView],
    self_video: &mut Option<VideoTrackView>,
    avail: Vec2,
) {
    let n = peers.len() + usize::from(self_video.is_some());
    let (cols, rows, cell) = grid_layout(n, avail);
    if n == 0 {
        return;
    }
    let cell_size: Vec2 = [cell, cell].into();
    let pad_x = ((avail.x - cell * cols as f32) * 0.5).max(0.0);
    let pad_y = ((avail.y - cell * rows as f32) * 0.5).max(0.0);

    ui.add_space(pad_y);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        egui::Grid::new("room_grid_self")
            .spacing(Vec2::ZERO)
            .show(ui, |ui| {
                let total_peers = peers.len();
                let mut i = 0;
                for _r in 0..rows {
                    for _c in 0..cols {
                        if i < total_peers {
                            if let Some(img) =
                                peers[i].video.as_mut().map(|v| v.render(ctx, cell_size).0)
                            {
                                let resp = ui.add_sized(cell_size, img);
                                peers[i]
                                    .overlay
                                    .show(ui, resp.rect, peers[i].broadcast.stats());
                            }
                        } else if i == total_peers {
                            if let Some(sv) = self_video {
                                let (img, _) = sv.render(ctx, cell_size);
                                ui.add_sized(cell_size, img);
                            } else {
                                ui.allocate_exact_size(cell_size, egui::Sense::hover());
                            }
                        } else {
                            ui.allocate_exact_size(cell_size, egui::Sense::hover());
                        }
                        i += 1;
                    }
                    ui.end_row();
                }
            });
    });
}

fn topic_id_from_env() -> Result<TopicId> {
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
