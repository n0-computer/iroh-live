//! `irl room`: a multi-party room, publishing one broadcast and watching
//! everyone else's.
//!
//! `iroh-rooms` does the discovery: peers announce the names of their
//! broadcasts on a shared gossip topic, and every name that appears comes back
//! as a MoQ subscription. This wraps each of those in a
//! [`RemoteBroadcast`](iroh_live::media::subscribe::RemoteBroadcast), lays them
//! out in a grid, and hands the chat track the room already knows about to the
//! panel at the bottom.
//!
//! Every participant subscribes to every other, so this is a small-group
//! design. There is no selective forwarding.

use iroh_live::{
    Live,
    media::{catalog::TrackRef, publish::LocalBroadcast},
};
use iroh_rooms::{
    Room, RoomTicket,
    chat::{CHAT_PRIORITY, CHAT_TRACK_NAME, ChatPublisher},
};
use n0_error::{Result, anyerr};
use tracing::info;

use crate::{args::RoomArgs, source, transport};

/// The name this node publishes its camera under inside the room.
///
/// Scoped to the room's gossip topic by `iroh-rooms`, so the same node can be
/// in several rooms at once without the names colliding.
const BROADCAST_NAME: &str = "cam";

/// Runs the `room` command.
pub fn run(args: RoomArgs, rt: &tokio::runtime::Runtime) -> Result {
    let (live, broadcast, room, ticket, display_name) = rt.block_on(setup(&args))?;

    // eframe takes the main thread from here on, so the runtime keeps its
    // workers only for as long as this guard lives.
    let _guard = rt.enter();
    window::run(live, broadcast, room, ticket, display_name, args.fullscreen)
}

/// Joins the room, publishes this node's camera into it, and prints the ticket
/// the next participant needs.
async fn setup(args: &RoomArgs) -> Result<(Live, LocalBroadcast, Room, String, String)> {
    let live = transport::setup_live_with_gossip().await?;
    let (live, (broadcast, room, ticket, display_name)) =
        transport::with_live(live, async |live| join(live, args).await).await?;
    Ok((live, broadcast, room, ticket, display_name))
}

/// Joins the room over `live`, which the caller closes if this fails.
///
/// # Errors
///
/// Fails if gossip is not running, if the room cannot be joined, or if the
/// capture sources do not parse.
async fn join(live: &Live, args: &RoomArgs) -> Result<(LocalBroadcast, Room, String, String)> {
    let gossip = live
        .gossip()
        .ok_or_else(|| anyerr!("gossip is not running, which a room cannot do without"))?;

    let ticket = args.ticket.clone().unwrap_or_else(RoomTicket::generate);
    let room = Room::new(live.endpoint(), live.transport(), gossip, ticket).await?;

    let broadcast = LocalBroadcast::new(room.publish(BROADCAST_NAME).await?)?;
    source::configure(&broadcast, &args.capture)?;

    // Chat rides on the same broadcast, so a peer subscribed for the video gets
    // the messages without a second subscription. `enable_chat` creates the
    // track and advertises it in the catalog; `iroh-rooms` finds it by the
    // well-known name it was created under.
    let chat = broadcast.enable_chat(TrackRef {
        name: CHAT_TRACK_NAME.to_string(),
        priority: CHAT_PRIORITY,
    })?;
    room.set_chat_publisher(ChatPublisher::new(chat)).await?;

    let display_name = args
        .display_name
        .clone()
        .unwrap_or_else(|| live.endpoint().id().fmt_short().to_string());
    room.set_display_name(display_name.clone()).await?;

    let ticket = room.ticket().to_string();
    println!("room ticket: {ticket}");
    transport::print_qr(&ticket, args.no_qr);
    info!(ticket, display_name, "joined the room");

    Ok((broadcast, room, ticket, display_name))
}

mod window {
    //! The room window: a grid of everybody's pictures over a chat panel.

    use std::{
        collections::{HashMap, VecDeque},
        time::Duration,
    };

    use eframe::egui;
    use iroh::EndpointId;
    use iroh_live::{
        Live, Subscription,
        media::{
            publish::LocalBroadcast,
            subscribe::{MediaTracks, RemoteBroadcast},
        },
        moq::MoqSession,
    };
    use iroh_rooms::{Room, RoomEvent, RoomEvents, RoomHandle};
    use moq_media_egui::egui_wgpu::RenderState;
    use n0_error::{Result, anyerr};
    use n0_future::task::AbortOnDropHandle;
    use tokio::task::JoinSet;
    use tracing::{info, warn};

    use crate::{
        transport::PEER_TIMEOUT,
        ui::{LocalPreview, RemoteView},
    };

    /// How often the window is woken while nothing is drawing it.
    ///
    /// The room actor writes its events into a bounded channel that only this
    /// window drains, so a window that stops running stops the room. A window
    /// nobody is looking at still has to keep the room's gossip moving.
    const HEARTBEAT: Duration = Duration::from_millis(100);

    /// How many chat lines are kept in the scrollback.
    const MAX_CHAT_LINES: usize = 200;

    /// Aspect ratio every tile is drawn at, whatever shape the picture in it
    /// happens to be.
    const ASPECT: f32 = 16.0 / 9.0;

    /// Height of the chat panel when the window opens, in points.
    const CHAT_HEIGHT: f32 = 160.0;

    /// Height of the chat input line, in points.
    const CHAT_INPUT_HEIGHT: f32 = 22.0;

    /// Opens the room window and runs it until it closes.
    pub(super) fn run(
        live: Live,
        broadcast: LocalBroadcast,
        room: Room,
        ticket: String,
        display_name: String,
        fullscreen: bool,
    ) -> Result {
        // Split so chat sends do not wait on the same task that drains the
        // events: the room actor replies through a channel only this window
        // reads, and the two would deadlock each other.
        let (events, handle) = room.split();

        eframe::run_native(
            "irl room",
            crate::ui::native_options(fullscreen),
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);
                Ok(Box::new(RoomApp {
                    live,
                    handle,
                    events,
                    ticket,
                    display_name,
                    peers: Vec::new(),
                    names: HashMap::new(),
                    opening: JoinSet::new(),
                    sending: JoinSet::new(),
                    chat: ChatState::default(),
                    preview: LocalPreview::new(
                        &cc.egui_ctx,
                        "room-preview",
                        cc.wgpu_render_state.as_ref(),
                    ),
                    render_state: cc.wgpu_render_state.clone(),
                    _heartbeat: crate::ui::spawn_heartbeat(&cc.egui_ctx, HEARTBEAT),
                    broadcast,
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    /// The room window.
    struct RoomApp {
        live: Live,
        /// This node's own broadcast, which every other participant subscribes
        /// to.
        broadcast: LocalBroadcast,
        handle: RoomHandle,
        events: RoomEvents,
        /// The room's ticket, shown in the top bar.
        ticket: String,
        /// The name this node announced, used to label its own chat lines.
        display_name: String,
        peers: Vec<PeerTile>,
        /// Display names by peer, from the gossip announcements.
        names: HashMap<EndpointId, String>,
        /// Subscriptions whose tracks are still opening.
        opening: JoinSet<Option<Opened>>,
        /// Chat messages still on their way to the room actor.
        sending: JoinSet<()>,
        chat: ChatState,
        preview: LocalPreview,
        render_state: Option<RenderState>,
        _heartbeat: AbortOnDropHandle<()>,
    }

    /// One participant's broadcast in the grid.
    struct PeerTile {
        remote: EndpointId,
        /// The name the peer announced the broadcast under, which is what
        /// tells two of a peer's tiles apart.
        name: String,
        view: RemoteView,
        /// Held so the session stays open and can be asked whether it closed.
        sub: Subscription,
    }

    /// A peer's broadcast, subscribed and decoding, on its way to the grid.
    struct Opened {
        remote: EndpointId,
        name: String,
        sub: Subscription,
        tracks: MediaTracks,
    }

    impl eframe::App for RoomApp {
        /// Drains the room and collects finished subscriptions.
        ///
        /// Here rather than in [`ui`](Self::ui) because eframe runs no egui
        /// pass while the window is minimized or occluded, and the room actor
        /// blocks on an event channel this is the only reader of.
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));
            self.drain_events();
            self.collect_opened(ctx);
            self.drop_closed();
            while self.sending.try_join_next().is_some() {}
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            self.preview.update(&ctx, &self.broadcast);

            egui::Panel::top("room-bar").show(ui, |ui| self.bar_ui(ui, &ctx));
            egui::Panel::bottom("room-chat")
                .resizable(true)
                .default_size(CHAT_HEIGHT)
                .show(ui, |ui| self.chat_ui(ui));
            egui::CentralPanel::default().show(ui, |ui| self.grid_ui(ui));
        }

        fn on_exit(&mut self) {
            info!("exit");
            for peer in &mut self.peers {
                peer.view.shutdown();
                peer.sub.session().close(moq_net::Error::Cancel);
            }
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }

    impl RoomApp {
        /// Applies whatever the room reported since the last pass.
        fn drain_events(&mut self) {
            while let Ok(event) = self.events.try_recv() {
                match event {
                    RoomEvent::PeerJoined {
                        remote,
                        display_name,
                    } => {
                        let name = display_name.unwrap_or_else(|| short(remote));
                        self.chat.push_system(format!("{name} joined"));
                        self.names.insert(remote, name);
                    }
                    RoomEvent::PeerLeft { remote } => {
                        let name = self.label(remote);
                        self.chat.push_system(format!("{name} left"));
                        self.names.remove(&remote);
                        self.close_tiles("the peer left", |peer| peer.remote == remote);
                    }
                    RoomEvent::BroadcastSubscribed {
                        remote,
                        name,
                        session,
                        broadcast,
                    } => {
                        info!(remote = %short(remote), %name, "subscribing to a peer");
                        self.opening.spawn(open(remote, name, *session, broadcast));
                    }
                    RoomEvent::ChatReceived { remote, message } => {
                        self.chat.push(self.label(remote), message.text);
                    }
                    RoomEvent::RemoteAnnounced { remote, broadcasts } => {
                        info!(remote = %short(remote), ?broadcasts, "peer announced");
                    }
                    // `RoomEvent` is non-exhaustive: a new variant is something
                    // this window has not been taught to draw yet.
                    _ => {}
                }
            }
        }

        /// Moves finished subscriptions into the grid.
        fn collect_opened(&mut self, ctx: &egui::Context) {
            while let Some(result) = self.opening.try_join_next() {
                let opened = match result {
                    Ok(Some(opened)) => opened,
                    Ok(None) => continue,
                    Err(err) => {
                        warn!(error = %err, "a peer subscription task panicked");
                        continue;
                    }
                };
                let Opened {
                    remote,
                    name,
                    sub,
                    tracks,
                } = opened;
                let view = RemoteView::new(
                    ctx,
                    &format!("{}-{name}", short(remote)),
                    sub.broadcast().clone(),
                    tracks,
                    sub.signals().clone(),
                    self.render_state.as_ref(),
                );
                self.peers.push(PeerTile {
                    remote,
                    name,
                    view,
                    sub,
                });
            }
        }

        /// Drops the tiles whose sessions have gone.
        ///
        /// `PeerLeft` covers a peer that ended its broadcast, but a peer whose
        /// connection simply failed leaves the tile behind, so the session is
        /// checked too.
        fn drop_closed(&mut self) {
            self.close_tiles("the session closed", |peer| {
                peer.sub.session().conn().close_reason().is_some()
            });
        }

        /// Removes the tiles `drop_it` picks out, shutting each one down first.
        ///
        /// Dropping a tile alone would stop its decoders but leave the
        /// subscription running, so a peer that went away would keep being
        /// downloaded. The session is left alone: a peer may hold several
        /// broadcasts on one, and closing it would take the siblings with it.
        fn close_tiles(&mut self, reason: &str, drop_it: impl Fn(&PeerTile) -> bool) {
            let mut index = 0;
            while index < self.peers.len() {
                if !drop_it(&self.peers[index]) {
                    index += 1;
                    continue;
                }
                let mut peer = self.peers.remove(index);
                info!(
                    remote = %short(peer.remote),
                    name = %peer.name,
                    reason,
                    "dropping a peer tile"
                );
                peer.view.shutdown();
            }
        }

        /// The name to show for `remote`, falling back to its short endpoint
        /// id.
        fn label(&self, remote: EndpointId) -> String {
            self.names
                .get(&remote)
                .cloned()
                .unwrap_or_else(|| short(remote))
        }

        /// Draws the top bar: the ticket to share and who is here.
        fn bar_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            ui.horizontal(|ui| {
                ui.label("Room ticket");
                if ui.button("Copy").clicked() {
                    ctx.copy_text(self.ticket.clone());
                }
                ui.separator();
                ui.label(format!("{} participants", self.peers.len() + 1));
            });
        }

        /// Draws the grid: this node's own picture first, then one tile per
        /// peer broadcast.
        fn grid_ui(&mut self, ui: &mut egui::Ui) {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let available = ui.available_size();
            let count = self.peers.len() + 1;
            let (cols, rows, cell) = layout(count, available);

            ui.add_space(((available.y - cell.y * rows as f32) * 0.5).max(0.0));
            let pad_x = ((available.x - cell.x * cols as f32) * 0.5).max(0.0);
            for row in 0..rows {
                ui.horizontal(|ui| {
                    ui.add_space(pad_x);
                    for col in 0..cols {
                        match row * cols + col {
                            index if index >= count => break,
                            0 => self.draw_self(ui, cell),
                            index => self.draw_peer(ui, index - 1, cell),
                        }
                    }
                });
            }
        }

        /// Draws this node's own picture as the first tile.
        fn draw_self(&mut self, ui: &mut egui::Ui, cell: egui::Vec2) {
            let response = ui.add_sized(cell, self.preview.image());
            tile_label(ui, response.rect, &format!("{} (you)", self.display_name));
        }

        /// Draws one peer's tile: the picture, its label, and the stats bar.
        fn draw_peer(&mut self, ui: &mut egui::Ui, index: usize, cell: egui::Vec2) {
            let Self { peers, names, .. } = self;
            let Some(peer) = peers.get_mut(index) else {
                return;
            };
            let label = names
                .get(&peer.remote)
                .cloned()
                .unwrap_or_else(|| short(peer.remote));
            let rect = peer.view.draw(ui, cell).rect;
            peer.view.draw_overlay(ui, rect);
            tile_label(ui, rect, &label);
        }

        /// Draws the chat scrollback and the line being typed.
        fn chat_ui(&mut self, ui: &mut egui::Ui) {
            let history = (ui.available_height() - CHAT_INPUT_HEIGHT - 6.0).max(0.0);
            egui::ScrollArea::vertical()
                .max_height(history)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.chat.lines {
                        match &line.sender {
                            None => {
                                ui.label(
                                    egui::RichText::new(&line.text)
                                        .italics()
                                        .color(egui::Color32::GRAY),
                                );
                            }
                            Some(sender) => {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("{sender}:")).strong());
                                    ui.label(&line.text);
                                });
                            }
                        }
                    }
                });

            let response = ui.add_sized(
                [ui.available_width(), CHAT_INPUT_HEIGHT],
                egui::TextEdit::singleline(&mut self.chat.input).hint_text("Message"),
            );
            if response.lost_focus() && ui.input(|state| state.key_pressed(egui::Key::Enter)) {
                self.send_chat();
                // Keep the focus so the next message can be typed straight
                // away, which is what pressing Enter in a chat box means.
                response.request_focus();
            }
        }

        /// Sends whatever is typed, and shows it locally.
        ///
        /// The room echoes nothing back to its own sender, so the local copy is
        /// the only one this window will ever see.
        fn send_chat(&mut self) {
            let text = self.chat.input.trim().to_string();
            if text.is_empty() {
                return;
            }
            self.chat.input.clear();
            self.chat.push(self.display_name.clone(), text.clone());
            let handle = self.handle.clone();
            self.sending.spawn(async move {
                if let Err(err) = handle.send_chat(text).await {
                    warn!(error = %err, "failed to send the chat message");
                }
            });
        }
    }

    /// Subscribes to a peer's broadcast and opens whichever tracks it carries.
    ///
    /// Returns `None` when the broadcast never produced a catalog, which is
    /// what a peer that announced a name it does not publish looks like.
    async fn open(
        remote: EndpointId,
        name: String,
        session: MoqSession,
        broadcast: moq_net::broadcast::Consumer,
    ) -> Option<Opened> {
        // A peer that announced a name it never published would otherwise
        // leave this task waiting forever.
        let opened = tokio::time::timeout(PEER_TIMEOUT, RemoteBroadcast::new(&name, broadcast));
        let broadcast = match opened.await {
            Ok(Ok(broadcast)) => broadcast,
            Ok(Err(err)) => {
                warn!(remote = %short(remote), %name, error = %err, "peer broadcast failed to open");
                return None;
            }
            Err(_) => {
                warn!(remote = %short(remote), %name, "peer broadcast produced no catalog");
                return None;
            }
        };
        // Room events hand back the session and the broadcast separately, so
        // the wiring `Live::subscribe` does for a subscription of its own is
        // done here instead.
        let sub = Subscription::new(session, broadcast);
        crate::ui::draw_without_downloading(sub.broadcast());
        let tracks = sub.broadcast().media().await;
        Some(Opened {
            remote,
            name,
            sub,
            tracks,
        })
    }

    /// Columns, rows, and cell size for `count` tiles in `available`.
    ///
    /// A square-ish grid wastes the least room, so the column count is the
    /// square root rounded up. Cells keep a 16:9 shape whatever the pictures in
    /// them are, which the views letterbox into.
    fn layout(count: usize, available: egui::Vec2) -> (usize, usize, egui::Vec2) {
        let cols = ((count as f32).sqrt().ceil() as usize).max(1);
        let rows = count.div_ceil(cols).max(1);
        let width = (available.x / cols as f32).min(available.y / rows as f32 * ASPECT);
        (
            cols,
            rows,
            egui::vec2(width.max(1.0), (width / ASPECT).max(1.0)),
        )
    }

    /// Draws a name in the corner of a tile.
    fn tile_label(ui: &egui::Ui, rect: egui::Rect, text: &str) {
        let painter = ui.painter_at(rect);
        let galley = painter.layout_no_wrap(
            text.to_string(),
            egui::FontId::monospace(11.0),
            egui::Color32::WHITE,
        );
        let at = rect.left_top() + egui::vec2(4.0, 4.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size() + egui::vec2(6.0, 2.0)),
            2.0,
            egui::Color32::from_black_alpha(160),
        );
        painter.galley(at + egui::vec2(3.0, 1.0), galley, egui::Color32::WHITE);
    }

    /// The short form of an endpoint id, which is what a peer without a
    /// display name is called.
    fn short(remote: EndpointId) -> String {
        remote.fmt_short().to_string()
    }

    /// The chat scrollback and the line being typed.
    #[derive(Debug, Default)]
    struct ChatState {
        lines: VecDeque<ChatLine>,
        input: String,
    }

    /// One line of the scrollback.
    #[derive(Debug)]
    struct ChatLine {
        /// Who said it, or `None` for a line the room itself produced.
        sender: Option<String>,
        text: String,
    }

    impl ChatState {
        /// Appends a message from `sender`.
        fn push(&mut self, sender: String, text: String) {
            self.append(ChatLine {
                sender: Some(sender),
                text,
            });
        }

        /// Appends a line the room produced, such as somebody joining.
        fn push_system(&mut self, text: String) {
            self.append(ChatLine { sender: None, text });
        }

        /// Appends `line`, dropping the oldest once the scrollback is full.
        fn append(&mut self, line: ChatLine) {
            self.lines.push_back(line);
            if self.lines.len() > MAX_CHAT_LINES {
                self.lines.pop_front();
            }
        }
    }
}
