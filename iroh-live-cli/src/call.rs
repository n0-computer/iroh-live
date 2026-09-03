//! `irl call`: a 1:1 bidirectional video call.
//!
//! Both peers publish their own side at `calls/<their endpoint id>` and
//! subscribe to the other's, which is all [`Call`] is: [`Live::publish`] and a
//! subscription pointed at each other. Everything here is the window over that,
//! plus the small state machine that decides whether this node is dialing,
//! answering, or already talking.

use iroh_live::{Call, Live, media::publish::LocalBroadcast};
use n0_error::Result;
use tracing::info;

use crate::{
    args::{CallArgs, CaptureArgs},
    source, transport,
};

/// Runs the `call` command.
pub fn run(args: CallArgs, rt: &tokio::runtime::Runtime) -> Result {
    if let Some(ticket) = &args.ticket {
        println!("calling {} ...", ticket.endpoint.id.fmt_short());
    }
    let (live, broadcast, ticket) = rt.block_on(setup(&args))?;

    // eframe takes the main thread from here on, so the runtime keeps its
    // workers only for as long as this guard lives.
    let _guard = rt.enter();
    window::run(live, broadcast, ticket, args)
}

/// Binds the endpoint, publishes this node's side, and prints the ticket the
/// peer needs.
async fn setup(args: &CallArgs) -> Result<(Live, LocalBroadcast, String)> {
    let live = transport::setup_live(true).await?;
    let (live, (broadcast, ticket)) = transport::with_live(live, async |live| {
        let broadcast = publish_local(live, &args.capture)?;
        let ticket = transport::ticket(live, &Call::path(live.endpoint().id()));
        println!("your call ticket: {ticket}");
        transport::print_qr(&ticket, args.no_qr);
        info!(ticket, "waiting for a call");
        Ok((broadcast, ticket))
    })
    .await?;
    Ok((live, broadcast, ticket))
}

/// Publishes this node's side of the call and opens the capture devices.
///
/// Published once and held for the process's lifetime. Publishing is node-wide,
/// so this broadcast is announced on every session, and a call neither creates
/// nor consumes it: peers that come and go all read the same one.
fn publish_local(live: &Live, args: &CaptureArgs) -> Result<LocalBroadcast> {
    let broadcast = live.publish(Call::path(live.endpoint().id()))?;
    source::configure(&broadcast, args)?;
    Ok(broadcast)
}

mod window {
    //! The call window: the peer's picture, this node's own in a corner, and
    //! the ticket exchange that gets the two connected.

    use std::time::Duration;

    use eframe::egui;
    use iroh_live::{
        Call, CallError, Live,
        media::{publish::LocalBroadcast, subscribe::MediaTracks},
        moq::MoqSession,
        ticket::LiveTicket,
    };
    use moq_media_egui::{egui_wgpu::RenderState, overlay::fit_to_aspect};
    use n0_error::{Result, anyerr};
    use n0_future::task::{AbortOnDropHandle, spawn};
    use tokio::sync::{mpsc, oneshot};
    use tracing::{debug, info, warn};

    use crate::{
        args::{CallArgs, PlaybackArgs},
        transport::PEER_TIMEOUT,
        ui::{CursorIdle, LocalPreview, RemoteView},
    };

    /// How many unanswered incoming sessions are held before the oldest one
    /// waits its turn. Callers are rare; this only has to cover a burst.
    const INCOMING_QUEUE: usize = 4;

    /// How often the window is woken while nothing is drawing it.
    ///
    /// Answering a call within this is imperceptible, and it costs a state
    /// machine pass ten times a second when the window is off screen.
    const HEARTBEAT: Duration = Duration::from_millis(100);

    /// Width of the local picture-in-picture, in points.
    const PIP_WIDTH: f32 = 240.0;

    /// Aspect ratio both the local preview and the picture-in-picture are drawn
    /// at, whatever the camera's own is.
    const ASPECT: f32 = 16.0 / 9.0;

    /// Opens the call window and runs it until it closes.
    pub(super) fn run(
        live: Live,
        broadcast: LocalBroadcast,
        ticket: String,
        args: CallArgs,
    ) -> Result {
        eframe::run_native(
            "irl call",
            crate::ui::native_options(args.fullscreen),
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);
                let (tx, incoming) = mpsc::channel(INCOMING_QUEUE);
                let heartbeat = crate::ui::spawn_heartbeat(&cc.egui_ctx, HEARTBEAT);
                let forwarder = spawn(forward_incoming(live.clone(), tx));
                let mut app = CallApp {
                    live,
                    ticket,
                    broadcast,
                    incoming,
                    _forwarder: AbortOnDropHandle::new(forwarder),
                    _heartbeat: heartbeat,
                    pending: None,
                    state: State::waiting(),
                    preview: LocalPreview::new(
                        &cc.egui_ctx,
                        "call-preview",
                        cc.wgpu_render_state.as_ref(),
                    ),
                    render_state: cc.wgpu_render_state.clone(),
                    cursor: CursorIdle::default(),
                    playback: args.playback,
                };
                if let Some(ticket) = args.ticket {
                    app.dial(ticket);
                }
                Ok(Box::new(app))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    /// The call window.
    struct CallApp {
        live: Live,
        /// This node's ticket, shown in the top bar and on the waiting screen.
        ticket: String,
        /// The local side, which every peer reads and no call owns.
        broadcast: LocalBroadcast,
        incoming: mpsc::Receiver<MoqSession>,
        _forwarder: AbortOnDropHandle<()>,
        /// Keeps the state machine ticking while nothing draws the window.
        _heartbeat: AbortOnDropHandle<()>,
        pending: Option<Pending>,
        state: State,
        preview: LocalPreview,
        render_state: Option<RenderState>,
        cursor: CursorIdle,
        /// The playback flags the peer's broadcast is opened under.
        playback: PlaybackArgs,
    }

    /// What the window is doing.
    enum State {
        /// Nobody on the line. The window shows this node's ticket and a box to
        /// paste the peer's into.
        Waiting {
            /// The ticket the user is typing.
            input: String,
            /// What became of the last attempt, if there was one.
            message: Option<String>,
        },
        /// A call in progress.
        InCall(Box<InCall>),
    }

    impl State {
        /// The waiting state with nothing typed and nothing to report.
        fn waiting() -> Self {
            Self::Waiting {
                input: String::new(),
                message: None,
            }
        }
    }

    /// A connected call: the session and the peer's picture and sound.
    struct InCall {
        /// Owns the session and the stats and signal tasks behind the overlay.
        call: Call,
        remote: RemoteView,
    }

    /// A dial or an answer in flight.
    struct Pending {
        rx: oneshot::Receiver<Answer>,
        _task: AbortOnDropHandle<()>,
    }

    /// What a dial or an answer came back with.
    enum Answer {
        /// The peer is on the line and its tracks are open.
        Connected(Box<Connected>),
        /// The attempt failed, with something to show the user.
        Failed(String),
    }

    /// A call that established, with the peer's tracks already open.
    struct Connected {
        call: Call,
        tracks: MediaTracks,
    }

    impl eframe::App for CallApp {
        /// Drives the state machine.
        ///
        /// Here rather than in [`ui`](Self::ui) because eframe runs no egui
        /// pass while the window is minimized or occluded, and a window nobody
        /// is looking at still has to answer the phone.
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));
            self.poll_pending(ctx);
            self.poll_hangup();
            self.answer_next();
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            crate::ui::escape_leaves_fullscreen(&ctx);
            self.update_preview(&ctx);

            match self.state {
                State::Waiting { .. } => self.waiting_ui(ui, &ctx),
                State::InCall(_) => self.in_call_ui(ui, &ctx),
            }
        }

        fn on_exit(&mut self) {
            info!("exit");
            if let State::InCall(session) = &mut self.state {
                session.remote.shutdown();
                session.call.close();
            }
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }

    impl CallApp {
        /// Takes the outcome of an attempt that finished since the last frame.
        fn poll_pending(&mut self, ctx: &egui::Context) {
            let Some(pending) = self.pending.as_mut() else {
                return;
            };
            let answer = match pending.rx.try_recv() {
                Ok(answer) => answer,
                Err(oneshot::error::TryRecvError::Empty) => return,
                // The task went away without answering, which happens only as
                // the runtime shuts down.
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.pending = None;
                    return;
                }
            };
            self.pending = None;
            match answer {
                Answer::Connected(connected) => self.enter_call(ctx, *connected),
                Answer::Failed(message) => {
                    warn!(%message, "call attempt failed");
                    self.report(message);
                }
            }
        }

        /// Moves to the in-call screen and opens the peer's video for drawing.
        fn enter_call(&mut self, ctx: &egui::Context, connected: Connected) {
            let Connected { call, tracks } = connected;
            info!(remote = %call.remote_id().fmt_short(), "call connected");
            let remote = RemoteView::new(
                ctx,
                "call-remote",
                call.remote().clone(),
                tracks,
                call.signals().clone(),
                self.render_state.as_ref(),
            );
            self.state = State::InCall(Box::new(InCall { call, remote }));
        }

        /// Returns to the waiting screen once the session closes, whichever
        /// side ended it.
        fn poll_hangup(&mut self) {
            let State::InCall(session) = &self.state else {
                return;
            };
            if session.call.session().conn().close_reason().is_none() {
                return;
            }
            info!("call ended");
            if let State::InCall(mut session) = std::mem::replace(&mut self.state, State::waiting())
            {
                session.remote.shutdown();
            }
            self.report("the call ended".to_string());
        }

        /// Answers the next caller waiting in the queue, if this node is idle.
        fn answer_next(&mut self) {
            if self.pending.is_some() || matches!(self.state, State::InCall(_)) {
                return;
            }
            // Skip sessions that closed while they waited: something that came
            // and went is not a caller holding the line.
            let session = loop {
                match self.incoming.try_recv() {
                    Ok(session) if session.conn().close_reason().is_none() => break session,
                    Ok(_) => continue,
                    Err(_) => return,
                }
            };
            self.start(answer_call(session, self.playback));
        }

        /// Dials `ticket`.
        fn dial(&mut self, ticket: LiveTicket) {
            self.report(format!("calling {} ...", ticket.endpoint.id.fmt_short()));
            self.start(dial_call(self.live.clone(), ticket, self.playback));
        }

        /// Runs `attempt` and remembers it as the pending one.
        fn start(&mut self, attempt: impl Future<Output = Answer> + Send + 'static) {
            let (tx, rx) = oneshot::channel();
            let task = spawn(async move {
                let _ = tx.send(attempt.await);
            });
            self.pending = Some(Pending {
                rx,
                _task: AbortOnDropHandle::new(task),
            });
        }

        /// Shows `message` on the waiting screen, if that is where the window
        /// is.
        fn report(&mut self, message: String) {
            if let State::Waiting { message: slot, .. } = &mut self.state {
                *slot = Some(message);
            }
        }

        /// Draws the newest captured frame.
        fn update_preview(&mut self, ctx: &egui::Context) {
            self.preview.update(ctx, &self.broadcast);
        }

        /// Draws the waiting screen: the ticket to share, the box to paste one
        /// into, and this node's own picture underneath.
        fn waiting_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let connecting = self.pending.is_some();
            let mut dial = None;
            let Self {
                state,
                preview,
                ticket,
                ..
            } = self;
            let State::Waiting { input, message } = state else {
                return;
            };

            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(20.0);
                    ui.heading("irl call");
                    ui.add_space(10.0);

                    ui.label("Your ticket, for the person you want to talk to:");
                    ui.horizontal(|ui| {
                        ui.monospace(shorten(ticket));
                        if ui.button("Copy").clicked() {
                            ctx.copy_text(ticket.clone());
                        }
                    });

                    ui.add_space(10.0);
                    ui.label("Or paste theirs and call:");
                    ui.horizontal(|ui| {
                        let response = ui.text_edit_singleline(input);
                        let ready = !input.trim().is_empty() && !connecting;
                        let clicked = ui.add_enabled(ready, egui::Button::new("Call")).clicked();
                        let entered = ready
                            && response.lost_focus()
                            && ui.input(|state| state.key_pressed(egui::Key::Enter));
                        if clicked || entered {
                            dial = Some(input.trim().to_string());
                        }
                    });

                    if connecting {
                        ui.spinner();
                    }
                    if let Some(message) = message {
                        ui.label(message.as_str());
                    }
                    ui.add_space(10.0);
                });

                let available = ui.available_size();
                let size = fit_to_aspect(available, ASPECT);
                ui.centered_and_justified(|ui| ui.add_sized(size, preview.image()));
            });

            let Some(input) = dial else {
                return;
            };
            match input.parse::<LiveTicket>() {
                Ok(ticket) => self.dial(ticket),
                Err(err) => self.report(format!("that is not a ticket: {err}")),
            }
        }

        /// Draws the in-call screen: the peer full width, this node in the
        /// corner, and the overlay while the pointer is moving.
        fn in_call_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let expanded = match &self.state {
                State::InCall(session) => session.remote.overlay_expanded(),
                State::Waiting { .. } => false,
            };
            let show_overlay = self.cursor.update(ctx, expanded);

            let Self {
                state,
                preview,
                ticket,
                ..
            } = self;
            let State::InCall(session) = state else {
                return;
            };

            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let available = ui.available_size();
            let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            session.remote.draw(ui, available);

            let pip = egui::vec2(PIP_WIDTH, PIP_WIDTH / ASPECT);
            egui::Area::new(egui::Id::new("call-pip"))
                .anchor(egui::Align2::RIGHT_BOTTOM, [-10.0, -10.0])
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::BLACK)
                        .corner_radius(4.0)
                        .inner_margin(2.0)
                        .show(ui, |ui| ui.add_sized(pip, preview.image()));
                });

            if !show_overlay {
                return;
            }
            crate::ui::top_bar(ui, ctx, ticket);
            session.remote.draw_overlay(ui, video_rect);
            crate::ui::control_panel(ctx, "call-controls", |ui| {
                session.remote.controls(ui, "call");
            });
        }
    }

    /// Forwards the sessions peers open to this node.
    ///
    /// Runs for the window's whole life rather than for one attempt: a stream
    /// read only between attempts would miss a caller that dialed during one.
    /// Sessions this node dialed arrive here too and are skipped, since they
    /// are the outgoing half of a call already under way.
    async fn forward_incoming(live: Live, tx: mpsc::Sender<MoqSession>) {
        let mut incoming = live.transport().incoming_sessions();
        while let Some(session) = incoming.next().await {
            if session.dialed() {
                continue;
            }
            debug!(remote = %session.remote_id().fmt_short(), "incoming session");
            if tx.send(session).await.is_err() {
                break;
            }
        }
    }

    /// Dials the peer named by `ticket` and opens its tracks.
    async fn dial_call(live: Live, ticket: LiveTicket, playback: PlaybackArgs) -> Answer {
        info!(remote = %ticket.endpoint.id.fmt_short(), "dialing");
        settle(Call::dial(&live, ticket.endpoint), playback).await
    }

    /// Answers `session` and opens the caller's tracks.
    async fn answer_call(session: MoqSession, playback: PlaybackArgs) -> Answer {
        info!(remote = %session.remote_id().fmt_short(), "answering");
        settle(Call::accept(session), playback).await
    }

    /// Waits for a call to establish, then opens whichever tracks the peer
    /// carries.
    ///
    /// Both directions are given [`PEER_TIMEOUT`], answering included: an
    /// incoming session is not necessarily a caller, since everything that
    /// speaks MoQ to this node arrives the same way and a plain subscriber
    /// never publishes the call path an answer waits for.
    async fn settle(
        setup: impl Future<Output = Result<Call, CallError>>,
        playback: PlaybackArgs,
    ) -> Answer {
        let call = match tokio::time::timeout(PEER_TIMEOUT, setup).await {
            Ok(Ok(call)) => call,
            Ok(Err(err)) => return Answer::Failed(format!("{err:#}")),
            Err(_) => {
                return Answer::Failed(format!(
                    "gave up after {}s: the peer never published its side",
                    PEER_TIMEOUT.as_secs()
                ));
            }
        };
        crate::ui::prepare_playback(call.remote(), &playback);
        let tracks = call.remote().media().await;
        Answer::Connected(Box::new(Connected { call, tracks }))
    }

    /// Trims a ticket to something that fits on one line.
    fn shorten(ticket: &str) -> String {
        /// How many characters of a ticket are shown before it is elided.
        const KEEP: usize = 60;

        match ticket.char_indices().nth(KEEP) {
            Some((end, _)) => format!("{}...", &ticket[..end]),
            None => ticket.to_string(),
        }
    }
}
