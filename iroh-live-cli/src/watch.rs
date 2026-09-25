//! `irl watch`: subscribe to a remote broadcast and play it.
//!
//! Unless `--rendition` pins one, the player switches renditions to follow the
//! downlink. `--scan` opens the window on the camera and connects to the ticket
//! in a QR code held up to it (see [`crate::scan`]).

use std::time::Duration;

use iroh_live::{
    BroadcastTicket, Live,
    media::{AudioOutput, Player, PlayerConfig, RenditionMode},
};
use n0_error::{Result, anyerr};

use crate::{
    args::WatchArgs,
    transport::{self, Subscribed},
};

/// Where the first ticket comes from, and who dials it.
#[derive(Debug)]
enum Start {
    /// A ticket the terminal dials before any window opens.
    Ticket(BroadcastTicket),
    /// A ticket the window dials, so `--scan` has a screen to cancel into.
    TicketInWindow(BroadcastTicket),
    /// No ticket: the camera reads one.
    Scan,
}

/// The parts of [`WatchArgs`] a subscription needs, owned so a task can hold them.
#[derive(Debug, Clone)]
struct Options {
    /// The rendition `--rendition` pinned.
    rendition: Option<String>,
    /// The camera the scan screen opens.
    scan_camera: Option<crate::source_spec::VideoSourceSpec>,
    /// Plays audio only, for `--no-video`.
    no_video: bool,
    /// How the video is decoded.
    playback: crate::args::PlaybackArgs,
    /// Where the audio plays.
    output: AudioOutput,
}

impl From<&WatchArgs> for Options {
    fn from(args: &WatchArgs) -> Self {
        Self {
            rendition: args.rendition.clone(),
            // Parsed by the caller, which can report a bad specifier.
            scan_camera: None,
            no_video: args.no_video,
            playback: args.playback,
            // Opened by the async `setup`.
            output: AudioOutput::null(),
        }
    }
}

/// Runs the `watch` command.
pub fn run(args: WatchArgs, rt: &tokio::runtime::Runtime) -> Result {
    let start = start(&args)?;

    let mut options = Options::from(&args);
    options.scan_camera =
        crate::scan::camera_spec(args.scan_camera.as_deref()).map_err(|err| anyerr!("{err}"))?;
    let (live, output) = rt.block_on(setup(&args))?;
    options.output = output;

    let ticket = match start {
        // eframe takes the main thread. The guard lets it spawn onto the runtime.
        Start::Scan => {
            let _guard = rt.enter();
            return window::run(live, window::Opening::Scanning, options, args.fullscreen);
        }
        Start::TicketInWindow(ticket) => {
            let _guard = rt.enter();
            let opening = window::Opening::Connecting(Box::new(ticket));
            return window::run(live, opening, options, args.fullscreen);
        }
        Start::Ticket(ticket) => ticket,
    };

    let (live, (sub, player)) = rt.block_on(transport::with_live(live, async |live| {
        connect(live, &ticket, &options, None).await
    }))?;

    if args.no_video {
        return wait_for_ctrl_c(rt, live, sub, player);
    }

    let _guard = rt.enter();
    let connected = window::Connected {
        ticket,
        sub,
        player,
    };
    let opening = window::Opening::Watching(Box::new(connected));
    window::run(live, opening, options, args.fullscreen)
}

/// Decides where the first ticket comes from and who dials it.
///
/// With `--scan`, a ticket is dialed from the window, because the machines
/// `--scan` is for often have no keyboard to press Ctrl+C on.
///
/// # Errors
///
/// Fails if nothing names a broadcast and `--scan` is not set.
fn start(args: &WatchArgs) -> Result<Start> {
    // clap rejects `--scan` with `--no-video`, so both paths open a window.
    if args.scan {
        return Ok(args
            .remote
            .ticket()
            .map_or(Start::Scan, Start::TicketInWindow));
    }

    let ticket = args
        .remote
        .ticket()
        .map_err(|err| anyerr!("{err}, or pass --scan to read one off a QR code"))?;
    Ok(Start::Ticket(ticket))
}

/// Opens the audio output and binds the endpoint every subscription runs on.
///
/// # Errors
///
/// Fails if `--audio-output` names a device that will not open, or if the
/// endpoint cannot bind.
async fn setup(args: &WatchArgs) -> Result<(Live, AudioOutput)> {
    #[cfg(feature = "playback")]
    let device = args.audio_output.clone();
    #[cfg(not(feature = "playback"))]
    let device = {
        let _ = args;
        None
    };
    let output = crate::playback::output(device).await?;
    Ok((transport::setup_live(false).await?, output))
}

/// Connects to `ticket` and starts playing.
///
/// `dial_deadline` bounds only the subscribe, not the wait for the catalog.
///
/// # Errors
///
/// Fails if the peer does not answer in time, if the catalog does not arrive,
/// or if the broadcast does not offer the pinned rendition.
async fn connect(
    live: &Live,
    ticket: &BroadcastTicket,
    options: &Options,
    dial_deadline: Option<Duration>,
) -> Result<(Subscribed, Player)> {
    let subscribing = transport::subscribe(live, ticket);
    let sub = match dial_deadline {
        Some(deadline) => tokio::time::timeout(deadline, subscribing)
            .await
            .map_err(|_| {
                anyerr!(
                    "no answer from {} within {}s: is the publisher running?",
                    ticket.peer().fmt_short(),
                    deadline.as_secs()
                )
            })??,
        None => subscribing.await?,
    };
    // A broadcast without a catalog fails here instead of opening a black window.
    let catalog = crate::playback::catalog(sub.broadcast()).await?;
    if let Some(name) = &options.rendition {
        catalog.video_rendition(name)?;
    }

    let config = crate::ui::player_config(&options.playback, Some(&options.output));
    let rendition = match (options.no_video, &options.rendition) {
        // An unused video decoder would still cost a core.
        (true, _) => RenditionMode::Off,
        (false, Some(name)) => RenditionMode::pinned(name.clone()),
        (false, None) => config.rendition.clone(),
    };
    let config = PlayerConfig {
        rendition,
        ..config
    };
    let player = sub.broadcast().play(config)?;
    Ok((sub, player))
}

/// Plays until the user interrupts, with no window.
fn wait_for_ctrl_c(
    rt: &tokio::runtime::Runtime,
    live: Live,
    sub: Subscribed,
    player: Player,
) -> Result {
    println!("playing, press Ctrl+C to stop");
    rt.block_on(async move {
        tokio::signal::ctrl_c().await?;
        drop(player);
        sub.close();
        live.shutdown().await;
        Ok(())
    })
}

mod window {
    //! The player window, with its scan, connecting and stopped screens.

    use std::time::{Duration, Instant};

    use eframe::egui;
    use iroh_live::{
        BroadcastTicket, Live,
        media::{Player, RenditionMode},
    };
    use iroh_live_egui::egui_wgpu::RenderState;
    use n0_error::{Result, anyerr};
    use n0_future::task::{AbortOnDropHandle, spawn};
    use tokio::sync::oneshot;
    use tracing::{info, warn};

    use super::{Options, connect};
    use crate::{
        scan::{ScanView, Skip},
        transport::Subscribed,
        ui::{CursorIdle, RemoteView},
    };

    /// The top bar title while nothing is playing.
    const IDLE_TITLE: &str = "irl watch";

    /// How often the window is woken while nothing draws it.
    ///
    /// A dial can finish while the window is hidden, and the state machine
    /// still has to pick it up.
    const HEARTBEAT: Duration = Duration::from_millis(100);

    /// The size of the buttons on screens without a picture.
    ///
    /// Large enough to hit on a small touchscreen.
    const BUTTON: egui::Vec2 = egui::vec2(200.0, 40.0);

    /// How many characters of a broadcast name a button label shows.
    const LABEL_CHARS: usize = 24;

    /// What the window shows when it opens.
    pub(super) enum Opening {
        /// A subscription the terminal already opened.
        Watching(Box<Connected>),
        /// A ticket to dial from inside the window.
        Connecting(Box<BroadcastTicket>),
        /// The scan screen.
        Scanning,
    }

    /// Opens the player window and runs it until it closes.
    pub(super) fn run(live: Live, opening: Opening, options: Options, fullscreen: bool) -> Result {
        eframe::run_native(
            "irl watch",
            crate::ui::native_options(fullscreen),
            Box::new(move |cc| {
                let ctx = &cc.egui_ctx;
                crate::ui::spawn_ctrl_c_handler(ctx);
                let render_state = cc.wgpu_render_state.clone();
                let mode = match opening {
                    Opening::Watching(connected) => {
                        watching(ctx, *connected, &options, render_state.as_ref())
                    }
                    Opening::Connecting(ticket) => {
                        Mode::Connecting(Box::new(connecting(ctx, &live, &options, *ticket, None)))
                    }
                    Opening::Scanning => scanning(
                        ctx,
                        render_state.as_ref(),
                        None,
                        None,
                        options.scan_camera.clone(),
                    ),
                };
                Ok(Box::new(WatchApp {
                    live,
                    options,
                    render_state,
                    mode,
                    message: None,
                    cursor: CursorIdle::default(),
                    refused: None,
                    _heartbeat: crate::ui::spawn_heartbeat(ctx, HEARTBEAT),
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    /// What the window is doing.
    enum Mode {
        /// Idle after a cancelled dial.
        Stopped(Option<Previous>),
        /// Looking for a ticket in the camera picture.
        Scanning(Box<Scanning>),
        /// Dialing a ticket.
        Connecting(Box<Connecting>),
        /// Playing a broadcast.
        Watching(Box<Watching>),
    }

    /// The broadcast a screen without a picture offers to go back to.
    ///
    /// Going back dials the ticket again. [`WatchApp::enter_scan`] says why.
    #[derive(Debug, Clone)]
    struct Previous {
        ticket: BroadcastTicket,
        /// The broadcast name, for the button label.
        name: String,
    }

    /// Returns the label for a button that goes back to the broadcast `name`.
    fn back_label(name: &str) -> String {
        match name.char_indices().nth(LABEL_CHARS) {
            Some((end, _)) => format!("Back to {}...", &name[..end]),
            None => format!("Back to {name}"),
        }
    }

    /// How long a dial from the window may take before it fails.
    ///
    /// A running publisher answers within a few seconds even over a relay.
    /// Twenty seconds leaves room for a slow hole-punch. The terminal path has
    /// no limit.
    const DIAL_DEADLINE: Duration = Duration::from_secs(20);

    /// How long the scanner ignores a ticket whose dial just failed.
    const REDIAL_WAIT: Duration = Duration::from_secs(3);

    /// The cap for [`REDIAL_WAIT`] as it doubles.
    const REDIAL_WAIT_MAX: Duration = Duration::from_secs(30);

    /// A ticket whose dial failed, and how many times in a row.
    ///
    /// The code is usually still in front of the camera. Without this, the
    /// scanner reads it again at once and redials a peer that just failed.
    /// A successful dial clears it.
    struct Refused {
        ticket: BroadcastTicket,
        /// Consecutive failures, starting at zero.
        strikes: u32,
    }

    impl Refused {
        /// Returns how long the scanner ignores this ticket.
        ///
        /// The wait doubles with each failure.
        fn wait(&self) -> Duration {
            REDIAL_WAIT
                .saturating_mul(1u32 << self.strikes.min(16))
                .min(REDIAL_WAIT_MAX)
        }
    }

    /// The scan screen, and what it can go back to.
    struct Scanning {
        view: ScanView,
        previous: Option<Previous>,
    }

    /// A dial in flight.
    struct Connecting {
        /// The ticket being dialed.
        ticket: BroadcastTicket,
        /// What the stopped screen offers to go back to if this is cancelled.
        previous: Option<Previous>,
        rx: oneshot::Receiver<Attempt>,
        task: AbortOnDropHandle<()>,
    }

    /// The outcome of a dial.
    enum Attempt {
        /// The broadcast is open and playing.
        Connected(Box<Connected>),
        /// The dial failed, with a message for the screen.
        Failed(String),
    }

    /// An open subscription and its player.
    pub(super) struct Connected {
        /// The dialed ticket, kept to dial again after a trip to the scanner.
        pub(super) ticket: BroadcastTicket,
        pub(super) sub: Subscribed,
        pub(super) player: Player,
    }

    impl Connected {
        /// Closes a subscription that no screen will show.
        ///
        /// Closing tells the peer. Dropping would leave it to time out.
        fn discard(self) {
            let Self { sub, player, .. } = self;
            drop(player);
            sub.close();
        }
    }

    /// A broadcast on screen.
    struct Watching {
        /// The broadcast name, shown in the top bar.
        title: String,
        /// The dialed ticket, to come back to after a trip to the scanner.
        ticket: BroadcastTicket,
        sub: Subscribed,
        remote: RemoteView,
    }

    impl Watching {
        /// Returns what a screen replacing this one can go back to.
        fn previous(&self) -> Previous {
            Previous {
                ticket: self.ticket.clone(),
                name: self.title.clone(),
            }
        }
    }

    /// Returns the scan mode with a newly opened camera.
    fn scanning(
        ctx: &egui::Context,
        render_state: Option<&RenderState>,
        previous: Option<Previous>,
        skip: Option<Skip>,
        camera: Option<crate::source_spec::VideoSourceSpec>,
    ) -> Mode {
        Mode::Scanning(Box::new(Scanning {
            view: ScanView::new(ctx, render_state, skip, camera),
            previous,
        }))
    }

    /// Starts a dial for `ticket` and returns the state that waits on it.
    ///
    /// Dropping the returned [`Connecting`] aborts the dial. A dial that
    /// finishes after its receiver is gone closes the subscription it opened.
    fn connecting(
        ctx: &egui::Context,
        live: &Live,
        options: &Options,
        ticket: BroadcastTicket,
        previous: Option<Previous>,
    ) -> Connecting {
        info!(
            remote = %ticket.peer().fmt_short(),
            broadcast = %ticket.name(),
            "dialing"
        );
        let (tx, rx) = oneshot::channel();
        let live = live.clone();
        let options = options.clone();
        let dialing = ticket.clone();
        let ctx = ctx.clone();
        let task = spawn(async move {
            // Bounded here because the terminal can wait for Ctrl+C, but a
            // touchscreen would otherwise spin until someone taps Cancel.
            let dial = connect(&live, &dialing, &options, Some(DIAL_DEADLINE));
            let attempt = match dial.await {
                Ok((sub, player)) => Attempt::Connected(Box::new(Connected {
                    ticket: dialing,
                    sub,
                    player,
                })),
                Err(err) => Attempt::Failed(format!("{err:#}")),
            };
            if let Err(Attempt::Connected(connected)) = tx.send(attempt) {
                info!("the connection landed after it was cancelled, closing it");
                connected.discard();
            }
            ctx.request_repaint();
        });
        Connecting {
            ticket,
            previous,
            rx,
            task: AbortOnDropHandle::new(task),
        }
    }

    /// Returns the playing mode for `connected`, with any pinned rendition.
    fn watching(
        ctx: &egui::Context,
        connected: Connected,
        options: &Options,
        render_state: Option<&RenderState>,
    ) -> Mode {
        let Connected {
            ticket,
            sub,
            player,
        } = connected;
        let title = ticket.name().to_owned();
        let mut remote =
            RemoteView::new(ctx, "video", player, options.playback.decoder, render_state)
                .with_link(sub.subscription().clone());
        if let Some(name) = options.rendition.clone() {
            remote.set_rendition(RenditionMode::pinned(name));
        }
        info!(broadcast = %title, "playing");
        Mode::Watching(Box::new(Watching {
            title,
            ticket,
            sub,
            remote,
        }))
    }

    struct WatchApp {
        live: Live,
        options: Options,
        render_state: Option<RenderState>,
        mode: Mode,
        /// The outcome of the last dial, for the next screen without a picture.
        message: Option<String>,
        cursor: CursorIdle,
        /// The ticket the last dial failed on.
        refused: Option<Refused>,
        _heartbeat: AbortOnDropHandle<()>,
    }

    impl eframe::App for WatchApp {
        /// Drives the state machine.
        ///
        /// eframe skips `ui` while the window is minimized or hidden, but a
        /// dial that finishes then must still be picked up.
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));
            self.poll_scan(ctx);
            self.poll_connecting(ctx);
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            // Before the mode switch, so Escape leaves full screen on every screen.
            crate::ui::escape_leaves_fullscreen(&ctx);
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            match self.mode {
                Mode::Stopped(_) => self.stopped_ui(ui, &ctx),
                Mode::Scanning(_) => self.scan_ui(ui, &ctx),
                Mode::Connecting(_) => self.connecting_ui(ui, &ctx),
                Mode::Watching(_) => self.watch_ui(ui, &ctx),
            }
        }

        fn on_exit(&mut self) {
            info!("exit");
            self.close_mode();
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }

    impl WatchApp {
        /// Takes the ticket the camera read, if it has read one.
        fn poll_scan(&mut self, ctx: &egui::Context) {
            let Mode::Scanning(screen) = &self.mode else {
                return;
            };
            let Some(ticket) = screen.view.ticket() else {
                return;
            };
            let previous = screen.previous.clone();
            self.dial(ctx, ticket, previous);
        }

        /// Takes the outcome of a dial that finished since the last pass.
        fn poll_connecting(&mut self, ctx: &egui::Context) {
            let Mode::Connecting(pending) = &mut self.mode else {
                return;
            };
            let attempt = match pending.rx.try_recv() {
                Ok(attempt) => attempt,
                Err(oneshot::error::TryRecvError::Empty) => return,
                // Only happens while the runtime shuts down.
                Err(oneshot::error::TryRecvError::Closed) => {
                    Attempt::Failed("the connection attempt stopped".to_string())
                }
            };
            let previous = pending.previous.clone();
            let ticket = pending.ticket.clone();
            match attempt {
                Attempt::Connected(connected) => {
                    self.message = None;
                    self.refused = None;
                    self.mode =
                        watching(ctx, *connected, &self.options, self.render_state.as_ref());
                }
                Attempt::Failed(message) => {
                    let strikes = match self.refused.take() {
                        Some(refused) if refused.ticket == ticket => refused.strikes + 1,
                        _ => 0,
                    };
                    warn!(%message, strikes, "the subscription failed");
                    self.message = Some(message);
                    self.refused = Some(Refused { ticket, strikes });
                    self.enter_scan(ctx, previous);
                }
            }
        }

        /// Subscribes to `ticket`, replacing whatever is on screen.
        ///
        /// `previous` is the broadcast that played before this dial.
        fn dial(
            &mut self,
            ctx: &egui::Context,
            ticket: BroadcastTicket,
            previous: Option<Previous>,
        ) {
            self.close_mode();
            self.message = None;
            self.mode = Mode::Connecting(Box::new(connecting(
                ctx,
                &self.live,
                &self.options,
                ticket,
                previous,
            )));
        }

        /// Abandons the dial in flight and shows the stopped screen.
        ///
        /// A subscription the dial already opened is closed. The session to the
        /// peer stays cached in the transport, and the next dial to that peer
        /// reuses it.
        fn cancel_dial(&mut self, name: &str, previous: Option<Previous>) {
            info!(broadcast = %name, "the connection attempt was cancelled");
            self.close_mode();
            self.message = Some(format!("stopped connecting to {name}"));
            self.mode = Mode::Stopped(previous);
        }

        /// Closes whatever is on screen and opens the camera.
        ///
        /// The subscription closes because a small device cannot run a video
        /// decoder and the QR decoder at once. Going back dials `previous` again.
        fn enter_scan(&mut self, ctx: &egui::Context, previous: Option<Previous>) {
            let skip = self.refused.as_ref().map(|refused| {
                let wait = refused.wait();
                info!(?wait, "scanning, holding off the ticket that just failed");
                Skip {
                    ticket: refused.ticket.clone(),
                    until: Instant::now() + wait,
                }
            });
            if skip.is_none() {
                info!("scanning for a ticket");
            }
            self.close_mode();
            let camera = self.options.scan_camera.clone();
            self.mode = scanning(ctx, self.render_state.as_ref(), previous, skip, camera);
        }

        /// Closes what the current mode holds open.
        ///
        /// Leaves the mode in place. The caller replaces it or closes the window.
        fn close_mode(&mut self) {
            match &mut self.mode {
                Mode::Watching(watching) => {
                    watching.sub.close();
                }
                Mode::Connecting(pending) => {
                    // Closing before draining means a dial that has not answered
                    // yet fails its send and closes its own subscription. One
                    // that has answered is closed here.
                    pending.rx.close();
                    if let Ok(Attempt::Connected(connected)) = pending.rx.try_recv() {
                        connected.discard();
                    }
                    pending.task.abort();
                }
                Mode::Stopped(_) | Mode::Scanning(_) => {}
            }
        }

        /// Returns whether the stats overlay is expanded.
        ///
        /// An expanded overlay keeps the controls visible.
        fn overlay_expanded(&self) -> bool {
            match &self.mode {
                Mode::Watching(watching) => watching.remote.overlay_expanded(),
                Mode::Stopped(_) | Mode::Scanning(_) | Mode::Connecting(_) => false,
            }
        }

        /// Draws the screen shown after a cancelled dial.
        ///
        /// The camera does not reopen by itself. The QR code is usually still in
        /// front of it, and the scanner would dial it again within a third of a
        /// second.
        fn stopped_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let Mode::Stopped(previous) = &self.mode else {
                return;
            };
            let previous = previous.clone();
            let message = self.message.clone();

            let mut back = false;
            let mut rescan = false;
            let space = ui.available_height() / 3.0;
            ui.vertical_centered(|ui| {
                ui.add_space(space);
                if let Some(message) = &message {
                    ui.heading(message);
                }
                ui.add_space(16.0);
                if let Some(previous) = &previous {
                    back = ui
                        .add_sized(BUTTON, egui::Button::new(back_label(&previous.name)))
                        .clicked();
                    ui.add_space(8.0);
                }
                rescan = ui.add_sized(BUTTON, egui::Button::new("Scan")).clicked();
            });
            crate::ui::top_bar(ui, ctx, IDLE_TITLE);

            match (back, rescan, previous) {
                (true, _, Some(previous)) => {
                    self.dial(ctx, previous.ticket.clone(), Some(previous));
                }
                (_, true, previous) => self.enter_scan(ctx, previous),
                _ => {}
            }
        }

        /// Draws the scan screen with the last error and a button to go back.
        fn scan_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let mut previous = None;
            if let Mode::Scanning(screen) = &mut self.mode {
                screen.view.draw(ui);
                previous = screen.previous.clone();
            }
            crate::ui::top_bar(ui, ctx, IDLE_TITLE);

            let message = self.message.clone();
            if message.is_none() && previous.is_none() {
                return;
            }
            let mut back = false;
            crate::ui::control_panel(ctx, "scan-controls", |ui| {
                if let Some(previous) = &previous {
                    back = ui
                        .add_sized(BUTTON, egui::Button::new(back_label(&previous.name)))
                        .clicked();
                }
                if let Some(message) = &message {
                    ui.colored_label(egui::Color32::LIGHT_RED, message);
                }
            });
            if back && let Some(previous) = previous {
                self.dial(ctx, previous.ticket.clone(), Some(previous));
            }
        }

        /// Draws the connecting screen with a Cancel button.
        fn connecting_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let Mode::Connecting(pending) = &self.mode else {
                return;
            };
            let name = pending.ticket.name().to_owned();
            let previous = pending.previous.clone();

            let mut cancel = false;
            let space = ui.available_height() / 3.0;
            ui.vertical_centered(|ui| {
                ui.add_space(space);
                ui.heading(format!("connecting to {name} ..."));
                ui.add_space(8.0);
                ui.spinner();
                ui.add_space(16.0);
                cancel = ui.add_sized(BUTTON, egui::Button::new("Cancel")).clicked();
            });
            crate::ui::top_bar(ui, ctx, IDLE_TITLE);
            if cancel {
                self.cancel_dial(&name, previous);
            }
        }

        /// Draws the picture, and the overlay while the pointer moves.
        fn watch_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let show_overlay = self.cursor.update(ctx, self.overlay_expanded());
            let available = ui.available_size();
            let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);

            let Mode::Watching(watching) = &mut self.mode else {
                return;
            };
            watching.remote.draw(ui, available);
            if !show_overlay {
                return;
            }

            let title = watching.title.clone();
            crate::ui::top_bar(ui, ctx, &title);
            watching.remote.draw_overlay(ui, video_rect);

            let mut rescan = false;
            crate::ui::control_panel(ctx, "watch-controls", |ui| {
                watching.remote.controls(ui, "watch");
                rescan = ui
                    .button("Scan")
                    .on_hover_text("Read a new ticket off a QR code")
                    .clicked();
            });
            let previous = rescan.then(|| watching.previous());
            if let Some(previous) = previous {
                self.enter_scan(ctx, Some(previous));
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use iroh_live::BroadcastTicket;

        use super::{REDIAL_WAIT, REDIAL_WAIT_MAX, Refused, back_label};

        fn refused(strikes: u32) -> Refused {
            Refused {
                ticket: BroadcastTicket::new(iroh::SecretKey::generate().public(), "hello"),
                strikes,
            }
        }

        #[test]
        fn the_first_failure_waits_the_base_interval() {
            assert_eq!(refused(0).wait(), REDIAL_WAIT);
        }

        #[test]
        fn each_repeat_doubles_the_wait_up_to_the_ceiling() {
            assert_eq!(refused(1).wait(), REDIAL_WAIT * 2);
            assert_eq!(refused(2).wait(), REDIAL_WAIT * 4);
            assert_eq!(refused(4).wait(), REDIAL_WAIT_MAX);
        }

        /// Many failures in a row do not overflow the shift.
        #[test]
        fn a_ticket_that_never_connects_stays_at_the_ceiling() {
            assert_eq!(refused(u32::MAX).wait(), REDIAL_WAIT_MAX);
        }

        #[test]
        fn a_short_broadcast_name_is_on_the_button_whole() {
            assert_eq!(back_label("camera"), "Back to camera");
        }

        #[test]
        fn a_long_broadcast_name_is_cut_short() {
            let label = back_label("a-broadcast-with-a-name-nobody-should-have-chosen");
            assert_eq!(label, "Back to a-broadcast-with-a-name-...");
        }

        /// A cut by byte index would panic on this name.
        #[test]
        fn a_name_of_multi_byte_characters_is_cut_on_a_character() {
            let label = back_label(&"e\u{301}".repeat(40));
            assert!(label.starts_with("Back to "), "unexpected: {label}");
            assert!(label.ends_with("..."), "unexpected: {label}");
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Start, start};

    /// Parses a `watch` command line.
    fn watch_args(args: &[&str]) -> crate::args::WatchArgs {
        let line = ["irl", "watch"].into_iter().chain(args.iter().copied());
        let cli = crate::Cli::try_parse_from(line).expect("the flags are accepted");
        match cli.command {
            crate::Command::Watch(args) => *args,
            other => panic!("expected watch, got {other:?}"),
        }
    }

    /// Returns an endpoint id and a broadcast name.
    fn endpoint_and_name() -> (String, String) {
        (
            iroh::SecretKey::generate().public().to_string(),
            "hello".to_string(),
        )
    }

    #[test]
    fn a_ticket_on_its_own_is_dialed_before_the_window_opens() {
        let (id, name) = endpoint_and_name();
        let start = start(&watch_args(&["--endpoint-id", &id, "--name", &name]));
        assert!(matches!(start, Ok(Start::Ticket(_))));
    }

    #[test]
    fn a_ticket_alongside_scan_is_dialed_from_inside_the_window() {
        let (id, name) = endpoint_and_name();
        let start = start(&watch_args(&[
            "--scan",
            "--endpoint-id",
            &id,
            "--name",
            &name,
        ]));
        assert!(matches!(start, Ok(Start::TicketInWindow(_))));
    }

    #[test]
    fn scan_without_a_ticket_opens_the_camera() {
        assert!(matches!(start(&watch_args(&["--scan"])), Ok(Start::Scan)));
    }

    #[test]
    fn a_run_that_names_no_broadcast_is_rejected() {
        let err = start(&watch_args(&[])).expect_err("nothing names a broadcast");
        assert!(
            err.to_string().contains("--endpoint-id"),
            "unexpected: {err}"
        );
    }
}
