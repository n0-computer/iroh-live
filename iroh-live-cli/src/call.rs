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

use iroh_live::{
    Call, Live,
    media::{AudioOutput, LocalBroadcast},
};
use n0_error::Result;
use tracing::info;

use crate::{
    args::{CallArgs, CaptureArgs},
    source,
    source_spec::VideoSourceSpec as Spec,
    transport,
};

/// Runs the `call` command.
pub fn run(args: CallArgs, rt: &tokio::runtime::Runtime) -> Result {
    let local = rt.block_on(setup(&args))?;

    // eframe takes the main thread from here on, so the runtime keeps its
    // workers only for as long as this guard lives.
    let _guard = rt.enter();
    window::run(local, args)
}

/// This node's side of the call, and the speaker the peer's side plays on.
struct Local {
    live: Live,
    broadcast: LocalBroadcast,
    sources: source::Opened,
    output: AudioOutput,
    ticket: String,
}

/// Binds the endpoint, publishes this node's side, and prints the ticket the
/// peer needs.
async fn setup(args: &CallArgs) -> Result<Local> {
    // Opened first, so the microphone can cancel what it plays.
    let output = crate::playback::output(None).await?;
    let live = transport::setup_live(true).await?;
    let (live, (broadcast, sources, ticket)) = transport::with_live(live, async |live| {
        let (broadcast, sources) = publish_local(live, &args.capture, &output).await?;
        let ticket = transport::ticket(live, &Call::path(live.endpoint().id()));
        println!("your call ticket: {ticket}");
        transport::print_qr(&ticket, args.no_qr);
        info!(ticket, "waiting for a call");
        Ok((broadcast, sources, ticket))
    })
    .await?;
    Ok(Local {
        live,
        broadcast,
        sources,
        output,
        ticket,
    })
}

/// Publishes this node's side of the call and opens the capture devices.
///
/// Published once and held for the process's lifetime. Publishing is node-wide,
/// so this broadcast is announced on every session, and a call neither creates
/// nor consumes it: peers that come and go all read the same one. The
/// microphone cancels `output`, which is where the peer's voice plays.
async fn publish_local(
    live: &Live,
    args: &CaptureArgs,
    output: &AudioOutput,
) -> Result<(LocalBroadcast, source::Opened)> {
    let broadcast = live.publish(Call::path(live.endpoint().id()))?;
    let sources = source::configure(&broadcast, args, Some(output)).await?;
    Ok((broadcast, sources))
}

/// Who holds the camera while nothing is being scanned.
///
/// The scan screen reads the default camera, and a capture device does not open
/// twice, so a window whose publisher already has one lends the scan screen its
/// pictures. A window publishing anything else lets the scan screen open the
/// camera itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Camera {
    /// The publisher opened it, so the scan screen reads its pictures, or
    /// borrows the device when the publisher has none to lend.
    Publisher,
    /// Nothing here has it: the local video is a display, a test pattern, or
    /// nothing at all, and the scan screen opens the camera on its own.
    Free,
}

impl Camera {
    /// Works out who holds the camera the scan screen reads, from what
    /// `--video` asked for and the `--scan-camera` the screen opens, if one
    /// was named.
    ///
    /// A specifier that does not parse never opened anything, and whoever set
    /// the publish up has already reported it, so it counts as free. A scan
    /// camera that is another device than the publisher's leaves the publisher
    /// alone: a USB webcam for the scan does not stop `rpicam`.
    fn of(args: &CaptureArgs, scan: Option<&Spec>) -> Self {
        let Ok(publishing) = args.video_source() else {
            return Self::Free;
        };
        let holds_a_camera = match &publishing {
            Spec::Camera(_) => true,
            // `rpicam-vid` drives the Pi camera through libcamera rather than
            // V4L2, and the two still cannot hold one sensor at once.
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Spec::Rpicam(_) => true,
            _ => false,
        };
        let shared = match scan {
            None => true,
            Some(scan) => same_device(&publishing, scan),
        };
        match holds_a_camera && shared {
            true => Self::Publisher,
            false => Self::Free,
        }
    }
}

/// Whether two camera specifiers may name the same device.
///
/// A default camera may be any of them, so it counts as the same as a named
/// one: guessing apart would open one device twice, which fails.
fn same_device(left: &Spec, right: &Spec) -> bool {
    match (left, right) {
        (Spec::Camera(left), Spec::Camera(right)) => {
            left.is_none() || right.is_none() || left == right
        }
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        (Spec::Rpicam(_), Spec::Rpicam(_)) => true,
        _ => false,
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

    use std::{sync::Arc, time::Duration};

    use eframe::egui;
    use iroh_live::{
        Call, CallError, Live,
        media::{AudioOutput, LocalBroadcast, Player, SlotState, VideoSource},
        moq::MoqSession,
        ticket::LiveTicket,
    };
    use iroh_live_egui::{egui_wgpu::RenderState, overlay::fit_to_aspect};
    use n0_error::{Result, anyerr};
    use n0_future::task::{AbortOnDropHandle, spawn};
    use n0_watcher::Watcher as _;
    use tokio::sync::{mpsc, oneshot};
    use tracing::{debug, info, warn};

    use super::{Camera, Local};
    use crate::{
        args::{CallArgs, PlaybackArgs},
        scan::ScanView,
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
    pub(super) fn run(local: Local, args: CallArgs) -> Result {
        let Local {
            live,
            broadcast,
            sources,
            output,
            ticket,
        } = local;
        // Parsed before the window opens, so a bad specifier is a line in the
        // terminal rather than a message on a screen nobody asked for yet.
        let scan_camera = crate::scan::camera_spec(args.scan_camera.as_deref())
            .map_err(|err| anyerr!("{err}"))?;
        eframe::run_native(
            "irl call",
            crate::ui::native_options(args.fullscreen),
            Box::new(move |cc| {
                let ctx = &cc.egui_ctx;
                crate::ui::spawn_ctrl_c_handler(ctx);
                let (tx, incoming) = mpsc::channel(INCOMING_QUEUE);
                let forwarder = spawn(forward_incoming(live.clone(), tx));
                let preview = LocalPreview::new(
                    ctx,
                    "call-preview",
                    sources.video.as_ref().map(VideoSource::frames),
                    cc.wgpu_render_state.as_ref(),
                );
                let mut app = CallApp {
                    qr: TicketQr::new(ctx, "call-ticket", &ticket),
                    ticket,
                    camera: Camera::of(&args.capture, scan_camera.as_ref()),
                    capture: args.capture.clone(),
                    restoring: None,
                    camera_owed: false,
                    camera_epoch: Arc::default(),
                    notice: None,
                    local_video: sources.video,
                    _local_audio: sources.audio,
                    output,
                    scan_camera,
                    playback: args.playback,
                    preview,
                    render_state: cc.wgpu_render_state.clone(),
                    _heartbeat: crate::ui::spawn_heartbeat(ctx, HEARTBEAT),
                    _forwarder: AbortOnDropHandle::new(forwarder),
                    live,
                    broadcast,
                    incoming,
                    pending: None,
                    screen: Screen::waiting(),
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
        /// The local video, which the preview draws and the scan screen reads
        /// when it is the camera.
        local_video: Option<VideoSource>,
        /// The local audio, held for the call's life.
        _local_audio: Option<iroh_live::media::AudioSource>,
        /// Where the peer's voice plays, and what the microphone cancels.
        output: AudioOutput,
        camera: Camera,
        /// What the local side captures, kept so a camera handed to the scan
        /// screen can be opened again afterwards.
        capture: crate::args::CaptureArgs,
        /// The publisher's camera opening again after a scan, and where the
        /// result arrives.
        restoring: Option<Restoring>,
        /// Whether the scan screen took the publisher's camera and it has not
        /// been given back yet. Settled whenever the scan screen is not up,
        /// whichever way the window left it: a call answered mid-scan replaces
        /// the screen without passing through the scan screen's own exit.
        camera_owed: bool,
        /// Bumped, under its lock, whenever the scan screen takes the camera,
        /// so a restore still opening the device cannot set it on the
        /// broadcast after the scan screen has cleared it.
        camera_epoch: Arc<std::sync::Mutex<u64>>,
        /// A message for the waiting screen that arrived while another screen
        /// was up.
        notice: Option<String>,
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
        /// Which camera the scan screen opens.
        scan_camera: Option<crate::source_spec::VideoSourceSpec>,
    }

    /// The publisher's camera opening again, after the scan screen had it.
    struct Restoring {
        done: oneshot::Receiver<n0_error::Result<Option<VideoSource>>>,
        _task: AbortOnDropHandle<()>,
    }

    /// How often the camera is tried again while the scan screen's capture of
    /// it lets go.
    const RESTORE_ATTEMPTS: u32 = 5;

    /// How long one attempt has to reach a running video before it counts as
    /// failed. Setting a source only spawns it: `rpicam-vid` finds out whether
    /// the sensor is free only once it runs.
    const RESTORE_PATIENCE: Duration = Duration::from_secs(10);

    /// The pause between two of those tries.
    const RESTORE_DELAY: Duration = Duration::from_secs(1);

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
        /// Owns the session and the signal task the player adapts on.
        call: Call,
        remote: RemoteView,
    }

    impl InCall {
        /// Ends the call; the player goes with the screen.
        fn shutdown(&mut self) {
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

    /// What one attempt to give the publisher its camera back came to.
    enum Restored {
        /// The video runs; the raw source, if it is one, for the preview.
        Running(Option<VideoSource>),
        /// The scan screen took the camera again before the source was set.
        Superseded,
    }

    /// Opens the publisher's camera once and waits for its video to run.
    ///
    /// The source is set on the broadcast under the camera epoch's lock, and
    /// only if the epoch is still `mine`, so a scan screen that took the camera
    /// meanwhile, and cleared the video under the same lock, is never undone.
    ///
    /// # Errors
    ///
    /// Fails if the camera does not open, or its video fails or does not run
    /// within [`RESTORE_PATIENCE`].
    async fn restore_once(
        broadcast: &LocalBroadcast,
        capture: &crate::args::CaptureArgs,
        epoch: &std::sync::Mutex<u64>,
        mine: u64,
    ) -> Result<Restored> {
        let opened = crate::source::open_video(capture).await?;
        let video = {
            let current = epoch.lock().expect("poisoned");
            if *current != mine {
                return Ok(Restored::Superseded);
            }
            match opened {
                Some(opened) => opened.apply(broadcast)?,
                None => return Ok(Restored::Running(None)),
            }
        };
        let mut status = broadcast.status();
        let running = tokio::time::timeout(RESTORE_PATIENCE, async {
            loop {
                match status.get().video {
                    SlotState::Running => return Ok(()),
                    SlotState::Failed(err) => return Err(anyerr!("the camera failed: {err}")),
                    _ => {}
                }
                if status.updated().await.is_err() {
                    return Err(anyerr!("the broadcast closed"));
                }
            }
        })
        .await;
        match running {
            Ok(Ok(())) => Ok(Restored::Running(video)),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(anyerr!(
                "the camera did not start within {}s",
                RESTORE_PATIENCE.as_secs()
            )),
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

    /// A call that established, with the peer's side already playing.
    struct Connected {
        call: Call,
        player: Player,
    }

    impl Connected {
        /// Ends a call that nothing is going to draw.
        ///
        /// An attempt that landed just as the user gave up on it owns a session
        /// and a set of decoders that no screen will ever hold, and dropping
        /// those leaves the peer to time the session out instead of being told.
        fn discard(self) {
            let Self { call, player } = self;
            drop(player);
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
        /// Here rather than in `ui` because eframe runs no egui
        /// pass while the window is minimized or occluded, and a window nobody
        /// is looking at still has to answer the phone.
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));
            self.poll_scan(ctx);
            self.poll_restore();
            self.poll_pending(ctx);
            self.poll_hangup();
            self.answer_next(ctx);
            self.settle_camera();
            self.show_notice();
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            // Before the screen switch, so Escape leaves full screen from the
            // scan and calling screens too, not only during a call.
            crate::ui::escape_leaves_fullscreen(&ctx);
            self.preview.update(&ctx);
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
            crate::ui::shutdown_publish_blocking(&self.live, &self.broadcast);
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
            let Connected { call, player } = connected;
            info!(remote = %call.remote_id().fmt_short(), "call connected");
            let remote = RemoteView::new(
                ctx,
                "call-remote",
                player,
                self.playback.decoder,
                self.render_state.as_ref(),
            )
            .with_link(call.session().clone(), call.signals().clone());
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
            drop(ended);
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
            let attempt = answer_call(session, self.playback, self.output.clone());
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
            let attempt = dial_call(
                self.live.clone(),
                ticket,
                self.playback,
                self.output.clone(),
            );
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

        /// Leaves the waiting screen and starts reading tickets off the camera.
        ///
        /// A publisher holding the camera lends the scan screen its pictures,
        /// because a capture device does not open twice; the publish carries on
        /// underneath. Otherwise the scan screen opens the camera itself.
        fn enter_scan(&mut self, ctx: &egui::Context) {
            info!("scanning for a ticket");
            // No ticket is held off here: a call that fails to connect lands on
            // the waiting screen with a button rather than reopening the
            // camera, so there is no loop for a hold-off to break.
            let view = match (&self.local_video, self.camera) {
                (Some(camera), Camera::Publisher) => {
                    ScanView::from_frames(ctx, self.render_state.as_ref(), None, camera.frames())
                }
                (None, Camera::Publisher) => {
                    // The publisher holds the camera but has no pictures to
                    // lend: `rpicam` hands over H.264 it encoded itself. It
                    // lets go of the sensor for the scan and takes it back
                    // afterwards; anything subscribed sees the video pause.
                    {
                        let mut epoch = self.camera_epoch.lock().expect("poisoned");
                        *epoch += 1;
                        self.broadcast.clear_video();
                    }
                    self.restoring = None;
                    self.camera_owed = true;
                    ScanView::new(
                        ctx,
                        self.render_state.as_ref(),
                        None,
                        self.scan_camera.clone(),
                    )
                }
                _ => ScanView::new(
                    ctx,
                    self.render_state.as_ref(),
                    None,
                    self.scan_camera.clone(),
                ),
            };
            self.screen = Screen::Scanning(Box::new(view));
        }

        /// Closes the scan screen.
        ///
        /// Leaves the waiting screen behind; a caller that wants a different
        /// one sets it afterwards. Dropping the view releases a camera it
        /// opened itself.
        fn leave_scan(&mut self) {
            if !matches!(self.screen, Screen::Scanning(_)) {
                return;
            }
            // Dropping the view is what releases a camera it opened, and
            // `settle_camera` gives the publisher its own back.
            self.screen = Screen::waiting();
        }

        /// Gives the publisher its camera back once the scan screen is gone,
        /// however it went.
        fn settle_camera(&mut self) {
            if self.camera_owed && !matches!(self.screen, Screen::Scanning(_)) {
                self.camera_owed = false;
                self.restore_camera();
            }
        }

        /// Opens the publisher's camera again, retrying while the scan
        /// screen's capture of it winds down.
        ///
        /// An attempt counts only once the video runs: the scan screen's own
        /// `rpicam-vid` may still hold the sensor, and a new one that finds it
        /// busy exits, which shows as a failed video rather than as an error
        /// from setting it.
        fn restore_camera(&mut self) {
            let broadcast = self.broadcast.clone();
            let capture = self.capture.clone();
            let epoch = self.camera_epoch.clone();
            let mine = *epoch.lock().expect("poisoned");
            let (done, report) = oneshot::channel();
            let task = tokio::spawn(async move {
                let mut attempt = 0;
                let result = loop {
                    attempt += 1;
                    let outcome = restore_once(&broadcast, &capture, &epoch, mine).await;
                    match outcome {
                        Ok(Restored::Running(video)) => break Ok(video),
                        Ok(Restored::Superseded) => return,
                        Err(err) if attempt >= RESTORE_ATTEMPTS => break Err(err),
                        Err(err) => {
                            debug!(error = %format!("{err:#}"), attempt, "the camera is not free yet");
                            tokio::time::sleep(RESTORE_DELAY).await;
                        }
                    }
                };
                let _ = done.send(result);
            });
            self.restoring = Some(Restoring {
                done: report,
                _task: AbortOnDropHandle::new(task),
            });
        }

        /// Collects the camera the publisher opened again after a scan.
        fn poll_restore(&mut self) {
            let Some(restoring) = self.restoring.as_mut() else {
                return;
            };
            let result = match restoring.done.try_recv() {
                Ok(result) => result,
                Err(oneshot::error::TryRecvError::Empty) => return,
                Err(oneshot::error::TryRecvError::Closed) => {
                    Err(n0_error::anyerr!("the camera restore was abandoned"))
                }
            };
            self.restoring = None;
            match result {
                Ok(video) => {
                    info!("the publisher has its camera back");
                    self.preview
                        .set_frames(video.as_ref().map(VideoSource::frames));
                    self.local_video = video;
                }
                Err(err) => {
                    let message = format!("the camera did not come back after the scan: {err:#}");
                    warn!(%message);
                    self.notice = Some(message);
                }
            }
        }

        /// Shows `message` on the waiting screen, if that is where the window
        /// is.
        fn report(&mut self, message: String) {
            if let Screen::Waiting(waiting) = &mut self.screen {
                waiting.message = Some(message);
            }
        }

        /// Shows a message kept for the waiting screen, once that is where the
        /// window is and nothing else is being reported there.
        fn show_notice(&mut self) {
            if let Screen::Waiting(waiting) = &mut self.screen
                && waiting.message.is_none()
                && let Some(message) = self.notice.take()
            {
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
    async fn dial_call(
        live: Live,
        ticket: LiveTicket,
        playback: PlaybackArgs,
        output: AudioOutput,
    ) -> Answer {
        info!(remote = %ticket.endpoint.id.fmt_short(), "dialing");
        settle(Call::dial(&live, ticket.endpoint), playback, output).await
    }

    /// Answers `session` and plays the caller's side.
    async fn answer_call(
        session: MoqSession,
        playback: PlaybackArgs,
        output: AudioOutput,
    ) -> Answer {
        info!(remote = %session.remote_id().fmt_short(), "answering");
        settle(Call::accept(session), playback, output).await
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
        output: AudioOutput,
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
        let config = crate::ui::player_config(&playback, Some(&output));
        match call.remote().play(config) {
            Ok(player) => Answer::Connected(Box::new(Connected { call, player })),
            Err(err) => Answer::Failed(format!("{err:#}")),
        }
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
        assert_eq!(Camera::of(&capture("cam"), None), Camera::Publisher);
        assert_eq!(Camera::of(&capture("cam:0"), None), Camera::Publisher);
    }

    /// Neither of these is what the scan screen opens, so both keep publishing
    /// while a ticket is read.
    #[test]
    fn a_publisher_of_anything_else_keeps_its_source() {
        assert_eq!(Camera::of(&capture("screen"), None), Camera::Free);
        assert_eq!(Camera::of(&capture("test"), None), Camera::Free);
        assert_eq!(Camera::of(&capture("none"), None), Camera::Free);
    }

    /// A scan camera that is another device leaves the publisher's alone;
    /// one that may be the same device is lent or borrowed.
    #[test]
    fn a_scan_camera_of_its_own_leaves_the_publisher_alone() {
        use crate::source_spec::VideoSourceSpec as Spec;
        let usb = Spec::Camera(Some("usb".to_string()));
        assert_eq!(Camera::of(&capture("cam:0"), Some(&usb)), Camera::Free);
        assert_eq!(
            Camera::of(&capture("cam:usb"), Some(&usb)),
            Camera::Publisher
        );
        assert_eq!(Camera::of(&capture("cam"), Some(&usb)), Camera::Publisher);
        assert_eq!(Camera::of(&capture("screen"), Some(&usb)), Camera::Free);
    }

    /// The publish already failed and said so; the scan screen is not the place
    /// to report it a second time.
    #[test]
    fn a_specifier_that_never_opened_anything_holds_nothing() {
        assert_eq!(Camera::of(&capture("nonsense:"), None), Camera::Free);
    }
}
