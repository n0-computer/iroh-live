//! `irl call`: a 1:1 bidirectional video call.
//!
//! Both peers publish their own side at `calls/<their endpoint id>` and
//! subscribe to the other's, which is all [`Call`] is: [`Live::publish`] and a
//! subscription pointed at each other. Everything here is the window over that,
//! plus the small state machine that decides whether this node is dialing,
//! answering, or already talking.
//!
//! A call is symmetric and so is the way the two sides find each other. Every
//! window shows its own ticket as a QR code while nobody is on the line, and
//! every window has a scan screen that reads one off the camera, so it does not
//! matter which side holds its screen up: whoever scans the other places the
//! call. That is the whole exchange on a machine with no keyboard to paste a
//! ticket into. See [`crate::scan`] for the reader.

use iroh_live::{Call, Live, media::publish::LocalBroadcast};
use n0_error::Result;
use tracing::info;

use crate::{
    args::{CallArgs, CaptureArgs},
    source, source_spec, transport,
};

/// Runs the `call` command.
pub fn run(args: CallArgs, rt: &tokio::runtime::Runtime) -> Result {
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

/// Who holds the camera while nothing is being scanned.
///
/// The scan screen opens the default camera, and a capture device does not open
/// twice, so a window whose publisher already has one has to hand it over and
/// take it back. A window publishing anything else keeps publishing throughout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Camera {
    /// The publisher opened it, so the scan screen is handed the device.
    Publisher,
    /// Nothing here has it: the local video is a display, a test pattern, or
    /// nothing at all, and the scan screen opens the camera on its own.
    Free,
}

impl Camera {
    /// Works out who holds the camera from what `--video` asked for.
    ///
    /// A specifier that does not parse never opened anything, and whoever set
    /// the publish up has already reported it, so it counts as free.
    fn of(args: &CaptureArgs) -> Self {
        use source_spec::VideoSourceSpec as Spec;

        match args.video_source() {
            Ok(Spec::Camera(_)) => Self::Publisher,
            // `rpicam-vid` drives the Pi camera through libcamera rather than
            // V4L2, and the two still cannot hold one sensor at once.
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Ok(Spec::Rpicam(_)) => Self::Publisher,
            Ok(_) | Err(_) => Self::Free,
        }
    }
}

mod window {
    //! The call window: the peer's picture, this node's own in a corner, and
    //! the ticket exchange that gets the two connected.
    //!
    //! Four screens. The waiting screen shows this node's ticket as a QR code
    //! over the local picture, the scan screen reads the peer's off the camera,
    //! the calling screen is what a dial can be given up from, and the call
    //! itself draws the peer full width. The chrome is `irl watch`'s, down to
    //! the top bar, the control panel, and the way both fade out with the
    //! pointer, so the two commands do not feel like different programs.

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

    use super::Camera;
    use crate::{
        args::{CallArgs, CaptureArgs, PlaybackArgs},
        scan::ScanView,
        source,
        transport::PEER_TIMEOUT,
        ui::{CursorIdle, LocalPreview, RemoteView, TicketQr},
    };

    /// How many unanswered incoming sessions are held before the oldest one
    /// waits its turn. Callers are rare; this only has to cover a burst.
    const INCOMING_QUEUE: usize = 4;

    /// How often the window is woken while nothing is drawing it.
    ///
    /// Answering a call within this is imperceptible, and it costs a state
    /// machine pass ten times a second when the window is off screen.
    const HEARTBEAT: Duration = Duration::from_millis(100);

    /// How long the publisher waits before taking the camera back from the scan
    /// screen.
    ///
    /// Width of the local picture-in-picture, in points.
    const PIP_WIDTH: f32 = 240.0;

    /// Aspect ratio both the local preview and the picture-in-picture are drawn
    /// at, whatever the camera's own is.
    const ASPECT: f32 = 16.0 / 9.0;

    /// Size of the buttons on the screens with no call on them.
    ///
    /// Larger than an egui default because the machine this was built for is a
    /// small touchscreen with no pointer to aim precisely with, and those
    /// buttons are the only way off those screens.
    const BUTTON: egui::Vec2 = egui::vec2(160.0, 36.0);

    /// Width of the column a screen with no ticket QR code on it draws in,
    /// in points.
    const DIALOG_WIDTH: f32 = 280.0;

    /// Fraction of the window's shorter side the ticket QR code takes.
    const QR_FRACTION: f32 = 0.5;

    /// Smallest the ticket QR code is drawn, in points.
    ///
    /// A button's width, because the buttons under the code are what set the
    /// width of the column both are drawn in, and a code narrower than them
    /// would sit in a panel with space either side of it.
    const QR_MIN: f32 = BUTTON.x;

    /// Largest the ticket QR code is drawn, in points.
    ///
    /// A camera only has to resolve the modules, and past this the code is
    /// merely taking up window.
    const QR_MAX: f32 = 280.0;

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
                let ctx = &cc.egui_ctx;
                crate::ui::spawn_ctrl_c_handler(ctx);
                let (tx, incoming) = mpsc::channel(INCOMING_QUEUE);
                let forwarder = spawn(forward_incoming(live.clone(), tx));
                let mut app = CallApp {
                    qr: TicketQr::new(ctx, "call-ticket", &ticket),
                    ticket,
                    camera: Camera::of(&args.capture),
                    capture: args.capture,
                    playback: args.playback,
                    preview: LocalPreview::new(ctx, "call-preview", cc.wgpu_render_state.as_ref()),
                    render_state: cc.wgpu_render_state.clone(),
                    _heartbeat: crate::ui::spawn_heartbeat(ctx, HEARTBEAT),
                    _forwarder: AbortOnDropHandle::new(forwarder),
                    live,
                    broadcast,
                    incoming,
                    pending: None,
                    screen: Screen::waiting(),
                    restore: false,
                    cursor: CursorIdle::default(),
                };
                if let Some(ticket) = args.ticket {
                    app.dial(ctx, ticket);
                }
                Ok(Box::new(app))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    /// The call window.
    struct CallApp {
        live: Live,
        /// This node's ticket, shown in the top bar and as a QR code.
        ticket: String,
        /// The ticket as a code the peer's camera can read, or `None` on the
        /// machines where it would not render.
        qr: Option<TicketQr>,
        /// The local side, which every peer reads and no call owns.
        broadcast: LocalBroadcast,
        /// What the local side captures, kept so the video can be restarted
        /// after the scan screen has borrowed the camera.
        capture: CaptureArgs,
        camera: Camera,
        /// Whether the publisher owes itself a camera, because the scan screen
        /// borrowed it.
        restore: bool,
        incoming: mpsc::Receiver<MoqSession>,
        _forwarder: AbortOnDropHandle<()>,
        /// Keeps the state machine ticking while nothing draws the window.
        _heartbeat: AbortOnDropHandle<()>,
        /// The attempt in flight, of which there is at most one.
        pending: Option<Pending>,
        screen: Screen,
        preview: LocalPreview,
        render_state: Option<RenderState>,
        cursor: CursorIdle,
        /// The playback flags the peer's broadcast is opened under.
        playback: PlaybackArgs,
    }

    /// What the window is showing.
    enum Screen {
        /// Nobody on the line: this node's ticket as a QR code for the peer to
        /// read, a box to paste theirs into, and the local picture behind both.
        Waiting(Waiting),
        /// The camera, looking for the peer's ticket in a QR code.
        Scanning(Box<ScanView>),
        /// A dial this window started, with nothing to draw until it answers.
        Calling(Calling),
        /// A call in progress.
        InCall(Box<InCall>),
    }

    impl Screen {
        /// The waiting screen with nothing typed and nothing to report.
        fn waiting() -> Self {
            Self::Waiting(Waiting::default())
        }

        /// The waiting screen, reporting what became of the last attempt.
        fn reporting(message: String) -> Self {
            Self::Waiting(Waiting {
                input: String::new(),
                message: Some(message),
            })
        }
    }

    /// The waiting screen's state.
    #[derive(Debug, Default)]
    struct Waiting {
        /// The ticket the user is typing.
        input: String,
        /// What became of the last attempt, if there was one.
        message: Option<String>,
    }

    /// A dial in flight, as the screen waiting on it sees it.
    #[derive(Debug)]
    struct Calling {
        /// Who is being called, named on the screen and in the note a cancel
        /// leaves behind. The attempt itself is in [`CallApp::pending`].
        peer: String,
    }

    /// A connected call: the session and the peer's picture and sound.
    struct InCall {
        /// Owns the session and the stats and signal tasks behind the overlay.
        call: Call,
        remote: RemoteView,
    }

    impl InCall {
        /// Ends the call and the decoders drawing it.
        fn shutdown(&mut self) {
            self.remote.shutdown();
            self.call.close();
        }
    }

    /// A dial or an answer in flight.
    struct Pending {
        /// Which way it goes, which is what decides how the window waits on it.
        direction: Direction,
        rx: oneshot::Receiver<Answer>,
        /// Aborting this is what abandons the attempt.
        task: AbortOnDropHandle<()>,
    }

    impl Pending {
        /// Gives up on the attempt, closing a call that landed as it was
        /// abandoned.
        ///
        /// Closing the channel before draining it means an attempt that has not
        /// answered yet fails its send and closes what it built, and one that
        /// has answered is closed here. The abort then stops whatever is still
        /// dialing.
        fn discard(mut self) {
            self.rx.close();
            if let Ok(Answer::Connected(connected)) = self.rx.try_recv() {
                connected.discard();
            }
            self.task.abort();
        }
    }

    /// Which side started an attempt.
    #[derive(Debug, Clone)]
    enum Direction {
        /// This node dialed, so the calling screen waits on it and the user can
        /// give up.
        Outgoing { peer: String },
        /// A peer opened a session and this node is answering it.
        ///
        /// Speculative: everything that speaks MoQ to this node arrives the
        /// same way, and a plain subscriber never publishes the call path an
        /// answer waits for. So this one runs behind whatever is on screen, and
        /// its failure goes to the log rather than to the user.
        Incoming { peer: String },
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

    impl Connected {
        /// Ends a call that nothing is going to draw.
        ///
        /// An attempt that landed just as the user gave up on it owns a session
        /// and a set of decoders that no screen will ever hold, and dropping
        /// those leaves the peer to time the session out instead of being told.
        fn discard(self) {
            let Self { call, tracks } = self;
            drop(tracks);
            call.remote().shutdown();
            call.close();
        }
    }

    /// What the waiting screen was asked to do, applied once its panel has
    /// closed and given the borrow of the screen's own state back.
    enum Action {
        /// Open the camera and look for a ticket.
        Scan,
        /// Call whatever was typed or pasted.
        Dial(String),
    }

    impl eframe::App for CallApp {
        /// Drives the state machine.
        ///
        /// Here rather than in [`ui`](Self::ui) because eframe runs no egui
        /// pass while the window is minimized or occluded, and a window nobody
        /// is looking at still has to answer the phone.
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));
            self.poll_scan(ctx);
            self.poll_pending(ctx);
            self.poll_hangup();
            self.poll_capture();
            self.answer_next(ctx);
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            // Before the screen switch, so Escape leaves full screen from the
            // scan and calling screens too, not only during a call.
            crate::ui::escape_leaves_fullscreen(&ctx);
            self.preview.update(&ctx, &self.broadcast);
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

            match self.screen {
                Screen::Waiting(_) => self.waiting_ui(ui, &ctx),
                Screen::Scanning(_) => self.scan_ui(ui, &ctx),
                Screen::Calling(_) => self.calling_ui(ui, &ctx),
                Screen::InCall(_) => self.in_call_ui(ui, &ctx),
            }
        }

        fn on_exit(&mut self) {
            info!("exit");
            if let Screen::InCall(session) = &mut self.screen {
                session.shutdown();
            }
            if let Some(pending) = self.pending.take() {
                pending.discard();
            }
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }

    impl CallApp {
        /// Calls whichever ticket the camera has read, if it has read one.
        fn poll_scan(&mut self, ctx: &egui::Context) {
            let Screen::Scanning(view) = &self.screen else {
                return;
            };
            let Some(ticket) = view.ticket() else {
                return;
            };
            self.dial(ctx, ticket);
        }

        /// Takes the outcome of an attempt that finished since the last pass.
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
                    Answer::Failed("the call attempt stopped".to_string())
                }
            };
            let direction = self
                .pending
                .take()
                .expect("the attempt was borrowed a moment ago")
                .direction;

            let message = match answer {
                Answer::Connected(connected) => return self.enter_call(ctx, *connected),
                Answer::Failed(message) => message,
            };
            match direction {
                Direction::Outgoing { peer } => {
                    warn!(%peer, %message, "the call failed");
                    self.screen = Screen::reporting(message);
                }
                // Not news: the session was most likely a subscriber that never
                // meant to place a call at all.
                Direction::Incoming { peer } => {
                    debug!(%peer, %message, "the session turned out not to be a caller");
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
            self.screen = Screen::InCall(Box::new(InCall { call, remote }));
        }

        /// Returns to the waiting screen once the session closes, whichever
        /// side ended it.
        fn poll_hangup(&mut self) {
            let Screen::InCall(session) = &self.screen else {
                return;
            };
            if session.call.session().conn().close_reason().is_none() {
                return;
            }
            info!("call ended");
            let ended = std::mem::replace(
                &mut self.screen,
                Screen::reporting("the call ended".to_string()),
            );
            if let Screen::InCall(mut session) = ended {
                session.remote.shutdown();
            }
        }

        /// Takes the camera back after the scan screen has had it.
        ///
        /// No wait for the scan camera's thread to let go. The publish path
        /// retries a device that will not open yet, from 200ms up to a
        /// ceiling, so asking immediately costs at most one refused attempt
        /// and a line in the log. Waiting a fixed interval instead was a guess
        /// at how long a thread takes to stop, and it was wrong in both
        /// directions: too long for a device already free, too short for one
        /// that is not.
        fn poll_capture(&mut self) {
            if !self.restore {
                return;
            }
            self.restore = false;
            info!("taking the camera back from the scan screen");
            if let Err(err) = source::configure_video(&self.broadcast, &self.capture) {
                warn!(error = %err, "could not restart the local video after scanning");
            }
        }

        /// Answers the next caller waiting in the queue, if this node is idle.
        fn answer_next(&mut self, ctx: &egui::Context) {
            if self.pending.is_some() || matches!(self.screen, Screen::InCall(_)) {
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
            let peer = session.remote_id().fmt_short().to_string();
            let attempt = answer_call(session, self.playback);
            self.start(ctx, Direction::Incoming { peer }, attempt);
        }

        /// Calls `ticket`, replacing whatever the window was doing.
        ///
        /// Leaves the scan screen first, so a ticket read off the camera hands
        /// the device back to the publisher while the dial is in flight.
        fn dial(&mut self, ctx: &egui::Context, ticket: LiveTicket) {
            let peer = ticket.endpoint.id.fmt_short().to_string();
            self.leave_scan();
            if let Some(pending) = self.pending.take() {
                pending.discard();
            }
            self.screen = Screen::Calling(Calling { peer: peer.clone() });
            let attempt = dial_call(self.live.clone(), ticket, self.playback);
            self.start(ctx, Direction::Outgoing { peer }, attempt);
        }

        /// Runs `attempt` and remembers it as the pending one.
        ///
        /// The task holds the only handle to what it builds until it answers,
        /// so discarding the returned [`Pending`] both aborts the attempt and
        /// takes the call with it. An attempt that answers into a channel
        /// nobody is holding any more closes what it built rather than dropping
        /// it.
        fn start(
            &mut self,
            ctx: &egui::Context,
            direction: Direction,
            attempt: impl Future<Output = Answer> + Send + 'static,
        ) {
            let (tx, rx) = oneshot::channel();
            let ctx = ctx.clone();
            let task = spawn(async move {
                if let Err(Answer::Connected(connected)) = tx.send(attempt.await) {
                    info!("the call landed after it was given up on, closing it");
                    connected.discard();
                }
                ctx.request_repaint();
            });
            self.pending = Some(Pending {
                direction,
                rx,
                task: AbortOnDropHandle::new(task),
            });
        }

        /// Gives up on the dial in flight and returns to the waiting screen.
        ///
        /// What is abandoned is our half: the task is aborted, so nothing here
        /// is still waiting for a catalog, and a call it established in the
        /// meantime is closed rather than dropped. The session underneath is
        /// the transport's, which coalesces one per peer, so a dial the actor
        /// completed after we stopped listening stays cached there until the
        /// window closes, and calling the same peer again picks it up.
        fn cancel(&mut self, peer: &str) {
            info!(%peer, "the call was cancelled");
            if let Some(pending) = self.pending.take() {
                pending.discard();
            }
            self.screen = Screen::reporting(format!("stopped calling {peer}"));
        }

        /// Leaves the waiting screen and opens the camera.
        ///
        /// A publisher holding the camera gives it up here, because a capture
        /// device does not open twice. The scan screen is only reachable while
        /// no call is up, so what stops is a track no call is drawing; anything
        /// else subscribed to this node sees the video pause and come back.
        fn enter_scan(&mut self, ctx: &egui::Context) {
            info!("scanning for a ticket");
            if self.camera == Camera::Publisher {
                self.broadcast.video().clear();
            }
            self.restore = false;
            // No ticket is held off here: a call that fails to connect lands on
            // the waiting screen with a button rather than reopening the
            // camera, so there is no loop for a hold-off to break.
            self.screen = Screen::Scanning(Box::new(ScanView::new(
                ctx,
                self.render_state.as_ref(),
                None,
            )));
        }

        /// Closes the scan screen and asks for the camera back.
        ///
        /// Leaves the waiting screen behind; a caller that wants a different
        /// one sets it afterwards. Dropping the view is what asks the scan
        /// camera's thread to stop, and the publisher asks for the device
        /// straight away: the open retries until the thread has let go.
        fn leave_scan(&mut self) {
            if !matches!(self.screen, Screen::Scanning(_)) {
                return;
            }
            self.screen = Screen::waiting();
            if self.camera == Camera::Publisher {
                self.restore = true;
            }
        }

        /// Shows `message` on the waiting screen, if that is where the window
        /// is.
        fn report(&mut self, message: String) {
            if let Screen::Waiting(waiting) = &mut self.screen {
                waiting.message = Some(message);
            }
        }

        /// Draws the local picture filling the window, which is what sits
        /// behind every screen with no remote video on it.
        fn draw_backdrop(&self, ui: &mut egui::Ui) {
            let available = ui.available_size();
            let size = fit_to_aspect(available, ASPECT);
            let image = self.preview.image();
            ui.centered_and_justified(|ui| ui.add_sized(size, image));
        }

        /// Draws the waiting screen: this node's ticket as a QR code for the
        /// peer to read, the two ways of taking one the other way, and the
        /// local picture behind them.
        fn waiting_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            self.draw_backdrop(ui);
            crate::ui::top_bar(ui, ctx, &self.ticket);

            let answering = self.pending.is_some();
            let side = qr_side(ctx.content_rect().size());
            let Self {
                screen, qr, ticket, ..
            } = self;
            let Screen::Waiting(waiting) = screen else {
                return;
            };

            let mut action = None;
            crate::ui::dialog(ctx, "call-waiting", side, |ui| {
                ui.label("Have them scan this, or send them the ticket:");
                if let Some(qr) = qr {
                    ui.add_sized(egui::Vec2::splat(side), qr.image());
                }
                if ui
                    .add_sized(BUTTON, egui::Button::new("Scan theirs"))
                    .clicked()
                {
                    action = Some(Action::Scan);
                }
                if ui
                    .add_sized(BUTTON, egui::Button::new("Copy ticket"))
                    .clicked()
                {
                    ctx.copy_text(ticket.clone());
                }
                if let Some(text) = paste_row(ui, side, waiting) {
                    action = Some(Action::Dial(text));
                }
                // An incoming attempt keeps the ticket on screen rather than
                // taking it over, because it is not yet known to be a caller.
                if answering {
                    ui.spinner();
                }
                if let Some(message) = &waiting.message {
                    ui.colored_label(egui::Color32::LIGHT_YELLOW, message);
                }
            });

            match action {
                Some(Action::Scan) => self.enter_scan(ctx),
                Some(Action::Dial(text)) => match text.parse::<LiveTicket>() {
                    Ok(ticket) => self.dial(ctx, ticket),
                    Err(err) => self.report(format!("that is not a ticket: {err}")),
                },
                None => {}
            }
        }

        /// Draws the scan screen: the camera picture, and the way back to the
        /// ticket.
        fn scan_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            if let Screen::Scanning(view) = &mut self.screen {
                view.draw(ui);
            }
            crate::ui::top_bar(ui, ctx, &self.ticket);

            let mut cancel = false;
            crate::ui::control_panel(ctx, "call-scan-controls", |ui| {
                cancel = ui.add_sized(BUTTON, egui::Button::new("Cancel")).clicked();
            });
            if cancel {
                info!("the scan was cancelled");
                self.leave_scan();
            }
        }

        /// Draws the screen shown while a dial is in flight, with the button
        /// that gives up on it.
        fn calling_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            self.draw_backdrop(ui);
            crate::ui::top_bar(ui, ctx, &self.ticket);

            let Screen::Calling(calling) = &self.screen else {
                return;
            };
            let peer = calling.peer.clone();

            let mut cancel = false;
            crate::ui::dialog(ctx, "call-calling", DIALOG_WIDTH, |ui| {
                ui.heading(format!("calling {peer} ..."));
                ui.spinner();
                cancel = ui.add_sized(BUTTON, egui::Button::new("Cancel")).clicked();
            });
            if cancel {
                self.cancel(&peer);
            }
        }

        /// Draws the in-call screen: the peer full width, this node in the
        /// corner, and the overlay while the pointer is moving.
        fn in_call_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let expanded = match &self.screen {
                Screen::InCall(session) => session.remote.overlay_expanded(),
                Screen::Waiting(_) | Screen::Scanning(_) | Screen::Calling(_) => false,
            };
            let show_overlay = self.cursor.update(ctx, expanded);

            let Self {
                screen,
                preview,
                ticket,
                ..
            } = self;
            let Screen::InCall(session) = screen else {
                return;
            };

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

            let mut hang_up = false;
            crate::ui::control_panel(ctx, "call-controls", |ui| {
                session.remote.controls(ui, "call");
                hang_up = ui
                    .button("Hang up")
                    .on_hover_text("End the call and go back to the ticket")
                    .clicked();
            });
            if hang_up {
                info!("hanging up");
                if let Screen::InCall(session) = &mut self.screen {
                    session.shutdown();
                }
                self.screen = Screen::reporting("you hung up".to_string());
            }
        }
    }

    /// Draws the box a ticket is pasted into and the button that calls it, and
    /// returns whatever was entered.
    fn paste_row(ui: &mut egui::Ui, width: f32, waiting: &mut Waiting) -> Option<String> {
        let text = egui::TextEdit::singleline(&mut waiting.input).hint_text("Their ticket");
        let response = ui.add_sized(egui::vec2(width, BUTTON.y), text);
        let ready = !waiting.input.trim().is_empty();
        let clicked = ui
            .add_enabled(ready, egui::Button::new("Call").min_size(BUTTON))
            .clicked();
        let submitted =
            ready && response.lost_focus() && ui.input(|state| state.key_pressed(egui::Key::Enter));
        match clicked || submitted {
            true => Some(waiting.input.trim().to_string()),
            false => None,
        }
    }

    /// The side of the ticket QR code, in points, for a window of `content`
    /// size.
    ///
    /// A proportion of the shorter side rather than a fixed size: a code that
    /// took a fixed [`QR_MAX`] of a small touchscreen would leave no room for
    /// the buttons under it, and one that took half of a desktop window would
    /// be larger than any camera needs.
    fn qr_side(content: egui::Vec2) -> f32 {
        (content.x.min(content.y) * QR_FRACTION).clamp(QR_MIN, QR_MAX)
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

    #[cfg(test)]
    mod tests {
        use super::{QR_MAX, QR_MIN, egui, qr_side};

        /// The window this was built for is a small touchscreen, where a code
        /// at [`QR_MAX`] would cover the buttons under it.
        #[test]
        fn a_small_window_draws_the_code_smaller() {
            let side = qr_side(egui::vec2(480.0, 320.0));
            assert!(side < QR_MAX, "unexpected: {side}");
            assert!(side >= QR_MIN, "unexpected: {side}");
        }

        /// Past a point the code is only taking up window: a camera resolves
        /// the modules long before then.
        #[test]
        fn a_large_window_stops_growing_the_code() {
            assert_eq!(qr_side(egui::vec2(2560.0, 1440.0)), QR_MAX);
        }

        /// A window dragged down to nothing would otherwise render a code of no
        /// pixels at all.
        #[test]
        fn a_window_with_no_room_still_draws_a_whole_code() {
            assert_eq!(qr_side(egui::vec2(40.0, 20.0)), QR_MIN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Camera, CaptureArgs};

    /// A `--video` specifier, as `irl call` would have parsed one.
    fn capture(video: &str) -> CaptureArgs {
        CaptureArgs {
            video: video.to_string(),
            ..Default::default()
        }
    }

    /// The default, and the one case where the scan screen has to be handed the
    /// device.
    #[test]
    fn a_camera_publisher_hands_the_scan_screen_its_device() {
        assert_eq!(Camera::of(&capture("cam")), Camera::Publisher);
        assert_eq!(Camera::of(&capture("cam:0")), Camera::Publisher);
    }

    /// Neither of these is what the scan screen opens, so both keep publishing
    /// while a ticket is read.
    #[test]
    fn a_publisher_of_anything_else_keeps_its_source() {
        assert_eq!(Camera::of(&capture("screen")), Camera::Free);
        assert_eq!(Camera::of(&capture("test")), Camera::Free);
        assert_eq!(Camera::of(&capture("none")), Camera::Free);
    }

    /// The publish already failed and said so; the scan screen is not the place
    /// to report it a second time.
    #[test]
    fn a_specifier_that_never_opened_anything_holds_nothing() {
        assert_eq!(Camera::of(&capture("nonsense:")), Camera::Free);
    }
}
