//! The call window, with the same chrome as `irl watch`.

use std::{collections::BTreeSet, sync::Arc, task::Poll};

use eframe::egui;
use iroh::EndpointId;
use iroh_live::{
    BroadcastTicket,
    calls::{Call, Calls},
    media::{LocalBroadcast, Player, VideoSource},
};
use moq_ext_egui::{VideoView, egui_wgpu::RenderState};
use n0_error::Result;
use n0_future::task::AbortOnDropHandle;
use n0_watcher::Watcher as _;
use tracing::{debug, info, warn};

use super::{
    Local,
    camera::Camera,
    screens::{Calling, InCall, Screen},
};
use crate::{
    args::{CallArgs, PlaybackArgs},
    playback::Speaker,
    transport::{PEER_TIMEOUT, SharedNode, Subscribed},
    ui::{CursorIdle, Pending, RemoteView, TicketQr},
};

/// Opens the call window and runs it until it closes.
pub(super) fn run(local: Local, args: CallArgs) -> Result {
    let Local {
        node,
        broadcast,
        calls,
        sources,
        output,
        ticket,
    } = local;
    // Parsed before the window opens, so a bad specifier fails in the
    // terminal.
    let scan_camera = args.scan_camera.clone();
    crate::ui::run_window("irl call", args.fullscreen, move |cc| {
        let ctx = &cc.egui_ctx;
        let ringing = calls.ringing();
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
            _heartbeat: crate::ui::spawn_heartbeat(ctx),
            node,
            broadcast,
            calls,
            ringing,
            ended: BTreeSet::new(),
            pending: None,
            screen: Screen::waiting(),
            cursor: CursorIdle::default(),
        };
        if let Some(ticket) = args.ticket {
            app.dial(ctx, ticket);
        }
        app
    })
}

/// The call window.
pub(super) struct CallApp {
    pub(super) node: SharedNode,
    /// This node's ticket, shown in the top bar and as a QR code.
    pub(super) ticket: String,
    /// The ticket as a QR code, or `None` where it does not render.
    pub(super) qr: Option<TicketQr>,
    /// The local side, published once and offered to one peer per call.
    pub(super) broadcast: LocalBroadcast,
    /// Who the local side is offered to.
    pub(super) calls: Calls,
    /// The local video, which the preview draws and the scan screen may read.
    pub(super) local_video: Option<VideoSource>,
    /// The local audio, kept alive with the window.
    pub(super) _local_audio: Option<iroh_live::media::AudioSource>,
    /// Where the peer's voice plays, and what the microphone cancels.
    pub(super) output: Option<Speaker>,
    pub(super) camera: Camera,
    /// The capture flags, kept to reopen the camera after a scan.
    pub(super) capture: crate::args::CaptureArgs,
    /// The publisher's camera reopening after a scan.
    pub(super) restoring: Option<Pending<Result<Option<VideoSource>>>>,
    /// Whether the scan screen took the publisher's camera and has not returned it.
    ///
    /// Settled whenever the scan screen is not up. A call answered mid-scan
    /// replaces the screen without going through [`CallApp::leave_scan`].
    pub(super) camera_owed: bool,
    /// Bumped under its lock each time the scan screen takes the camera.
    ///
    /// A restore still opening the device checks it, so it cannot set the
    /// video after the scan screen cleared it.
    pub(super) camera_epoch: Arc<std::sync::Mutex<u64>>,
    /// A message for the waiting screen that arrived while another was up.
    pub(super) notice: Option<String>,
    /// The peers offering this node their side of a call.
    pub(super) ringing: n0_watcher::Direct<BTreeSet<EndpointId>>,
    /// Peers whose call ended here and who still offer their side.
    ///
    /// Skipped until they withdraw it, so a hang-up is not answered again.
    pub(super) ended: BTreeSet<EndpointId>,
    /// Keeps the state machine ticking while nothing draws the window.
    pub(super) _heartbeat: AbortOnDropHandle<()>,
    /// The attempt in flight. There is at most one.
    pub(super) pending: Option<Attempt>,
    pub(super) screen: Screen,
    pub(super) preview: VideoView,
    pub(super) render_state: Option<RenderState>,
    pub(super) cursor: CursorIdle,
    /// The playback flags for the peer's broadcast.
    pub(super) playback: PlaybackArgs,
    /// Which camera the scan screen opens.
    pub(super) scan_camera: Option<crate::source_spec::VideoSourceSpec>,
}

/// A dial or an answer in flight.
pub(super) struct Attempt {
    /// Which side started it.
    pub(super) direction: Direction,
    /// Dropping this abandons the attempt, which withdraws the offer.
    pub(super) answer: Pending<Answer>,
}

/// Which side started an attempt.
#[derive(Debug, Clone, Copy)]
pub(super) enum Direction {
    /// This node dialed. The calling screen waits on it and can cancel it.
    Outgoing(EndpointId),
    /// A peer offered its side and this node is answering.
    ///
    /// This runs behind the current screen, and its failure only goes to
    /// the log.
    Incoming(EndpointId),
}

impl Direction {
    /// Returns the other side of the call.
    pub(super) fn peer(self) -> EndpointId {
        match self {
            Self::Outgoing(peer) | Self::Incoming(peer) => peer,
        }
    }
}

/// The outcome of a dial or an answer.
pub(super) enum Answer {
    /// The peer is on the line and its tracks are open.
    Connected(Box<Connected>),
    /// The attempt failed, with a message for the user.
    Failed(String),
}

/// The peer's side of an established call, playing.
pub(super) struct Connected {
    pub(super) call: Call,
    pub(super) sub: Subscribed,
    pub(super) player: Player,
}

/// A waiting screen action, applied once the panel releases its borrow.
pub(super) enum Action {
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
        self.poll_scan(ctx);
        self.poll_restore();
        self.poll_pending(ctx);
        self.poll_hangup();
        self.answer_next(ctx);
        self.settle_camera(ctx);
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
        crate::ui::shutdown_publish_blocking(&self.node, &self.broadcast);
    }
}

impl CallApp {
    /// Calls the ticket the camera has read, if any.
    pub(super) fn poll_scan(&mut self, ctx: &egui::Context) {
        let Screen::Scanning(view) = &self.screen else {
            return;
        };
        let Some(ticket) = view.ticket() else {
            return;
        };
        self.dial(ctx, ticket);
    }

    /// Handles an attempt that finished since the last pass.
    pub(super) fn poll_pending(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let Poll::Ready(answer) = pending.answer.poll() else {
            return;
        };
        let answer =
            answer.unwrap_or_else(|| Answer::Failed("the call attempt stopped".to_string()));
        let Attempt { direction, .. } = self
            .pending
            .take()
            .expect("the attempt was borrowed a moment ago");

        let message = match answer {
            Answer::Connected(connected) => {
                return self.enter_call(ctx, direction.peer(), *connected);
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
    pub(super) fn enter_call(
        &mut self,
        ctx: &egui::Context,
        peer: EndpointId,
        connected: Connected,
    ) {
        let Connected { call, sub, player } = connected;
        info!(peer = %peer.fmt_short(), "call connected");
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
            _call: call,
            sub,
            remote,
        }));
    }

    /// Returns to the waiting screen once the peer hangs up.
    pub(super) fn poll_hangup(&mut self) {
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
    pub(super) fn hang_up(&mut self, message: &str) {
        let screen = std::mem::replace(&mut self.screen, Screen::reporting(message.to_owned()));
        if let Screen::InCall(call) = screen {
            self.ended.insert(call.peer);
        }
    }

    /// Answers a peer that offers its side, if this node is idle.
    pub(super) fn answer_next(&mut self, ctx: &egui::Context) {
        let ringing = self.ringing.get();
        self.ended.retain(|peer| ringing.contains(peer));
        if self.pending.is_some() || matches!(self.screen, Screen::InCall(_)) {
            return;
        }
        let Some(peer) = ringing.into_iter().find(|peer| !self.ended.contains(peer)) else {
            return;
        };
        info!(peer = %peer.fmt_short(), "answering");
        self.start(ctx, Direction::Incoming(peer));
    }

    /// Calls `ticket`, replacing whatever the window was doing.
    ///
    /// Leaves the scan screen first, so the publisher gets its camera back
    /// while the dial runs.
    pub(super) fn dial(&mut self, ctx: &egui::Context, ticket: BroadcastTicket) {
        let peer = ticket.peer();
        info!(peer = %peer.fmt_short(), "dialing");
        self.leave_scan();
        self.discard_pending();
        self.screen = Screen::Calling(Calling {
            peer: peer.fmt_short().to_string(),
        });
        self.start(ctx, Direction::Outgoing(peer));
    }

    /// Offers this node's side to the peer and waits for the peer's, as the pending attempt.
    pub(super) fn start(&mut self, ctx: &egui::Context, direction: Direction) {
        let attempt = reach(
            self.calls.clone(),
            direction,
            self.playback,
            self.output.clone(),
        );
        let answer = Pending::spawn(ctx, attempt, |_| {
            info!("the call landed after it was given up on, dropping it");
        });
        self.pending = Some(Attempt { direction, answer });
    }

    /// Abandons the attempt in flight, withdrawing this node's offer.
    pub(super) fn discard_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.ended.insert(pending.direction.peer());
        }
    }

    /// Gives up on the dial in flight and returns to the waiting screen.
    pub(super) fn cancel(&mut self, peer: &str) {
        info!(%peer, "the call was cancelled");
        self.discard_pending();
        self.screen = Screen::reporting(format!("stopped calling {peer}"));
    }

    /// Shows `message` on the waiting screen, if it is up.
    pub(super) fn report(&mut self, message: String) {
        if let Screen::Waiting(waiting) = &mut self.screen {
            waiting.message = Some(message);
        }
    }

    /// Moves the kept notice onto the waiting screen once it is up and free.
    pub(super) fn show_notice(&mut self) {
        if let Screen::Waiting(waiting) = &mut self.screen
            && waiting.message.is_none()
            && let Some(message) = self.notice.take()
        {
            waiting.message = Some(message);
        }
    }
}

/// Offers this node's side to the peer, waits for the peer's, and plays it.
///
/// Gives up after [`PEER_TIMEOUT`], for a peer that is busy or away.
/// Dropping the future withdraws the offer.
pub(super) async fn reach(
    calls: Calls,
    direction: Direction,
    playback: PlaybackArgs,
    output: Option<Speaker>,
) -> Answer {
    let peer = direction.peer();
    let placing = async {
        match direction {
            Direction::Outgoing(_) => calls.call(peer).await,
            Direction::Incoming(_) => calls.answer(peer).await,
        }
    };
    let call = match tokio::time::timeout(PEER_TIMEOUT, placing).await {
        Ok(Ok(call)) => call,
        Ok(Err(err)) => return Answer::Failed(format!("{err:#}")),
        Err(_) => {
            return Answer::Failed(format!(
                "gave up after {}s: the peer did not answer",
                PEER_TIMEOUT.as_secs()
            ));
        }
    };
    let sub = Subscribed::open(call.subscription().clone());
    let config = crate::playback::player_config(&playback, output.as_ref());
    match crate::playback::open(sub, config).await {
        Ok((sub, player)) => Answer::Connected(Box::new(Connected { call, sub, player })),
        Err(err) => Answer::Failed(format!("{err:#}")),
    }
}
