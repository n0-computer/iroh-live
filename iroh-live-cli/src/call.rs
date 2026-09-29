//! `irl call`: a 1:1 video call.
//!
//! Each side offers its broadcast as [`CALL`] to the other peer only, and
//! subscribes to the other's. A node is called when a peer's `call` path
//! appears in its route table, and a hang-up withdraws the offer. The Android
//! demo does the same, so the two can call each other.
//!
//! Each window shows its own ticket as a QR code and can scan the peer's off
//! the camera. Whoever scans places the call, so no keyboard is needed.

use iroh_live::{
    CALL, Live,
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

    // eframe takes over the main thread. The guard lets code on it spawn onto
    // the runtime.
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

/// Binds the endpoint, opens this node's side, and prints the ticket.
async fn setup(args: &CallArgs) -> Result<Local> {
    // Opened first so the microphone can cancel its echo.
    let output = crate::playback::output(None).await?;
    let live = transport::setup_live(true).await?;
    let (live, (broadcast, sources, ticket)) = transport::with_live(live, async |live| {
        let broadcast = LocalBroadcast::new();
        let sources = source::configure(&broadcast, &args.capture, Some(&output)).await?;
        let ticket = live.ticket(CALL).to_string();
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

/// Who holds the camera while nothing is being scanned.
///
/// A capture device does not open twice. When the publisher already has the
/// camera, the scan screen reads its frames instead of opening it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Camera {
    /// The publisher has the camera, and the scan screen reads or borrows it.
    Publisher,
    /// Nothing here has the camera, so the scan screen opens it itself.
    Free,
}

impl Camera {
    /// Works out who holds the camera, from `--video` and `--scan-camera`.
    ///
    /// A `--video` that does not parse opened nothing, so the camera counts as
    /// free. A scan camera on another device leaves the publisher alone.
    fn of(args: &CaptureArgs, scan: Option<&Spec>) -> Self {
        let Ok(publishing) = args.video_source() else {
            return Self::Free;
        };
        let holds_a_camera = match &publishing {
            Spec::Camera(_) => true,
            // `rpicam-vid` goes through libcamera, not V4L2, and the two still
            // cannot hold one sensor at once.
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Spec::Rpicam(_) => true,
            _ => false,
        };
        let shared = match scan {
            None => true,
            Some(scan) => same_device(&publishing, scan),
        };
        if holds_a_camera && shared {
            Self::Publisher
        } else {
            Self::Free
        }
    }
}

/// Returns whether two camera specifiers may name the same device.
///
/// A default camera counts as the same as any named one. Guessing wrong would
/// open one device twice, which fails.
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
    //! The call window, with the same chrome as `irl watch`.

    use std::{collections::BTreeSet, sync::Arc, time::Duration};

    use eframe::egui;
    use iroh::EndpointId;
    use iroh_live::{
        Audience, BroadcastTicket, CALL, Live, Publication, Reach,
        media::{AudioOutput, LocalBroadcast, Player, SlotState, VideoSource},
    };
    use iroh_live_egui::{VideoView, egui_wgpu::RenderState, overlay::fit_to_aspect};
    use n0_error::{Result, anyerr};
    use n0_future::task::{AbortOnDropHandle, spawn};
    use n0_watcher::{Watchable, Watcher as _};
    use tokio::sync::oneshot;
    use tracing::{debug, info, warn};

    use super::{Camera, Local};
    use crate::{
        args::{CallArgs, PlaybackArgs},
        scan::ScanView,
        transport::{PEER_TIMEOUT, Subscribed},
        ui::{CursorIdle, RemoteView, TicketQr},
    };

    /// How often the state machine runs while nothing draws the window.
    const HEARTBEAT: Duration = Duration::from_millis(100);

    /// Width of the local picture-in-picture, in points.
    const PIP_WIDTH: f32 = 240.0;

    /// Aspect ratio of the local preview and the picture-in-picture.
    const ASPECT: f32 = 16.0 / 9.0;

    /// Size of the buttons outside a call.
    ///
    /// Larger than the egui default, for small touchscreens.
    const BUTTON: egui::Vec2 = egui::vec2(160.0, 36.0);

    /// Width of the calling screen's dialog, in points.
    const DIALOG_WIDTH: f32 = 280.0;

    /// Fraction of the window's shorter side the ticket QR code takes.
    const QR_FRACTION: f32 = 0.5;

    /// Smallest side of the ticket QR code, in points.
    ///
    /// The buttons under the code set the column width, so a narrower code
    /// would leave gaps beside it.
    const QR_MIN: f32 = BUTTON.x;

    /// Largest side of the ticket QR code, in points.
    ///
    /// A camera reads the code well below this size.
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
        // Parsed before the window opens, so a bad specifier fails in the
        // terminal.
        let scan_camera = crate::scan::camera_spec(args.scan_camera.as_deref())
            .map_err(|err| anyerr!("{err}"))?;
        eframe::run_native(
            "irl call",
            crate::ui::native_options(args.fullscreen),
            Box::new(move |cc| {
                let ctx = &cc.egui_ctx;
                crate::ui::spawn_ctrl_c_handler(ctx);
                let callers = Watchable::new(BTreeSet::new());
                let ringing = callers.watch();
                let watcher = spawn(watch_callers(live.clone(), callers));
                let preview = VideoView::new(
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
                    _watcher: AbortOnDropHandle::new(watcher),
                    live,
                    broadcast,
                    ringing,
                    ended: BTreeSet::new(),
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
        /// The ticket as a QR code, or `None` where it does not render.
        qr: Option<TicketQr>,
        /// The local side, offered to one peer per call.
        broadcast: LocalBroadcast,
        /// The local video, which the preview draws and the scan screen may read.
        local_video: Option<VideoSource>,
        /// The local audio, kept alive with the window.
        _local_audio: Option<iroh_live::media::AudioSource>,
        /// Where the peer's voice plays, and what the microphone cancels.
        output: AudioOutput,
        camera: Camera,
        /// The capture flags, kept to reopen the camera after a scan.
        capture: crate::args::CaptureArgs,
        /// The publisher's camera reopening after a scan.
        restoring: Option<Restoring>,
        /// Whether the scan screen took the publisher's camera and has not returned it.
        ///
        /// Settled whenever the scan screen is not up. A call answered mid-scan
        /// replaces the screen without going through [`CallApp::leave_scan`].
        camera_owed: bool,
        /// Bumped under its lock each time the scan screen takes the camera.
        ///
        /// A restore still opening the device checks it, so it cannot set the
        /// video after the scan screen cleared it.
        camera_epoch: Arc<std::sync::Mutex<u64>>,
        /// A message for the waiting screen that arrived while another was up.
        notice: Option<String>,
        /// The peers offering this node their side of a call.
        ringing: n0_watcher::Direct<BTreeSet<EndpointId>>,
        /// Peers whose call ended here and who still offer their side.
        ///
        /// Skipped until they withdraw it, so a hang-up is not answered again.
        ended: BTreeSet<EndpointId>,
        _watcher: AbortOnDropHandle<()>,
        /// Keeps the state machine ticking while nothing draws the window.
        _heartbeat: AbortOnDropHandle<()>,
        /// The attempt in flight. There is at most one.
        pending: Option<Pending>,
        screen: Screen,
        preview: VideoView,
        render_state: Option<RenderState>,
        cursor: CursorIdle,
        /// The playback flags for the peer's broadcast.
        playback: PlaybackArgs,
        /// Which camera the scan screen opens.
        scan_camera: Option<crate::source_spec::VideoSourceSpec>,
    }

    /// A task reopening the publisher's camera after a scan.
    struct Restoring {
        done: oneshot::Receiver<n0_error::Result<Option<VideoSource>>>,
        _task: AbortOnDropHandle<()>,
    }

    /// How many times the camera is tried while the scan screen releases it.
    const RESTORE_ATTEMPTS: u32 = 5;

    /// How long one attempt has to get the video running.
    ///
    /// Setting a source only spawns it. `rpicam-vid` finds out the sensor is
    /// busy only once it runs.
    const RESTORE_PATIENCE: Duration = Duration::from_secs(10);

    /// The pause between two attempts.
    const RESTORE_DELAY: Duration = Duration::from_secs(1);

    /// What the window is showing.
    enum Screen {
        /// Nobody on the line: this node's ticket and a box for the peer's.
        Waiting(Waiting),
        /// The camera, looking for the peer's ticket.
        Scanning(Box<ScanView>),
        /// A dial this window started, not yet answered.
        Calling(Calling),
        /// A call in progress.
        InCall(Box<InCall>),
    }

    impl Screen {
        /// Returns an empty waiting screen.
        fn waiting() -> Self {
            Self::Waiting(Waiting::default())
        }

        /// Returns the waiting screen showing `message`.
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
        /// The ticket being typed.
        input: String,
        /// The outcome of the last attempt, if any.
        message: Option<String>,
    }

    /// A dial in flight, as the calling screen sees it.
    #[derive(Debug)]
    struct Calling {
        /// Who is being called. The attempt itself is in [`CallApp::pending`].
        peer: String,
    }

    /// A connected call. Dropping it hangs up.
    struct InCall {
        peer: EndpointId,
        _offer: Offer,
        sub: Subscribed,
        remote: RemoteView,
    }

    impl InCall {
        /// Reports whether the peer hung up or its session ended.
        ///
        /// The peer withdrawing its offer ends the broadcast at once.
        fn ended(&self) -> bool {
            self.sub.subscription().as_moq().is_closed() || self.sub.broadcast().is_closed()
        }
    }

    /// This node's side of a call, offered to one peer until dropped.
    ///
    /// The peer sees the offer in its route table, which is how it learns it
    /// is called, and dropping the offer is how it learns of a hang-up.
    struct Offer(Publication);

    impl Offer {
        fn new(live: &Live, broadcast: &LocalBroadcast, peer: EndpointId) -> Result<Self> {
            let publication = live.moq().publish(
                live.ticket(CALL).path(),
                broadcast,
                Audience::Peers(Watchable::new(BTreeSet::from([peer]))),
            )?;
            Ok(Self(publication))
        }
    }

    impl Drop for Offer {
        fn drop(&mut self) {
            self.0.unpublish();
        }
    }

    /// A dial or an answer in flight.
    struct Pending {
        /// Which side started it.
        direction: Direction,
        /// Held here, not in the task, so abandoning the attempt withdraws it
        /// at once.
        offer: Offer,
        rx: oneshot::Receiver<Answer>,
        /// Aborting this abandons the attempt.
        _task: AbortOnDropHandle<()>,
    }

    /// The outcome of one attempt to reopen the publisher's camera.
    enum Restored {
        /// The video runs. Holds the raw source, if any, for the preview.
        Running(Option<VideoSource>),
        /// The scan screen took the camera again before the source was set.
        Superseded,
    }

    /// Opens the publisher's camera once and waits for its video to run.
    ///
    /// Sets the source under the epoch lock, and only if the epoch is still
    /// `mine`, so a scan screen that took the camera meanwhile is never undone.
    /// Fails if the video does not run within [`RESTORE_PATIENCE`].
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
                match status.borrow_and_update().video.clone() {
                    SlotState::Running => return Ok(()),
                    SlotState::Failed(err) => return Err(anyerr!("the camera failed: {err}")),
                    _ => {}
                }
                if status.changed().await.is_err() {
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
    #[derive(Debug, Clone, Copy)]
    enum Direction {
        /// This node dialed. The calling screen waits on it and can cancel it.
        Outgoing(EndpointId),
        /// A peer offered its side and this node is answering.
        ///
        /// This runs behind the current screen, and its failure only goes to
        /// the log.
        Incoming(EndpointId),
    }

    impl Direction {
        fn peer(self) -> EndpointId {
            match self {
                Self::Outgoing(peer) | Self::Incoming(peer) => peer,
            }
        }
    }

    /// The outcome of a dial or an answer.
    enum Answer {
        /// The peer is on the line and its tracks are open.
        Connected(Box<Connected>),
        /// The attempt failed, with a message for the user.
        Failed(String),
    }

    /// The peer's side of an established call, playing.
    struct Connected {
        sub: Subscribed,
        player: Player,
    }

    /// A waiting screen action, applied once the panel releases its borrow.
    enum Action {
        /// Open the camera and look for a ticket.
        Scan,
        /// Call whatever was typed or pasted.
        Dial(String),
    }

    impl eframe::App for CallApp {
        /// Drives the state machine.
        ///
        /// eframe skips `ui` while the window is minimized or occluded, and the
        /// window still has to answer calls then.
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
            // Before the screen match, so Escape works on every screen.
            crate::ui::escape_leaves_fullscreen(&ctx);
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
            self.screen = Screen::waiting();
            self.pending = None;
            crate::ui::shutdown_publish_blocking(&self.live, &self.broadcast);
        }
    }

    impl CallApp {
        /// Calls the ticket the camera has read, if any.
        fn poll_scan(&mut self, ctx: &egui::Context) {
            let Screen::Scanning(view) = &self.screen else {
                return;
            };
            let Some(ticket) = view.ticket() else {
                return;
            };
            self.dial(ctx, ticket);
        }

        /// Handles an attempt that finished since the last pass.
        fn poll_pending(&mut self, ctx: &egui::Context) {
            let Some(pending) = self.pending.as_mut() else {
                return;
            };
            let answer = match pending.rx.try_recv() {
                Ok(answer) => answer,
                Err(oneshot::error::TryRecvError::Empty) => return,
                // The task dropped its sender, which only happens at shutdown.
                Err(oneshot::error::TryRecvError::Closed) => {
                    Answer::Failed("the call attempt stopped".to_string())
                }
            };
            let Pending {
                direction, offer, ..
            } = self
                .pending
                .take()
                .expect("the attempt was borrowed a moment ago");

            let message = match answer {
                Answer::Connected(connected) => {
                    return self.enter_call(ctx, direction.peer(), offer, *connected);
                }
                Answer::Failed(message) => message,
            };
            self.ended.insert(direction.peer());
            let peer = direction.peer().fmt_short();
            match direction {
                Direction::Outgoing(_) => {
                    warn!(%peer, %message, "the call failed");
                    self.screen = Screen::reporting(message);
                }
                Direction::Incoming(_) => debug!(%peer, %message, "answering failed"),
            }
        }

        /// Moves to the in-call screen and opens the peer's video for drawing.
        fn enter_call(
            &mut self,
            ctx: &egui::Context,
            peer: EndpointId,
            offer: Offer,
            connected: Connected,
        ) {
            let Connected { sub, player } = connected;
            info!(remote = %peer.fmt_short(), "call connected");
            let remote = RemoteView::new(
                ctx,
                "call-remote",
                player,
                self.playback.decoder,
                self.render_state.as_ref(),
            )
            .with_link(sub.subscription().clone());
            self.screen = Screen::InCall(Box::new(InCall {
                peer,
                _offer: offer,
                sub,
                remote,
            }));
        }

        /// Returns to the waiting screen once the peer hangs up.
        fn poll_hangup(&mut self) {
            let Screen::InCall(call) = &self.screen else {
                return;
            };
            if !call.ended() {
                return;
            }
            info!("call ended");
            self.hang_up("the call ended");
        }

        /// Ends the call on screen, if any, and shows `message`.
        fn hang_up(&mut self, message: &str) {
            let screen = std::mem::replace(&mut self.screen, Screen::reporting(message.to_owned()));
            if let Screen::InCall(call) = screen {
                self.ended.insert(call.peer);
            }
        }

        /// Answers a peer that offers its side, if this node is idle.
        fn answer_next(&mut self, ctx: &egui::Context) {
            let ringing = self.ringing.get();
            self.ended.retain(|peer| ringing.contains(peer));
            if self.pending.is_some() || matches!(self.screen, Screen::InCall(_)) {
                return;
            }
            let Some(peer) = ringing.into_iter().find(|peer| !self.ended.contains(peer)) else {
                return;
            };
            info!(remote = %peer.fmt_short(), "answering");
            self.start(ctx, Direction::Incoming(peer));
        }

        /// Calls `ticket`, replacing whatever the window was doing.
        ///
        /// Leaves the scan screen first, so the publisher gets its camera back
        /// while the dial runs.
        fn dial(&mut self, ctx: &egui::Context, ticket: BroadcastTicket) {
            let peer = ticket.peer();
            info!(remote = %peer.fmt_short(), "dialing");
            self.leave_scan();
            self.discard_pending();
            self.screen = Screen::Calling(Calling {
                peer: peer.fmt_short().to_string(),
            });
            self.start(ctx, Direction::Outgoing(peer));
        }

        /// Offers this node's side to the peer and waits for the peer's, as the pending attempt.
        fn start(&mut self, ctx: &egui::Context, direction: Direction) {
            let peer = direction.peer();
            let offer = match Offer::new(&self.live, &self.broadcast, peer) {
                Ok(offer) => offer,
                Err(err) => {
                    warn!(remote = %peer.fmt_short(), "cannot offer this side: {err:#}");
                    self.ended.insert(peer);
                    if let Direction::Outgoing(_) = direction {
                        self.screen = Screen::reporting(format!("{err:#}"));
                    }
                    return;
                }
            };
            let attempt = reach(self.live.clone(), peer, self.playback, self.output.clone());
            let (tx, rx) = oneshot::channel();
            let ctx = ctx.clone();
            let task = spawn(async move {
                if tx.send(attempt.await).is_err() {
                    info!("the call landed after it was given up on, dropping it");
                }
                ctx.request_repaint();
            });
            self.pending = Some(Pending {
                direction,
                offer,
                rx,
                _task: AbortOnDropHandle::new(task),
            });
        }

        /// Abandons the attempt in flight, withdrawing this node's offer.
        fn discard_pending(&mut self) {
            if let Some(pending) = self.pending.take() {
                self.ended.insert(pending.direction.peer());
            }
        }

        /// Gives up on the dial in flight and returns to the waiting screen.
        fn cancel(&mut self, peer: &str) {
            info!(%peer, "the call was cancelled");
            self.discard_pending();
            self.screen = Screen::reporting(format!("stopped calling {peer}"));
        }

        /// Switches to the scan screen.
        ///
        /// A publisher holding the camera lends the scan screen its frames and
        /// keeps publishing. Otherwise the scan screen opens the camera itself.
        fn enter_scan(&mut self, ctx: &egui::Context) {
            info!("scanning for a ticket");
            let view = match (&self.local_video, self.camera) {
                (Some(camera), Camera::Publisher) => {
                    ScanView::from_frames(ctx, self.render_state.as_ref(), None, camera.frames())
                }
                (None, Camera::Publisher) => {
                    // The publisher holds the camera but has no frames to
                    // lend, since `rpicam` outputs encoded H.264. It releases
                    // the sensor for the scan and reopens it afterwards.
                    // Subscribers see the video pause.
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

        /// Closes the scan screen and shows the waiting screen.
        ///
        /// Dropping the view releases a camera it opened.
        /// [`Self::settle_camera`] gives the publisher its camera back.
        fn leave_scan(&mut self) {
            if !matches!(self.screen, Screen::Scanning(_)) {
                return;
            }
            self.screen = Screen::waiting();
        }

        /// Gives the publisher its camera back once the scan screen is gone.
        fn settle_camera(&mut self) {
            if self.camera_owed && !matches!(self.screen, Screen::Scanning(_)) {
                self.camera_owed = false;
                self.restore_camera();
            }
        }

        /// Reopens the publisher's camera, retrying while the scan screen releases it.
        ///
        /// An attempt counts only once the video runs. A new `rpicam-vid` that
        /// finds the sensor busy exits, which shows as a failed video, not as
        /// an error from setting the source.
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

        /// Collects the result of reopening the publisher's camera.
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

        /// Shows `message` on the waiting screen, if it is up.
        fn report(&mut self, message: String) {
            if let Screen::Waiting(waiting) = &mut self.screen {
                waiting.message = Some(message);
            }
        }

        /// Moves the kept notice onto the waiting screen once it is up and free.
        fn show_notice(&mut self) {
            if let Screen::Waiting(waiting) = &mut self.screen
                && waiting.message.is_none()
                && let Some(message) = self.notice.take()
            {
                waiting.message = Some(message);
            }
        }

        /// Draws the local picture behind the screens without remote video.
        fn draw_backdrop(&mut self, ui: &mut egui::Ui) {
            let available = ui.available_size();
            let size = fit_to_aspect(available, ASPECT);
            let image = self.preview.render();
            ui.centered_and_justified(|ui| ui.add_sized(size, image));
        }

        /// Draws the waiting screen.
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
                // An incoming attempt only shows a spinner. The session may
                // not be a caller.
                if answering {
                    ui.spinner();
                }
                if let Some(message) = &waiting.message {
                    ui.colored_label(egui::Color32::LIGHT_YELLOW, message);
                }
            });

            match action {
                Some(Action::Scan) => self.enter_scan(ctx),
                Some(Action::Dial(text)) => match text.parse::<BroadcastTicket>() {
                    Ok(ticket) => self.dial(ctx, ticket),
                    Err(err) => self.report(format!("that is not a ticket: {err}")),
                },
                None => {}
            }
        }

        /// Draws the scan screen and its cancel button.
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

        /// Draws the calling screen and its cancel button.
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

        /// Draws the call: the peer, this node in a corner, and the overlay.
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
                        .show(ui, |ui| ui.add_sized(pip, preview.render()));
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
                self.hang_up("you hung up");
            }
        }
    }

    /// Draws the ticket box and the call button, and returns an entered ticket.
    fn paste_row(ui: &mut egui::Ui, width: f32, waiting: &mut Waiting) -> Option<String> {
        let text = egui::TextEdit::singleline(&mut waiting.input).hint_text("Their ticket");
        let response = ui.add_sized(egui::vec2(width, BUTTON.y), text);
        let ready = !waiting.input.trim().is_empty();
        let clicked = ui
            .add_enabled(ready, egui::Button::new("Call").min_size(BUTTON))
            .clicked();
        let submitted =
            ready && response.lost_focus() && ui.input(|state| state.key_pressed(egui::Key::Enter));
        if clicked || submitted {
            Some(waiting.input.trim().to_string())
        } else {
            None
        }
    }

    /// Returns the ticket QR code's side, in points, for a window of `content` size.
    ///
    /// Scales with the shorter side, so the buttons under the code still fit
    /// on a small touchscreen.
    fn qr_side(content: egui::Vec2) -> f32 {
        (content.x.min(content.y) * QR_FRACTION).clamp(QR_MIN, QR_MAX)
    }

    /// Tracks which peers offer this node their side of a call.
    ///
    /// A caller offers `live/<its id>/call` to the callee only, so the path
    /// appearing in the route table is the ring. Runs for the window's life.
    async fn watch_callers(live: Live, callers: Watchable<BTreeSet<EndpointId>>) {
        let me = live.endpoint().id();
        let mut updates = live.moq().origin().announced();
        while let Some(update) = updates.next().await {
            let Some(caller) = BroadcastTicket::from_path(update.prefix.as_str())
                .filter(|ticket| ticket.name() == CALL && ticket.peer() != me)
                .map(|ticket| ticket.peer())
            else {
                continue;
            };
            let mut ringing = callers.get();
            if update.kind.is_active() {
                debug!(remote = %caller.fmt_short(), "ringing");
                ringing.insert(caller);
            } else {
                ringing.remove(&caller);
            }
            callers.set(ringing).ok();
        }
    }

    /// Waits for `peer`'s side of the call, then plays it.
    ///
    /// Gives up after [`PEER_TIMEOUT`], for a peer that is busy or away.
    async fn reach(
        live: Live,
        peer: EndpointId,
        playback: PlaybackArgs,
        output: AudioOutput,
    ) -> Answer {
        let path = BroadcastTicket::new(peer, CALL).path();
        let subscription = match tokio::time::timeout(
            PEER_TIMEOUT,
            live.moq().subscribe(path, Reach::Direct(peer)),
        )
        .await
        {
            Ok(Ok(subscription)) => subscription,
            Ok(Err(err)) => return Answer::Failed(format!("{err:#}")),
            Err(_) => {
                return Answer::Failed(format!(
                    "gave up after {}s: the peer did not answer",
                    PEER_TIMEOUT.as_secs()
                ));
            }
        };
        let sub = Subscribed::open(&live, subscription);
        let config = crate::ui::player_config(&playback, Some(&output));
        match sub.broadcast().play(config) {
            Ok(player) => Answer::Connected(Box::new(Connected { sub, player })),
            Err(err) => Answer::Failed(format!("{err:#}")),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{QR_MAX, QR_MIN, egui, qr_side};

        /// A small window draws the code below [`QR_MAX`], so the buttons fit.
        #[test]
        fn a_small_window_draws_the_code_smaller() {
            let side = qr_side(egui::vec2(480.0, 320.0));
            assert!(side < QR_MAX, "unexpected: {side}");
            assert!(side >= QR_MIN, "unexpected: {side}");
        }

        /// The code stops growing at [`QR_MAX`].
        #[test]
        fn a_large_window_stops_growing_the_code() {
            assert_eq!(qr_side(egui::vec2(2560.0, 1440.0)), QR_MAX);
        }

        /// A tiny window still draws the code at [`QR_MIN`].
        #[test]
        fn a_window_with_no_room_still_draws_a_whole_code() {
            assert_eq!(qr_side(egui::vec2(40.0, 20.0)), QR_MIN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Camera, CaptureArgs};

    /// Returns capture flags with the given `--video` specifier.
    fn capture(video: &str) -> CaptureArgs {
        CaptureArgs {
            video: video.to_string(),
            ..Default::default()
        }
    }

    /// A camera publisher lends its device to the scan screen.
    #[test]
    fn a_camera_publisher_hands_the_scan_screen_its_device() {
        assert_eq!(Camera::of(&capture("cam"), None), Camera::Publisher);
        assert_eq!(Camera::of(&capture("cam:0"), None), Camera::Publisher);
    }

    /// A publisher without a camera leaves the camera free.
    #[test]
    fn a_publisher_of_anything_else_keeps_its_source() {
        assert_eq!(Camera::of(&capture("screen"), None), Camera::Free);
        assert_eq!(Camera::of(&capture("test"), None), Camera::Free);
        assert_eq!(Camera::of(&capture("none"), None), Camera::Free);
    }

    /// A scan camera on another device leaves the publisher alone.
    ///
    /// One that may be the same device is shared.
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

    /// A `--video` that does not parse holds no camera.
    #[test]
    fn a_specifier_that_never_opened_anything_holds_nothing() {
        assert_eq!(Camera::of(&capture("nonsense:"), None), Camera::Free);
    }
}
