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
//! can still be pointed somewhere else. See [`crate::scan`].

use iroh_live::{Live, Subscription, media::subscribe::MediaTracks, ticket::LiveTicket};
use n0_error::{Result, anyerr};
#[cfg(feature = "playback")]
use tracing::info;
use tracing::warn;

use crate::{args::WatchArgs, transport};

/// Where this run's first ticket comes from.
#[derive(Debug)]
enum Start {
    /// The command line named one.
    Ticket(LiveTicket),
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
    tracks: TrackSelection,
    /// How the video is decoded.
    #[cfg(feature = "render")]
    playback: crate::args::PlaybackArgs,
}

impl From<&WatchArgs> for Options {
    fn from(args: &WatchArgs) -> Self {
        Self {
            rendition: args.rendition.clone(),
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

    let options = Options::from(&args);
    let live = rt.block_on(setup(&args))?;

    let ticket = match start {
        // Nothing to dial yet, so the window opens straight onto the camera.
        // eframe takes the main thread from here on, so the runtime keeps its
        // workers only for as long as this guard lives.
        #[cfg(feature = "render")]
        Start::Scan => {
            let _guard = rt.enter();
            return window::run(live, window::Opening::Scanning, options, args.fullscreen);
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
        let opening = window::Opening::Watching(Box::new(window::Connected { sub, tracks }));
        window::run(live, opening, options, args.fullscreen)
    }
    #[cfg(not(feature = "render"))]
    unreachable!("video was rejected above in a build without the render feature")
}

/// Decides where the first ticket comes from.
///
/// `--scan` alongside a ticket starts the window on that broadcast rather than
/// on the camera: the scan screen is one button away from the player, so
/// opening the camera first would only delay what the user already asked for.
///
/// # Errors
///
/// Fails if nothing names a broadcast: no ticket, no `--endpoint-id` and
/// `--name` pair, and no `--scan` to read one off a QR code.
fn start(args: &WatchArgs) -> Result<Start> {
    #[cfg(feature = "render")]
    if args.scan {
        return Ok(args.remote.ticket().map_or(Start::Scan, Start::Ticket));
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
    //! choose a rendition and set the volume, and the scan screen that points
    //! the window at a different broadcast.

    use std::time::Duration;

    use eframe::egui;
    use iroh_live::{Live, Subscription, media::subscribe::MediaTracks, ticket::LiveTicket};
    use moq_media_egui::egui_wgpu::RenderState;
    use n0_error::{Result, anyerr};
    use n0_future::task::{AbortOnDropHandle, spawn};
    use tokio::sync::oneshot;
    use tracing::{info, warn};

    use super::{Options, connect};
    use crate::{
        scan::ScanView,
        ui::{CursorIdle, RemoteView, RenditionChoice},
    };

    /// What the top bar says while no broadcast is playing.
    const SCAN_TITLE: &str = "irl watch: scan";

    /// How often the window is woken while nothing is drawing it.
    ///
    /// A dial started from the scan screen finishes whether or not the
    /// compositor thinks the window is visible, and the state machine has to
    /// run for the picture to follow. Ten passes a second makes that feel
    /// immediate and costs little enough to hold for the window's whole life.
    const HEARTBEAT: Duration = Duration::from_millis(100);

    /// What the window shows when it opens.
    pub(super) enum Opening {
        /// A subscription the command line's ticket already established.
        Watching(Box<Connected>),
        /// The scan screen, because `--scan` was given without a ticket.
        Scanning,
    }

    /// Opens the player window and runs it until it closes.
    pub(super) fn run(live: Live, opening: Opening, options: Options, fullscreen: bool) -> Result {
        eframe::run_native(
            "irl watch",
            crate::ui::native_options(fullscreen),
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);
                let render_state = cc.wgpu_render_state.clone();
                let mode = match opening {
                    Opening::Watching(connected) => {
                        watching(&cc.egui_ctx, *connected, &options, render_state.as_ref())
                    }
                    Opening::Scanning => scanning(&cc.egui_ctx, render_state.as_ref()),
                };
                Ok(Box::new(WatchApp {
                    live,
                    options,
                    render_state,
                    mode,
                    message: None,
                    cursor: CursorIdle::default(),
                    _heartbeat: crate::ui::spawn_heartbeat(&cc.egui_ctx, HEARTBEAT),
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    /// What the window is doing.
    enum Mode {
        /// Looking for a ticket in the camera picture.
        Scanning(Box<ScanView>),
        /// Dialing a ticket, with nothing to draw until it answers.
        Connecting(Box<Connecting>),
        /// Playing a broadcast.
        Watching(Box<Watching>),
    }

    /// A subscription attempt in flight.
    struct Connecting {
        /// What is being dialed, named on the connecting screen.
        ticket: LiveTicket,
        rx: oneshot::Receiver<Attempt>,
        _task: AbortOnDropHandle<()>,
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
        pub(super) sub: Subscription,
        pub(super) tracks: MediaTracks,
    }

    /// A broadcast on screen.
    struct Watching {
        /// The broadcast path, shown in the top bar.
        title: String,
        sub: Subscription,
        remote: RemoteView,
    }

    /// The scan mode, with the camera freshly opened.
    fn scanning(ctx: &egui::Context, render_state: Option<&RenderState>) -> Mode {
        Mode::Scanning(Box::new(ScanView::new(ctx, render_state)))
    }

    /// The playing mode for `connected`, honouring a pinned rendition.
    fn watching(
        ctx: &egui::Context,
        connected: Connected,
        options: &Options,
        render_state: Option<&RenderState>,
    ) -> Mode {
        let Connected { sub, tracks } = connected;
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
        Mode::Watching(Box::new(Watching { title, sub, remote }))
    }

    struct WatchApp {
        live: Live,
        options: Options,
        render_state: Option<RenderState>,
        mode: Mode,
        /// What became of the last attempt, shown on the scan screen.
        message: Option<String>,
        cursor: CursorIdle,
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
            let Mode::Scanning(view) = &self.mode else {
                return;
            };
            let Some(ticket) = view.ticket() else {
                return;
            };
            self.dial(ctx, ticket);
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
            match attempt {
                Attempt::Connected(connected) => {
                    self.message = None;
                    self.mode =
                        watching(ctx, *connected, &self.options, self.render_state.as_ref());
                }
                Attempt::Failed(message) => {
                    warn!(%message, "the subscription failed");
                    self.message = Some(message);
                    self.enter_scan(ctx);
                }
            }
        }

        /// Subscribes to `ticket`, replacing whatever is on screen.
        fn dial(&mut self, ctx: &egui::Context, ticket: LiveTicket) {
            info!(
                remote = %ticket.endpoint.id.fmt_short(),
                broadcast = %ticket.broadcast_name,
                "dialing a scanned ticket"
            );
            self.close_mode();

            let (tx, rx) = oneshot::channel();
            let live = self.live.clone();
            let options = self.options.clone();
            let dialing = ticket.clone();
            let ctx = ctx.clone();
            let task = spawn(async move {
                let attempt = match connect(&live, &dialing, &options).await {
                    Ok((sub, tracks)) => Attempt::Connected(Box::new(Connected { sub, tracks })),
                    Err(err) => Attempt::Failed(format!("{err:#}")),
                };
                let _ = tx.send(attempt);
                ctx.request_repaint();
            });
            self.mode = Mode::Connecting(Box::new(Connecting {
                ticket,
                rx,
                _task: AbortOnDropHandle::new(task),
            }));
        }

        /// Leaves whatever is playing and opens the camera.
        ///
        /// The subscription ends rather than staying warm in the background: a
        /// machine small enough to want a QR code instead of a keyboard has
        /// nothing left over for a video decoder while it searches frames, and
        /// coming back means dialing a ticket the camera is about to hand over
        /// anyway.
        fn enter_scan(&mut self, ctx: &egui::Context) {
            info!("scanning for a ticket");
            self.close_mode();
            self.mode = scanning(ctx, self.render_state.as_ref());
        }

        /// Ends whatever the current mode holds open.
        ///
        /// The caller either replaces the mode straight afterwards or is
        /// closing the window, so this leaves the old one in place.
        fn close_mode(&mut self) {
            if let Mode::Watching(watching) = &mut self.mode {
                watching.remote.shutdown();
                watching.sub.session().close(moq_net::Error::Cancel);
            }
        }

        /// Reports whether the stats overlay is expanded, which keeps the
        /// controls up while it is being read.
        fn overlay_expanded(&self) -> bool {
            match &self.mode {
                Mode::Watching(watching) => watching.remote.overlay_expanded(),
                Mode::Scanning(_) | Mode::Connecting(_) => false,
            }
        }

        /// Draws the scan screen: the camera picture, and whatever the last
        /// attempt had to say for itself.
        fn scan_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            if let Mode::Scanning(view) = &mut self.mode {
                view.draw(ui);
            }
            crate::ui::top_bar(ui, ctx, SCAN_TITLE);
            if let Some(message) = &self.message {
                crate::ui::control_panel(ctx, "scan-message", |ui| {
                    ui.colored_label(egui::Color32::LIGHT_RED, message);
                });
            }
        }

        /// Draws the waiting screen shown while a ticket is being dialed.
        fn connecting_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            let Mode::Connecting(pending) = &self.mode else {
                return;
            };
            let title = format!("connecting to {} ...", pending.ticket.broadcast_name);
            let space = ui.available_height() / 3.0;
            ui.vertical_centered(|ui| {
                ui.add_space(space);
                ui.heading(title);
                ui.add_space(8.0);
                ui.spinner();
            });
            crate::ui::top_bar(ui, ctx, SCAN_TITLE);
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
            if rescan {
                self.enter_scan(ctx);
            }
        }
    }
}
