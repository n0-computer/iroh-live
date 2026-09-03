//! `irl watch`: subscribe to a remote broadcast and play it.
//!
//! The window draws the decoded video and the playback engine takes the audio.
//! Unless `--rendition` pins one, the video track follows the downlink: the
//! subscription's transport signals drive `moq-media`'s adaptation, which swaps
//! renditions without the picture going blank.
//!
//! `--scan` starts that window on the camera rather than on a ticket, and
//! connects to whichever broadcast a QR code held up to the lens names. The
//! player keeps a button back to that screen, so a run started with a ticket
//! can still be pointed somewhere else, and every screen without a picture
//! keeps a button back to whatever was playing before it. See [`crate::scan`].

use iroh_live::{Live, Subscription, media::subscribe::MediaTracks, ticket::LiveTicket};
use n0_error::{Result, anyerr};
#[cfg(feature = "playback")]
use tracing::info;
use tracing::warn;

use crate::{args::WatchArgs, transport};

/// Where this run's first ticket comes from, and who dials it.
#[derive(Debug)]
enum Start {
    /// The command line named one and the terminal dials it, before any window
    /// opens.
    Ticket(LiveTicket),
    /// The command line named one and the window dials it, because `--scan`
    /// leaves a camera screen to cancel into.
    #[cfg(feature = "render")]
    TicketInWindow(LiveTicket),
    /// The camera will read one, because `--scan` was given without a ticket.
    #[cfg(feature = "render")]
    Scan,
}

/// Which tracks a subscription opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TrackSelection {
    /// Video and audio, which is what a window draws.
    #[default]
    Both,
    /// Audio alone, for `--no-video`. Opening the video track and discarding
    /// its frames would still cost a core to a decoder nobody draws from.
    AudioOnly,
}

/// The parts of [`WatchArgs`] a subscription needs, owned so the window can
/// carry them into a task that outlives any borrow of the flags.
#[derive(Debug, Clone, Default)]
struct Options {
    /// The rendition `--rendition` pinned, if any.
    rendition: Option<String>,
    /// Which camera the scan screen opens.
    #[cfg(feature = "render")]
    scan_camera: Option<crate::source_spec::VideoSourceSpec>,
    tracks: TrackSelection,
    /// How the video is decoded.
    #[cfg(feature = "render")]
    playback: crate::args::PlaybackArgs,
}

impl From<&WatchArgs> for Options {
    fn from(args: &WatchArgs) -> Self {
        Self {
            rendition: args.rendition.clone(),
            // Parsed by the caller, which can report a bad specifier; a
            // conversion that cannot fail has nowhere to put the message.
            #[cfg(feature = "render")]
            scan_camera: None,
            tracks: match args.no_video {
                true => TrackSelection::AudioOnly,
                false => TrackSelection::Both,
            },
            #[cfg(feature = "render")]
            playback: args.playback,
        }
    }
}

/// Runs the `watch` command.
pub fn run(args: WatchArgs, rt: &tokio::runtime::Runtime) -> Result {
    let start = start(&args)?;

    // Checked before dialing: a build that cannot draw should say so rather
    // than connect first and fail once the tracks are open.
    #[cfg(not(feature = "render"))]
    if !args.no_video {
        return Err(anyerr!(
            "watching video needs the 'render' feature, which this build was \
             compiled without; pass --no-video to play the audio alone"
        ));
    }

    #[cfg_attr(
        not(feature = "render"),
        expect(unused_mut, reason = "the scanner is gated")
    )]
    let mut options = Options::from(&args);
    #[cfg(feature = "render")]
    {
        options.scan_camera = crate::scan::camera_spec(args.scan_camera.as_deref())
            .map_err(|err| anyerr!("{err}"))?;
    }
    let live = rt.block_on(setup(&args))?;

    // Two of the three arms are gated on `render`, so a build without it leaves
    // one and the lint reads the match as pointless. It is not; it is the shape
    // that carries the other two.
    #[cfg_attr(
        not(feature = "render"),
        allow(
            clippy::infallible_destructuring_match,
            reason = "the other two arms are compiled out, not absent"
        )
    )]
    let ticket = match start {
        // Nothing to dial yet, so the window opens straight onto the camera.
        // eframe takes the main thread from here on, so the runtime keeps its
        // workers only for as long as this guard lives.
        #[cfg(feature = "render")]
        Start::Scan => {
            let _guard = rt.enter();
            return window::run(live, window::Opening::Scanning, options, args.fullscreen);
        }
        // Dialed from inside the window, so the wait shows a Cancel button
        // rather than a terminal line about Ctrl+C.
        #[cfg(feature = "render")]
        Start::TicketInWindow(ticket) => {
            let _guard = rt.enter();
            let opening = window::Opening::Connecting(Box::new(ticket));
            return window::run(live, opening, options, args.fullscreen);
        }
        Start::Ticket(ticket) => ticket,
    };

    let (live, (sub, tracks)) = rt.block_on(transport::with_live(live, async |live| {
        connect(live, &ticket, &options).await
    }))?;

    if args.no_video {
        return wait_for_ctrl_c(rt, live, sub, tracks);
    }

    #[cfg(feature = "render")]
    {
        let _guard = rt.enter();
        let connected = window::Connected {
            ticket,
            sub,
            tracks,
        };
        let opening = window::Opening::Watching(Box::new(connected));
        window::run(live, opening, options, args.fullscreen)
    }
    #[cfg(not(feature = "render"))]
    unreachable!("video was rejected above in a build without the render feature")
}

/// Decides where the first ticket comes from and who dials it.
///
/// `--scan` alongside a ticket starts the window on that broadcast rather than
/// on the camera: the scan screen is one button away from the player, so
/// opening the camera first would only delay what the user already asked for.
/// The dial moves into the window along with it, because a publisher that is
/// not running yet leaves the terminal waiting on a keyboard the machine
/// `--scan` was built for does not have.
///
/// # Errors
///
/// Fails if nothing names a broadcast: no ticket, no `--endpoint-id` and
/// `--name` pair, and no `--scan` to read one off a QR code.
fn start(args: &WatchArgs) -> Result<Start> {
    // clap rejects `--scan` alongside `--no-video`, so a window opens on both
    // of these paths and the dial has somewhere to be cancelled to.
    #[cfg(feature = "render")]
    if args.scan {
        return Ok(args
            .remote
            .ticket()
            .map_or(Start::Scan, Start::TicketInWindow));
    }

    let ticket = args
        .remote
        .ticket()
        .map_err(|err| match cfg!(feature = "render") {
            true => anyerr!("{err}, or pass --scan to read one off a QR code"),
            false => err,
        })?;
    Ok(Start::Ticket(ticket))
}

/// Opens the audio output and binds the endpoint every subscription runs on.
///
/// # Errors
///
/// Fails if `--audio-output` names a device that will not open, or if the
/// endpoint cannot bind.
async fn setup(args: &WatchArgs) -> Result<Live> {
    // Opening the engine before the first sink is what makes `--audio-output`
    // take effect: a sink built against the default device would already be
    // playing there by the time a switch could move it.
    #[cfg(feature = "playback")]
    if let Some(device) = args.audio_output.clone() {
        let mut config = iroh_live::media::audio::playback::Config::default();
        config.device = Some(device.clone());
        iroh_live::media::playback::open(config)
            .await
            .map_err(|err| {
                anyerr!(
                    "cannot open audio output '{device}': {err}. \
                     Run `irl devices` for the ids this machine accepts"
                )
            })?;
        info!(device = %device, "audio output selected");
    }
    #[cfg(not(feature = "playback"))]
    let _ = args;

    transport::setup_live(false).await
}

/// Connects to `ticket` and opens the tracks this run will actually play.
///
/// # Errors
///
/// Fails if the peer cannot be reached, or if the pinned rendition is not one
/// the broadcast offers.
async fn connect(
    live: &Live,
    ticket: &LiveTicket,
    options: &Options,
) -> Result<(Subscription, MediaTracks)> {
    let sub = transport::subscribe(live, ticket).await?;
    if let Some(name) = &options.rendition {
        check_rendition(&sub, name)?;
    }

    // Without a renderer nothing draws, so downloading is what a frame is for,
    // and there is no video decoder to choose either.
    #[cfg(feature = "render")]
    crate::ui::prepare_playback(sub.broadcast(), &options.playback);

    let tracks = match options.tracks {
        TrackSelection::AudioOnly => audio_only(&sub).await,
        TrackSelection::Both => sub.media().await,
    };
    Ok((sub, tracks))
}

/// Checks `--rendition` against what the broadcast actually offers.
///
/// A name nothing matches would otherwise pin the video track to a rendition
/// that never arrives, which looks exactly like a stalled link.
///
/// # Errors
///
/// Fails if the catalog has no video rendition of that name, listing the ones
/// it does have.
fn check_rendition(sub: &Subscription, name: &str) -> Result<()> {
    let catalog = sub.broadcast().catalog();
    if catalog.video().contains_key(name) {
        return Ok(());
    }
    let offered: Vec<&str> = catalog.video().keys().map(String::as_str).collect();
    Err(anyerr!(
        "the broadcast has no video rendition named '{name}'; it offers {}",
        match offered.is_empty() {
            true => "no video at all".to_string(),
            false => offered.join(", "),
        }
    ))
}

/// Opens the audio track alone, for `--no-video`.
// A build without `playback` has no sink to open, so nothing here awaits.
#[allow(
    clippy::unused_async,
    reason = "one arm of a feature-gated body awaits"
)]
async fn audio_only(sub: &Subscription) -> MediaTracks {
    #[cfg(feature = "playback")]
    {
        let broadcast = sub.broadcast();
        if !broadcast.has_audio() {
            warn!("the broadcast carries no audio, so --no-video plays nothing");
            return MediaTracks::default();
        }
        let audio = broadcast
            .audio()
            .await
            .inspect_err(|err| warn!(error = %err, "audio track failed to open"))
            .ok();
        MediaTracks { video: None, audio }
    }
    #[cfg(not(feature = "playback"))]
    {
        let _ = sub;
        warn!("this build has no playback support, so --no-video plays nothing");
        MediaTracks::default()
    }
}

/// Plays until the user interrupts, with no window.
fn wait_for_ctrl_c(
    rt: &tokio::runtime::Runtime,
    live: Live,
    sub: Subscription,
    tracks: MediaTracks,
) -> Result {
    println!("playing, press Ctrl+C to stop");
    rt.block_on(async move {
        tokio::signal::ctrl_c().await?;
        drop(tracks);
        sub.broadcast().shutdown();
        sub.session().close(moq_net::Error::Cancel);
        live.shutdown().await;
        Ok(())
    })
}

#[cfg(feature = "render")]
mod window {
    //! The player window: the picture, the stats overlay, the controls that
    //! choose a rendition and set the volume, the scan screen that points the
    //! window at a different broadcast, and the way back from every screen
    //! that has no picture on it.

    use std::time::{Duration, Instant};

    use eframe::egui;
    use iroh_live::{Live, Subscription, media::subscribe::MediaTracks, ticket::LiveTicket};
    use moq_media_egui::egui_wgpu::RenderState;
    use n0_error::{Result, anyerr};
    use n0_future::task::{AbortOnDropHandle, spawn};
    use tokio::sync::oneshot;
    use tracing::{info, warn};

    use super::{Options, connect};
    use crate::{
        scan::{ScanView, Skip},
        ui::{CursorIdle, RemoteView, RenditionChoice},
    };

    /// What the top bar says on the screens where nothing is playing.
    const IDLE_TITLE: &str = "irl watch";

    /// How often the window is woken while nothing is drawing it.
    ///
    /// A dial started from the scan screen finishes whether or not the
    /// compositor thinks the window is visible, and the state machine has to
    /// run for the picture to follow. Ten passes a second makes that feel
    /// immediate and costs little enough to hold for the window's whole life.
    const HEARTBEAT: Duration = Duration::from_millis(100);

    /// Size of the buttons on the screens with no picture behind them.
    ///
    /// Larger than an egui default because those buttons are the only way off
    /// those screens, and the machine this was built for is a small touchscreen
    /// with no pointer to aim precisely with.
    const BUTTON: egui::Vec2 = egui::vec2(200.0, 40.0);

    /// How much of a broadcast name a button label carries before it is cut.
    ///
    /// A name is whatever the publisher chose, and a long one would otherwise
    /// stretch its button across the screen.
    const LABEL_CHARS: usize = 24;

    /// What the window shows when it opens.
    pub(super) enum Opening {
        /// A subscription the command line's ticket already established.
        Watching(Box<Connected>),
        /// A dial of the command line's ticket, run from inside the window so
        /// that it can be given up on without a keyboard.
        Connecting(Box<LiveTicket>),
        /// The scan screen, because `--scan` was given without a ticket.
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
        /// Nothing on screen and nothing being attempted, which is where a
        /// cancelled dial lands.
        Stopped(Option<Previous>),
        /// Looking for a ticket in the camera picture.
        Scanning(Box<Scanning>),
        /// Dialing a ticket, with nothing to draw until it answers.
        Connecting(Box<Connecting>),
        /// Playing a broadcast.
        Watching(Box<Watching>),
    }

    /// The broadcast a screen with no picture on it can go back to.
    ///
    /// Carried out of the player and through the screens that replace it, so
    /// that a trip to the camera is not a one-way door. Going back means
    /// dialing the ticket again: see [`WatchApp::enter_scan`] for why the
    /// subscription does not stay open instead.
    #[derive(Debug, Clone)]
    struct Previous {
        ticket: LiveTicket,
        /// The broadcast's name as its catalog gave it, for the button label.
        name: String,
    }

    /// Returns the label for a button that goes back to the broadcast `name`.
    fn back_label(name: &str) -> String {
        match name.char_indices().nth(LABEL_CHARS) {
            Some((end, _)) => format!("Back to {}...", &name[..end]),
            None => format!("Back to {name}"),
        }
    }

    /// How long the scan screen keeps looking past a ticket whose dial just
    /// failed.
    ///
    /// Long enough that a peer which is simply not there costs one camera open
    /// rather than three a second, short enough that somebody still holding the
    /// code up while the other end finishes starting does not think the scanner
    /// has stopped looking.
    const REDIAL_WAIT: Duration = Duration::from_secs(3);

    /// The ceiling [`REDIAL_WAIT`] doubles up to.
    ///
    /// A ticket that has failed five times in a row is a peer that is not
    /// coming back, and by then the useful thing to do is stop burning the
    /// camera and let the user point it somewhere else.
    const REDIAL_WAIT_MAX: Duration = Duration::from_secs(30);

    /// A ticket whose dial failed, and how many times in a row.
    ///
    /// Held across the trip back to the camera, because the code is still in
    /// front of the lens: without this the scan reports it again within a frame
    /// or two and the window re-dials a peer that has just refused. Cleared by
    /// a dial that connects, so an outage that ends costs nothing afterwards.
    struct Refused {
        ticket: LiveTicket,
        /// Consecutive failures, counting from zero for the first.
        strikes: u32,
    }

    impl Refused {
        /// Returns how long the scan screen should keep looking past this
        /// ticket, doubling with each consecutive failure.
        fn wait(&self) -> Duration {
            REDIAL_WAIT
                .saturating_mul(1u32 << self.strikes.min(16))
                .min(REDIAL_WAIT_MAX)
        }
    }

    /// The scan screen, and whatever it can return to.
    struct Scanning {
        view: ScanView,
        previous: Option<Previous>,
    }

    /// A subscription attempt in flight.
    struct Connecting {
        /// What is being dialed, named on the connecting screen.
        ticket: LiveTicket,
        /// Carried through the attempt so that cancelling it still leads back
        /// to whatever was playing before.
        previous: Option<Previous>,
        rx: oneshot::Receiver<Attempt>,
        /// Aborting this is what abandons the dial.
        task: AbortOnDropHandle<()>,
    }

    /// What an attempt came back with.
    enum Attempt {
        /// The broadcast is open and its tracks are decoding.
        Connected(Box<Connected>),
        /// The attempt failed, with something to show on the scan screen.
        Failed(String),
    }

    /// A subscription whose tracks are already open.
    pub(super) struct Connected {
        /// What was dialed to reach it, kept so that the window can dial it
        /// again after a trip to the scan screen.
        pub(super) ticket: LiveTicket,
        pub(super) sub: Subscription,
        pub(super) tracks: MediaTracks,
    }

    impl Connected {
        /// Ends a subscription that nothing is going to draw.
        ///
        /// A dial that landed just as the user cancelled it owns a session and
        /// a set of decoders that no screen will ever hold, and dropping those
        /// leaves the peer to time the session out instead of being told.
        fn discard(self) {
            let Self { sub, tracks, .. } = self;
            drop(tracks);
            sub.broadcast().shutdown();
            sub.session().close(moq_net::Error::Cancel);
        }
    }

    /// A broadcast on screen.
    struct Watching {
        /// The broadcast path, shown in the top bar.
        title: String,
        /// What was dialed to reach it, for the way back from the screens that
        /// replace this one.
        ticket: LiveTicket,
        sub: Subscription,
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

    /// The scan mode, with the camera freshly opened.
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
    /// The task holds the only handle to what it builds until it answers, so
    /// dropping the returned [`Connecting`] both aborts the dial and takes the
    /// subscription with it. A dial that answers into a channel nobody is
    /// holding any more closes what it built rather than dropping it.
    fn connecting(
        ctx: &egui::Context,
        live: &Live,
        options: &Options,
        ticket: LiveTicket,
        previous: Option<Previous>,
    ) -> Connecting {
        info!(
            remote = %ticket.endpoint.id.fmt_short(),
            broadcast = %ticket.broadcast_name,
            "dialing"
        );
        let (tx, rx) = oneshot::channel();
        let live = live.clone();
        let options = options.clone();
        let dialing = ticket.clone();
        let ctx = ctx.clone();
        let task = spawn(async move {
            let attempt = match connect(&live, &dialing, &options).await {
                Ok((sub, tracks)) => Attempt::Connected(Box::new(Connected {
                    ticket: dialing,
                    sub,
                    tracks,
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

    /// The playing mode for `connected`, honouring a pinned rendition.
    fn watching(
        ctx: &egui::Context,
        connected: Connected,
        options: &Options,
        render_state: Option<&RenderState>,
    ) -> Mode {
        let Connected {
            ticket,
            sub,
            tracks,
        } = connected;
        let broadcast = sub.broadcast().clone();
        let title = broadcast.name().to_string();
        let mut remote = RemoteView::new(
            ctx,
            "video",
            broadcast,
            tracks,
            sub.signals().clone(),
            render_state,
        );
        if let Some(name) = options.rendition.clone() {
            remote.set_rendition(RenditionChoice::Pinned(name));
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
        /// What became of the last attempt, shown on whichever screen with no
        /// picture on it follows.
        message: Option<String>,
        cursor: CursorIdle,
        /// The ticket the last dial failed on, if the one before it did not
        /// connect.
        refused: Option<Refused>,
        /// Keeps the state machine ticking while nothing draws the window.
        _heartbeat: AbortOnDropHandle<()>,
    }

    impl eframe::App for WatchApp {
        /// Drives the state machine.
        ///
        /// Here rather than in [`ui`](Self::ui) because eframe runs no egui
        /// pass while the window is minimized or occluded, and a dial that
        /// finishes off screen still has to be picked up.
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(16));
            self.poll_scan(ctx);
            self.poll_connecting(ctx);
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            // Before the mode switch, so Escape leaves full screen from the
            // scan and connecting screens too, not only while watching.
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
                // The task went away without answering, which happens only as
                // the runtime shuts down.
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
        /// `previous` is what the connecting screen offers as a way back, which
        /// is the broadcast that was playing before this dial rather than the
        /// one being dialed.
        fn dial(&mut self, ctx: &egui::Context, ticket: LiveTicket, previous: Option<Previous>) {
            self.close_mode();
            // The message belongs to the attempt that just ended, and this one
            // has its own screen to report on.
            self.message = None;
            self.mode = Mode::Connecting(Box::new(connecting(
                ctx,
                &self.live,
                &self.options,
                ticket,
                previous,
            )));
        }

        /// Gives up on the dial in flight and leaves the window on the stopped
        /// screen.
        ///
        /// What is abandoned is our half: the task is aborted, so nothing here
        /// is still waiting for a catalog, and a subscription it managed to
        /// open in the meantime is closed rather than dropped. The session
        /// underneath is the transport's, which coalesces one per peer, so a
        /// dial the actor completed after we stopped listening stays cached
        /// there until the window closes, and a second attempt at the same peer
        /// picks it up rather than dialing again.
        fn cancel_dial(&mut self, name: &str, previous: Option<Previous>) {
            info!(broadcast = %name, "the connection attempt was cancelled");
            self.close_mode();
            self.message = Some(format!("stopped connecting to {name}"));
            self.mode = Mode::Stopped(previous);
        }

        /// Leaves whatever is on screen and opens the camera.
        ///
        /// The subscription ends rather than staying warm behind the camera: a
        /// machine small enough to want a QR code instead of a keyboard has
        /// nothing left over for a video decoder while it searches frames for a
        /// grid, and the picture it kept would be a scan's worth of minutes
        /// stale by the time anyone saw it again. Coming back therefore means
        /// dialing `previous` afresh, which is the spinner the connecting
        /// screen is for.
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

        /// Ends whatever the current mode holds open.
        ///
        /// The caller either replaces the mode straight afterwards or is
        /// closing the window, so this leaves the old one in place.
        fn close_mode(&mut self) {
            match &mut self.mode {
                Mode::Watching(watching) => {
                    watching.remote.shutdown();
                    watching.sub.session().close(moq_net::Error::Cancel);
                }
                Mode::Connecting(pending) => {
                    // Closing the channel before draining it means an attempt
                    // that has not answered yet fails its send and closes what
                    // it built, and one that has answered is closed here. The
                    // abort then stops whatever is still dialing.
                    pending.rx.close();
                    if let Ok(Attempt::Connected(connected)) = pending.rx.try_recv() {
                        connected.discard();
                    }
                    pending.task.abort();
                }
                Mode::Stopped(_) | Mode::Scanning(_) => {}
            }
        }

        /// Reports whether the stats overlay is expanded, which keeps the
        /// controls up while it is being read.
        fn overlay_expanded(&self) -> bool {
            match &self.mode {
                Mode::Watching(watching) => watching.remote.overlay_expanded(),
                Mode::Stopped(_) | Mode::Scanning(_) | Mode::Connecting(_) => false,
            }
        }

        /// Draws the screen a cancelled dial leaves behind: what stopped, and
        /// the ways on from it.
        ///
        /// The camera does not reopen on its own here. The QR code that started
        /// the cancelled dial is usually still in front of the lens, and a scan
        /// screen would read it again within a third of a second and dial
        /// straight back into what the user just stopped.
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

        /// Draws the scan screen: the camera picture, whatever the last attempt
        /// had to say for itself, and the way back to what was playing.
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

        /// Draws the waiting screen shown while a ticket is being dialed, with
        /// the button that gives up on it.
        fn connecting_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let Mode::Connecting(pending) = &self.mode else {
                return;
            };
            let name = pending.ticket.broadcast_name.clone();
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

        /// Draws the player: the picture, and the overlay while the pointer is
        /// moving.
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
        use iroh_live::ticket::LiveTicket;

        use super::{REDIAL_WAIT, REDIAL_WAIT_MAX, Refused, back_label};

        fn refused(strikes: u32) -> Refused {
            Refused {
                ticket: LiveTicket::new(iroh::SecretKey::generate().public(), "hello"),
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

        /// A peer that has been away all afternoon must not overflow the shift
        /// that computes its wait.
        #[test]
        fn a_ticket_that_never_connects_stays_at_the_ceiling() {
            assert_eq!(refused(u32::MAX).wait(), REDIAL_WAIT_MAX);
        }

        #[test]
        fn a_short_broadcast_name_is_on_the_button_whole() {
            assert_eq!(back_label("camera"), "Back to camera");
        }

        /// The name comes from the publisher, and the button it labels sits in
        /// a panel over the picture.
        #[test]
        fn a_long_broadcast_name_is_cut_short() {
            let label = back_label("a-broadcast-with-a-name-nobody-should-have-chosen");
            assert_eq!(label, "Back to a-broadcast-with-a-name-...");
        }

        /// Cutting by byte would panic here rather than shorten anything.
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

    /// Parses a `watch` command line, as `irl` itself would.
    fn watch_args(args: &[&str]) -> crate::args::WatchArgs {
        let line = ["irl", "watch"].into_iter().chain(args.iter().copied());
        let cli = crate::Cli::try_parse_from(line).expect("the flags are accepted");
        match cli.command {
            crate::Command::Watch(args) => args,
            other => panic!("expected watch, got {other:?}"),
        }
    }

    /// An endpoint id and broadcast name, which name a broadcast without a
    /// ticket to paste.
    fn endpoint_and_name() -> (String, String) {
        (
            iroh::SecretKey::generate().public().to_string(),
            "hello".to_string(),
        )
    }

    /// Nothing is on screen to cancel it, and the terminal it was typed into
    /// still has a Ctrl+C.
    #[test]
    fn a_ticket_on_its_own_is_dialed_before_the_window_opens() {
        let (id, name) = endpoint_and_name();
        let start = start(&watch_args(&["--endpoint-id", &id, "--name", &name]));
        assert!(matches!(start, Ok(Start::Ticket(_))));
    }

    /// The window dials this one itself, because its connecting screen has a
    /// camera to cancel back to.
    #[test]
    #[cfg(feature = "render")]
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
    #[cfg(feature = "render")]
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
