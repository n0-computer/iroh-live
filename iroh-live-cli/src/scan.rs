//! Reading a connection ticket off a QR code held up to the camera.
//!
//! `irl watch --scan` opens the camera instead of taking a ticket on the
//! command line: the window shows what the lens sees and connects as soon as a
//! frame carries a QR code that parses as a [`LiveTicket`]. It was built for a
//! Raspberry Pi with a touchscreen reading the code off another node's e-paper
//! display, where there is no keyboard to paste a ticket into.
//!
//! The camera runs on a thread of its own, the arrangement
//! `moq_media::local_task` exists for: a capture stream holds AVFoundation
//! objects on Apple platforms and cannot go to a work-stealing executor, and
//! the QR decoder is CPU-bound enough that a runtime worker is the wrong place
//! for it either way.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use eframe::egui;
#[cfg(all(target_os = "linux", feature = "rpicam"))]
use iroh_live::media::rpicam;
use iroh_live::{
    media::{
        frame_channel::{FrameReceiver, FrameSender, frame_channel},
        local_task::{self, LocalTask},
        video::{self, Frame, I420, Size, Surface, capture},
    },
    ticket::LiveTicket,
};
use moq_media_egui::FrameView;
use moq_net::Timestamp;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::source_spec::VideoSourceSpec;

/// How long the scanner waits between two looks for a QR code.
///
/// Finding a grid costs a full-frame binarization pass followed by a corner
/// search, which at 720p is on the order of tens of milliseconds on a
/// Raspberry Pi 4. Running that on every frame would take most of a core and
/// leave the preview stuttering, and it would buy nothing: somebody lining a
/// code up in front of a lens holds it there for a second or more, so three
/// looks a second finds it as fast as thirty would.
const DECODE_INTERVAL: Duration = Duration::from_millis(333);

/// Capture geometry the scanner asks the camera for.
///
/// Resolution matters more here than frame rate, because a QR code has to
/// survive binarization at whatever size it lands in the picture: 720p reads a
/// Pi Zero's e-paper display held at arm's length where 480p often does not.
/// The camera snaps to its nearest supported mode regardless.
const SCAN_SIZE: Size = Size {
    width: 1280,
    height: 720,
};

/// Capture frame rate the scanner asks the camera for.
///
/// The picture only has to look live to somebody lining a code up, and every
/// frame the camera produces costs a pixel format conversion on the capture
/// thread before anything else happens to it.
const SCAN_FRAMERATE: u32 = 15;

/// How long the scanner waits before reopening a camera that failed.
const REOPEN_DELAY: Duration = Duration::from_secs(2);

/// How many camera frames between one log line about the preview rate and the
/// next. A hundred is every seven seconds at the scan frame rate.
const PREVIEW_REPORT_EVERY: u64 = 100;

/// How long a camera has to produce its first picture before it counts as the
/// wrong camera.
///
/// Generous, because a webcam that has to power up and settle its exposure can
/// take a second or two, and reporting a working camera as broken is worse than
/// waiting. Short enough that somebody holding a code up to a Pi finds out what
/// is wrong rather than concluding the feature does not work.
const FIRST_FRAME_GRACE: Duration = Duration::from_secs(5);

/// A ticket the scanner keeps looking past, and until when.
///
/// A dial that fails sends the window back to the camera with the same code
/// still held up to it, which is the premise of the screen rather than an
/// accident. Reporting that code again straight away re-dials a peer that just
/// refused, and the caller can only answer by opening the camera again, so the
/// two of them spin: open, decode, dial, fail, close. Carrying the refusal into
/// the scan makes the wait happen behind a live preview instead, and a
/// different code held up during it still connects immediately.
#[derive(Debug, Clone)]
pub struct Skip {
    /// The ticket not to report.
    pub ticket: LiveTicket,
    /// When it may be reported again.
    pub until: Instant,
}

/// The camera the scanner reads, once a specifier has been resolved.
enum ScanCamera {
    Capture(capture::Stream),
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    Rpicam {
        frames: n0_future::boxed::BoxStream<Frame>,
        size: Size,
    },
}

impl ScanCamera {
    /// Opens whichever camera `choice` names, or the best guess when it names
    /// none.
    ///
    /// # Errors
    ///
    /// Returns a message for the screen if the camera will not open.
    async fn open(choice: Option<&VideoSourceSpec>) -> Result<Self, String> {
        match resolve(choice) {
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            VideoSourceSpec::Rpicam(_) => Self::open_rpicam(),
            VideoSourceSpec::Camera(id) => Self::open_capture(id).await,
            // `camera_spec` rejects everything else at the flag, so this is
            // unreachable rather than a case with a sensible answer.
            other => Err(format!("{other:?} is not a camera")),
        }
    }

    async fn open_capture(id: Option<String>) -> Result<Self, String> {
        let mut config = capture::Config::default();
        config.source = capture::Source::Camera(id);
        config.width = Some(SCAN_SIZE.width);
        config.height = Some(SCAN_SIZE.height);
        config.framerate = Some(SCAN_FRAMERATE);
        capture::open(&config)
            .await
            .map(Self::Capture)
            .map_err(|err| format!("the camera would not open: {err}"))
    }

    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    fn open_rpicam() -> Result<Self, String> {
        // The geometry is rounded to one libcamera leaves unpadded, and the
        // rounded figure is what the pictures arrive at, so it is what the QR
        // decoder has to be told about.
        let config = rpicam::RawConfig::new(SCAN_SIZE.width, SCAN_SIZE.height, SCAN_FRAMERATE);
        let size = Size {
            width: config.width(),
            height: config.height(),
        };
        // A clock of its own: nothing here lines the pictures up against audio,
        // and the timestamps are discarded where the frames are read.
        let frames = rpicam::frames(config, moq_mux::Clock::new())
            .map_err(|err| format!("the Raspberry Pi camera would not open: {err}"))?;
        Ok(Self::Rpicam { frames, size })
    }

    /// What to call this camera in a log line or an error.
    fn label(&self) -> String {
        match self {
            Self::Capture(stream) => stream.label().to_string(),
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam { .. } => "rpicam-vid".to_string(),
        }
    }

    fn size(&self) -> Size {
        match self {
            Self::Capture(stream) => Size {
                width: stream.width(),
                height: stream.height(),
            },
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam { size, .. } => *size,
        }
    }

    /// Whether this is the Raspberry Pi camera, which changes what a camera
    /// that sends nothing is likely to mean.
    fn is_rpicam(&self) -> bool {
        match self {
            Self::Capture(_) => false,
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam { .. } => true,
        }
    }

    /// The next picture, or `None` once the camera has stopped.
    ///
    /// # Errors
    ///
    /// Returns a message for the screen if the camera fails mid-stream.
    async fn read(&mut self) -> Result<Option<Surface>, String> {
        match self {
            Self::Capture(stream) => stream
                .read()
                .await
                .map_err(|err| format!("the camera failed: {err}")),
            #[cfg(all(target_os = "linux", feature = "rpicam"))]
            Self::Rpicam { frames, .. } => {
                use n0_future::StreamExt as _;
                // The timestamp goes: the preview draws whichever picture is in
                // the slot when the window next paints, with no presentation
                // clock in between.
                Ok(frames.next().await.map(|frame| frame.surface))
            }
        }
    }
}

/// Reads `--scan-camera`, which takes the grammar `--video` takes.
///
/// Restricted to the sources that can hand over pixels, which is what a QR
/// decoder needs: a camera by the id `irl devices` prints, or the Raspberry Pi
/// camera. `rpicam` means its raw pictures whether or not `:raw` is spelled
/// out, because the H.264 the camera app can produce instead is not something
/// this can read. A display, a file or a test pattern parse and are refused
/// here rather than opened and stared at.
///
/// `None` leaves the choice to [`resolve`].
///
/// # Errors
///
/// Returns a message naming the accepted forms.
pub fn camera_spec(spec: Option<&str>) -> Result<Option<VideoSourceSpec>, String> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let parsed = VideoSourceSpec::parse(spec)?;
    match parsed {
        VideoSourceSpec::Camera(id) => Ok(Some(VideoSourceSpec::Camera(id))),
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        VideoSourceSpec::Rpicam(_) => Ok(Some(VideoSourceSpec::Rpicam(
            crate::source_spec::RpicamMode::Raw,
        ))),
        _ => Err(format!(
            "--scan-camera reads a camera: 'cam' for the default one, 'cam:<id>' for one              `irl devices` lists, or 'rpicam' for the Raspberry Pi camera; got '{spec}'"
        )),
    }
}

/// The camera to open when `--scan-camera` named none.
///
/// A Raspberry Pi's `/dev/video0` is the Unicam node and hands back raw Bayer,
/// which is not a picture anything here can read. It does not fail honestly
/// either: the device accepts the requested geometry and then never delivers a
/// frame, so taking the default camera leaves a black preview and no error.
/// Where this build can drive `rpicam-vid`, that is the better guess.
///
/// A Pi with a USB webcam is the case this gets wrong, and `--scan-camera cam`
/// is the answer to it.
fn resolve(choice: Option<&VideoSourceSpec>) -> VideoSourceSpec {
    if let Some(spec) = choice {
        return spec.clone();
    }
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    if rpicam_on_path() {
        return VideoSourceSpec::Rpicam(crate::source_spec::RpicamMode::Raw);
    }
    VideoSourceSpec::Camera(None)
}

/// Whether `rpicam-vid` is installed.
///
/// Checked by looking for the binary rather than by enumerating:
/// `--list-cameras` probes the I2C buses the CSI connector sits on and takes
/// seconds, which is a long time to hold a screen somebody is pointing at a QR
/// code.
#[cfg(all(target_os = "linux", feature = "rpicam"))]
fn rpicam_on_path() -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join("rpicam-vid").is_file())
    })
}

/// What the scan screen tells the user to do.
const PROMPT: &str = "Hold a ticket QR code up to the camera";

/// How far the scanner has got.
#[derive(Debug, Clone)]
enum ScanState {
    /// The camera is open and no QR code has decoded yet.
    Looking,
    /// A QR code decoded but is not an iroh-live ticket. Worth saying out
    /// loud: pointing the camera at the wrong code otherwise looks exactly
    /// like pointing it at nothing.
    NotATicket,
    /// A ticket decoded. The camera has been released.
    Found(Box<LiveTicket>),
    /// The code in front of the camera is the one whose dial just failed, and
    /// the wait before offering it again has not run out. Carries what is left
    /// of that wait, so the screen can count it down.
    Waiting(Duration),
    /// The camera could not be opened, or it stopped.
    Failed(String),
}

/// The scan screen: a live camera picture and the ticket read out of it.
///
/// Dropping it cancels the capture thread, which releases the camera. Create
/// one when the window enters scan mode and drop it when the window leaves,
/// rather than holding an idle camera open behind a player.
pub struct ScanView {
    view: FrameView,
    frames: FrameReceiver<Frame>,
    state: watch::Receiver<ScanState>,
    _capture: LocalTask,
}

impl fmt::Debug for ScanView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanView")
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl ScanView {
    /// Opens the camera and starts looking for a ticket.
    ///
    /// Returns immediately: a camera that will not open is reported on the
    /// screen rather than here, because by then the window is already up and an
    /// error it can show is more use than one it cannot.
    pub fn new(
        ctx: &egui::Context,
        render_state: Option<&moq_media_egui::egui_wgpu::RenderState>,
        skip: Option<Skip>,
        camera: Option<VideoSourceSpec>,
    ) -> Self {
        let (frame_tx, frames) = frame_channel::<Frame>();
        let (state_tx, state) = watch::channel(ScanState::Looking);
        let ctx = ctx.clone();
        let view = FrameView::new_wgpu(&ctx, "scan", render_state);
        let capture = local_task::spawn("qr-scan", move |shutdown| async move {
            tokio::select! {
                () = scan(&frame_tx, &state_tx, &ctx, skip.as_ref(), camera.as_ref()) => {}
                () = shutdown.cancelled() => info!("scan cancelled"),
            }
        });
        Self {
            view,
            frames,
            state,
            _capture: capture,
        }
    }

    /// Returns the ticket, once the camera has read one it is willing to
    /// report.
    pub fn ticket(&self) -> Option<LiveTicket> {
        match &*self.state.borrow() {
            ScanState::Found(ticket) => Some((**ticket).clone()),
            _ => None,
        }
    }

    /// Draws the camera picture filling `ui`, with the instruction over it.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(frame) = self.frames.take() {
            self.view.render_frame(&frame);
        }

        let available = ui.available_size();
        let image = self.view.image();
        ui.centered_and_justified(|ui| ui.add_sized(available, image));
        self.banner(&ctx);
    }

    /// Draws the instruction, and whatever the scanner has to report, over the
    /// bottom of the picture.
    fn banner(&self, ctx: &egui::Context) {
        let note = match &*self.state.borrow() {
            ScanState::Looking => None,
            ScanState::NotATicket => Some("that QR code is not an iroh-live ticket".to_string()),
            ScanState::Found(ticket) => Some(format!("connecting to {}", ticket.broadcast_name)),
            ScanState::Waiting(left) => Some(format!(
                "that ticket did not connect, trying it again in {}s",
                left.as_secs() + 1
            )),
            ScanState::Failed(err) => Some(err.clone()),
        };

        egui::Area::new(egui::Id::new("scan-banner"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -32.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 190))
                    .corner_radius(6.0)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new(PROMPT)
                                    .size(20.0)
                                    .color(egui::Color32::WHITE),
                            );
                            if let Some(note) = note {
                                ui.label(
                                    egui::RichText::new(note).color(egui::Color32::LIGHT_YELLOW),
                                );
                            }
                        });
                    });
            });
    }
}

/// Reads the camera until it yields a ticket, reopening it whenever it fails.
///
/// The scan screen has no way forward other than a working camera, so a device
/// that is momentarily busy (another process letting go of it, a USB camera
/// settling after a replug) is worth waiting for rather than reporting once and
/// giving up.
async fn scan(
    frames: &FrameSender<Frame>,
    state: &watch::Sender<ScanState>,
    ctx: &egui::Context,
    skip: Option<&Skip>,
    camera: Option<&VideoSourceSpec>,
) {
    let mut said_so = false;
    loop {
        let problem = match look(frames, state, ctx, skip, camera).await {
            Ok(ticket) => {
                info!(
                    remote = %ticket.endpoint.id.fmt_short(),
                    broadcast = %ticket.broadcast_name,
                    "ticket scanned"
                );
                report(state, ctx, ScanState::Found(Box::new(ticket)));
                return;
            }
            Err(problem) => problem,
        };

        // One camera that is not there would otherwise say so every couple of
        // seconds and bury everything else in the log. The screen keeps showing
        // it regardless of which level this went out at.
        match said_so {
            false => warn!(%problem, "the scan camera is unusable"),
            true => debug!(%problem, "the scan camera is still unusable"),
        }
        said_so = true;
        report(state, ctx, ScanState::Failed(problem));
        tokio::time::sleep(REOPEN_DELAY).await;
    }
}

/// Opens the camera and reads it, forwarding every frame for drawing and
/// decoding some of them, until one carries a ticket.
///
/// # Errors
///
/// Returns a message for the screen if the camera will not open, or if it stops
/// producing frames.
async fn look(
    frames: &FrameSender<Frame>,
    state: &watch::Sender<ScanState>,
    ctx: &egui::Context,
    skip: Option<&Skip>,
    camera: Option<&VideoSourceSpec>,
) -> Result<LiveTicket, String> {
    let mut stream = ScanCamera::open(camera).await?;
    let size = stream.size();
    info!(
        device = stream.label(),
        width = size.width,
        height = size.height,
        "scanning for a ticket QR code"
    );
    report(state, ctx, ScanState::Looking);

    let opened = Instant::now();
    let mut seen_a_frame = false;
    let mut delivered = 0u64;
    let mut next_look = Instant::now();
    let mut decoder = Decoder::spawn(state.clone(), ctx.clone(), skip.cloned());
    loop {
        // A camera that opens and then says nothing is the failure a Raspberry
        // Pi produces when pointed at `/dev/video0`, and it is silent: the
        // device accepts the geometry and never delivers. Without this the
        // screen is black with no explanation, which is indistinguishable from
        // a lens cap.
        let read = tokio::select! {
            read = tokio::time::timeout(FIRST_FRAME_GRACE, stream.read()) => read,
            found = decoder.found() => {
                // Returning here drops the stream, so the camera is released
                // while the window dials rather than held open behind it.
                return Ok(found);
            }
        };
        let surface = match read {
            Ok(Ok(Some(surface))) => surface,
            Ok(Ok(None)) => return Err("the camera stopped".to_string()),
            Ok(Err(err)) => return Err(err),
            Err(_) if !seen_a_frame => return Err(no_frames(&stream)),
            // Already delivering, so a gap is the camera stalling rather than
            // the wrong device: say so and let the reopen loop have it.
            Err(_) => {
                return Err(format!(
                    "the camera stopped delivering after {}s",
                    opened.elapsed().as_secs()
                ));
            }
        };
        seen_a_frame = true;
        delivered += 1;
        if delivered.is_multiple_of(PREVIEW_REPORT_EVERY) {
            // A rate for the preview, read off the log rather than the eye.
            // The decode used to run on this thread and froze the picture for
            // its duration; this is what says whether that is still so.
            debug!(
                frames = delivered,
                fps = format_args!("{:.1}", delivered as f64 / opened.elapsed().as_secs_f64()),
                "scan camera delivering"
            );
        }

        // Taken before the frame is handed over, because handing it over gives
        // up ownership and reading the pixels needs it. Only when the decoder
        // is free: a look it cannot take yet is a look at a frame it will never
        // see anyway, so the interval restarts from the hand-over rather than
        // from a wish.
        let (surface, luma) = match Instant::now() >= next_look && decoder.is_idle() {
            false => (surface, None),
            true => match split_luma(surface) {
                Ok((surface, luma)) => (surface, Some(luma)),
                Err(err) => {
                    // The download consumed the surface, so there is nothing
                    // left to draw for this frame either. The interval restarts
                    // here as well: a download that fails once will fail again,
                    // and retrying it on every frame costs what decoding on
                    // every frame costs.
                    next_look = Instant::now() + DECODE_INTERVAL;
                    warn!(error = %err, "could not read the frame's luma plane");
                    continue;
                }
            },
        };

        // Drawn regardless of whether this frame is being decoded: the decode
        // runs on its own thread now, so the preview never waits on it. The
        // timestamp is zero because nothing reads it: the preview draws
        // whichever frame is in the slot when the window next paints, with no
        // presentation clock in between.
        frames.send(Frame::new(surface, Timestamp::ZERO));
        ctx.request_repaint();

        if let Some(luma) = luma {
            decoder.look_at(luma);
            // Measured from the hand-over. The decoder reports when it is idle
            // again, so a decode slower than the interval simply means the next
            // look waits for it rather than piling up behind it.
            next_look = Instant::now() + DECODE_INTERVAL;
        }
    }
}

/// The QR decoder, on a thread of its own.
///
/// Decoding a 720p frame costs about 175ms on a Raspberry Pi 4, and the
/// capture loop that would otherwise run it is also the loop that feeds the
/// preview. On the capture thread every look froze the picture for that long,
/// which at three looks a second is a preview that stutters for a third of the
/// time. Here the capture loop hands a plane over and carries on reading; the
/// decoder takes it, and says when it has finished.
///
/// One plane in flight at a time. A second look before the first has finished
/// is a look at a frame the decoder would reach late and read the same code out
/// of, so the capture loop skips it rather than queueing it: [`is_idle`] is
/// what it asks.
///
/// [`is_idle`]: Self::is_idle
struct Decoder {
    /// Planes go this way, at most one queued.
    planes: std::sync::mpsc::SyncSender<Luma>,
    /// The ticket comes back this way, once.
    found: tokio::sync::mpsc::Receiver<LiveTicket>,
    /// Whether the worker has taken and finished the last plane.
    idle: Arc<AtomicBool>,
}

impl Decoder {
    /// Starts the worker. It runs until the sender it is handed is dropped,
    /// which happens when the [`Decoder`] is.
    fn spawn(state: watch::Sender<ScanState>, ctx: egui::Context, skip: Option<Skip>) -> Self {
        let (planes, incoming) = std::sync::mpsc::sync_channel::<Luma>(1);
        let (report_found, found) = tokio::sync::mpsc::channel::<LiveTicket>(1);
        let idle = Arc::new(AtomicBool::new(true));
        let worker_idle = Arc::clone(&idle);
        std::thread::Builder::new()
            .name("qr-decode".into())
            .spawn(move || {
                while let Ok(luma) = incoming.recv() {
                    let decoding = Instant::now();
                    let found = decode(&luma);
                    // Every look, at debug: the decode is the one expensive
                    // thing here, and on a small ARM core its cost is what
                    // decides how often the scanner can look at all. A number
                    // in the log turns "scanning is slow on the Pi" into a
                    // figure to act on.
                    debug!(
                        took_ms = decoding.elapsed().as_millis() as u64,
                        found = found.is_some(),
                        "looked for a QR code"
                    );
                    if let Some(text) = found {
                        match text.parse::<LiveTicket>() {
                            Ok(ticket) => {
                                if let Some(left) = still_skipped(skip.as_ref(), &ticket) {
                                    report(&state, &ctx, ScanState::Waiting(left));
                                } else if report_found.blocking_send(ticket).is_ok() {
                                    // Found and delivered: nothing more to look for.
                                    return;
                                }
                            }
                            Err(err) => {
                                debug!(error = %err, bytes = text.len(), "the QR code is not a ticket");
                                report(&state, &ctx, ScanState::NotATicket);
                            }
                        }
                    }
                    worker_idle.store(true, Ordering::Release);
                }
            })
            .expect("a thread can be spawned");
        Self {
            planes,
            found,
            idle,
        }
    }

    /// Whether the worker is free to take a plane.
    fn is_idle(&self) -> bool {
        self.idle.load(Ordering::Acquire)
    }

    /// Hands a plane to the worker.
    ///
    /// Only meaningful right after [`is_idle`](Self::is_idle) said so; a plane
    /// handed over while the worker is busy is dropped, since the one it is on
    /// will be read the same.
    fn look_at(&self, luma: Luma) {
        self.idle.store(false, Ordering::Release);
        if self.planes.try_send(luma).is_err() {
            self.idle.store(true, Ordering::Release);
        }
    }

    /// Waits for the ticket the worker reads, if it ever reads one.
    ///
    /// Pending forever once the worker has gone, which only happens after it
    /// delivered a ticket or this [`Decoder`] was dropped.
    async fn found(&mut self) -> LiveTicket {
        match self.found.recv().await {
            Some(ticket) => ticket,
            None => std::future::pending().await,
        }
    }
}

/// Returns how much of `skip`'s wait is left, if `ticket` is the one it names
/// and the wait has not run out.
fn still_skipped(skip: Option<&Skip>, ticket: &LiveTicket) -> Option<Duration> {
    let skip = skip?;
    if skip.ticket != *ticket {
        return None;
    }
    skip.until.checked_duration_since(Instant::now())
}

/// What to say about a camera that opened and then produced nothing.
///
/// Names the flag, because the fix is a flag and the reader is looking at a
/// black rectangle.
fn no_frames(stream: &ScanCamera) -> String {
    let device = stream.label();
    match stream.is_rpicam() {
        // Already on the Pi camera, so the next question is the hardware.
        true => format!(
            "{device} opened but sent no pictures within {}s; check the ribbon cable",
            FIRST_FRAME_GRACE.as_secs()
        ),
        false => format!(
            "{device} opened but sent no pictures within {}s. A Raspberry Pi camera is not \
             reachable this way: pass --scan-camera rpicam",
            FIRST_FRAME_GRACE.as_secs()
        ),
    }
}

/// Publishes `next` and wakes the window so it is drawn.
fn report(state: &watch::Sender<ScanState>, ctx: &egui::Context, next: ScanState) {
    state.send_replace(next);
    ctx.request_repaint();
}

/// A grayscale image: one byte per pixel, rows tightly packed.
///
/// This is the Y plane of a captured frame, which is exactly what a QR decoder
/// wants. Nothing converts to RGB along the way, because a QR code carries no
/// color and the round trip would cost two passes over the pixels.
struct Luma {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// Copies the luma plane out of `surface` and hands the surface back for
/// drawing.
///
/// A Linux or Windows camera hands over CPU-resident I420, so the usual path
/// borrows the Y plane and copies it out. A macOS camera's `CVPixelBuffer`, and
/// any other GPU-resident surface, has to be downloaded first, and downloading
/// consumes the surface, so what comes back is rebuilt from the downloaded
/// planes.
///
/// # Errors
///
/// Fails if a GPU surface cannot be downloaded, in which case the frame is lost
/// along with it.
fn split_luma(surface: Surface) -> Result<(Surface, Luma), video::Error> {
    let width = surface.width();
    let height = surface.height();
    if let Surface::I420(i420) = &surface {
        let data = i420.y().to_vec();
        return Ok((
            surface,
            Luma {
                width,
                height,
                data,
            },
        ));
    }

    let planes = surface.into_i420()?;
    let luma = Luma {
        width,
        height,
        data: planes[..width as usize * height as usize].to_vec(),
    };
    Ok((
        Surface::I420(I420::new(width, height, planes.to_vec())?),
        luma,
    ))
}

/// Reads the first QR code in `image`, if it holds one.
///
/// How much of a blur the sharpening pass tries to undo, as a fraction of the
/// picture's shorter side.
///
/// The unsharp mask needs a radius, and the right one is about a module, which
/// depends on how big the code is in the frame. A code somebody is holding up
/// fills a fifth to a half of a 720p frame at 33 modules, so a module is four
/// to ten pixels; a 300th of the short side is 2.4 px at 720p, in the middle of
/// that. Measured: at this radius the mask lifts the blur a decoder reads
/// through from a third of a module to over half, on both decoders.
const SHARPEN_RADIUS_FRACTION: f64 = 1.0 / 300.0;

/// How much of the recovered detail the sharpening pass adds back.
///
/// Above about two the mask amplifies sensor noise into false modules faster
/// than it recovers real ones. One and a half is where the harness peaks.
const SHARPEN_AMOUNT: f64 = 1.5;

/// Reads the QR code out of a picture, if there is one it can read.
///
/// Two decoders, tried in the order that reads through the most blur. A
/// laptop webcam has to get close to a small panel to resolve its modules,
/// and close is out of focus; a scanner that only reads sharp pictures is a
/// scanner that does not read the Pi Zero's e-paper from a laptop, which is
/// what this was built for.
///
/// The picture is sharpened once with an unsharp mask, which partially undoes
/// a defocus at the cost of noise. `rxing`, the zxing port, is asked first: on
/// the blur harness it reads through about twice the defocus `rqrr` does.
/// `rqrr` runs on the same sharpened picture when `rxing` finds nothing,
/// because the two fail on different pictures and it is cheap. Measured
/// ceilings are in `blur_ceiling_report`.
fn decode(image: &Luma) -> Option<String> {
    let radius = f64::from(image.width.min(image.height)) * SHARPEN_RADIUS_FRACTION;
    let sharpened = sharpen(image, radius, SHARPEN_AMOUNT);
    decode_rxing(&sharpened).or_else(|| decode_rqrr(&sharpened))
}

/// The zxing port, told it is looking for a QR code and asked to try harder,
/// which turns on the slower finder-pattern search that copes with soft edges.
fn decode_rxing(image: &Luma) -> Option<String> {
    let mut hints = rxing::DecodeHints {
        TryHarder: Some(true),
        ..Default::default()
    };
    rxing::helpers::detect_in_luma_with_hints(
        image.data.clone(),
        image.width,
        image.height,
        Some(rxing::BarcodeFormat::QR_CODE),
        &mut hints,
    )
    .ok()
    .map(|result| result.getText().to_string())
}

/// `rqrr`, a pure-Rust port of quirc that takes a grayscale callback, so the
/// plane goes straight in with no format conversion.
fn decode_rqrr(image: &Luma) -> Option<String> {
    let width = image.width as usize;
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, image.height as usize, |x, y| {
            image.data[y * width + x]
        });
    // A picture can hold several codes, and a partly obscured one detects as a
    // grid and fails to decode; the first that decodes is the answer.
    prepared
        .detect_grids()
        .into_iter()
        .find_map(|grid| grid.decode().ok())
        .map(|(_meta, text)| text)
}

/// Unsharp mask: the picture plus `amount` times what a Gaussian blur of
/// `sigma` pixels took away from it.
///
/// Partially undoes a defocus. What it cannot undo is detail the blur removed
/// entirely, so it lifts the readable blur by a fraction rather than removing
/// the ceiling; and it amplifies noise, which is why `amount` is bounded.
fn sharpen(image: &Luma, sigma: f64, amount: f64) -> Luma {
    let soft = gaussian_blur(image, sigma);
    let data = image
        .data
        .iter()
        .zip(&soft.data)
        .map(|(&sharp, &soft)| {
            let value = f64::from(sharp) + amount * (f64::from(sharp) - f64::from(soft));
            value.round().clamp(0.0, 255.0) as u8
        })
        .collect();
    Luma {
        width: image.width,
        height: image.height,
        data,
    }
}

/// A separable Gaussian blur of `sigma` pixels, three sigma each side, with
/// the border pixel repeated past the edge.
///
/// Used by the sharpening pass, and by the tests to manufacture the defocus
/// the pass exists to undo.
fn gaussian_blur(image: &Luma, sigma: f64) -> Luma {
    if sigma <= 0.0 {
        return Luma {
            width: image.width,
            height: image.height,
            data: image.data.clone(),
        };
    }
    let radius = (sigma * 3.0).ceil() as i64;
    let kernel: Vec<f64> = (-radius..=radius)
        .map(|offset| (-(offset * offset) as f64 / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f64 = kernel.iter().sum();
    let (width, height) = (i64::from(image.width), i64::from(image.height));
    let at = |data: &[u8], x: i64, y: i64| -> f64 {
        let x = x.clamp(0, width - 1);
        let y = y.clamp(0, height - 1);
        f64::from(data[(y * width + x) as usize])
    };
    let pass = |source: &[u8], horizontal: bool| -> Vec<u8> {
        let mut out = vec![0u8; source.len()];
        for y in 0..height {
            for x in 0..width {
                let sum: f64 = kernel
                    .iter()
                    .enumerate()
                    .map(|(index, weight)| {
                        let offset = index as i64 - radius;
                        match horizontal {
                            true => weight * at(source, x + offset, y),
                            false => weight * at(source, x, y + offset),
                        }
                    })
                    .sum();
                out[(y * width + x) as usize] = (sum / total).round() as u8;
            }
        }
        out
    };
    let rows = pass(&image.data, true);
    Luma {
        width: image.width,
        height: image.height,
        data: pass(&rows, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pixels per QR module in the rendered test images.
    ///
    /// Roughly what a camera sees of a code filling a third of a 720p frame,
    /// and enough that `rqrr`'s binarization has clean edges to lock onto.
    const MODULE_PIXELS: u32 = 8;

    /// Quiet zone around the rendered code, in modules. Four is what the QR
    /// standard asks for, and a decoder is entitled to rely on it.
    const QUIET_MODULES: u32 = 4;

    /// Renders `text` as a QR code the way a camera would see one: dark
    /// modules on a light background, with the quiet zone around them.
    fn render(text: &str) -> Luma {
        let code = qrcode::QrCode::new(text).expect("the text fits in a QR code");
        let modules = u32::try_from(code.width()).expect("a QR code is at most 177 modules wide");
        let colors = code.to_colors();

        let side = (modules + 2 * QUIET_MODULES) * MODULE_PIXELS;
        let mut data = vec![u8::MAX; (side * side) as usize];
        for row in 0..modules {
            for column in 0..modules {
                if colors[(row * modules + column) as usize] != qrcode::Color::Dark {
                    continue;
                }
                let top = (row + QUIET_MODULES) * MODULE_PIXELS;
                let left = (column + QUIET_MODULES) * MODULE_PIXELS;
                for y in top..top + MODULE_PIXELS {
                    let start = (y * side + left) as usize;
                    data[start..start + MODULE_PIXELS as usize].fill(0);
                }
            }
        }

        Luma {
            width: side,
            height: side,
            data,
        }
    }

    /// A candidate decoder, as the harness compares them.
    type Decoder = fn(&Luma) -> Option<String>;

    /// The defocus a lens that is not on the code applies to it.
    fn blur(image: &Luma, sigma: f64) -> Luma {
        gaussian_blur(image, sigma)
    }

    /// The frame the scanner reads, which is the geometry the sharpen radius
    /// is scaled to. A harness on a code-sized image would under-sharpen and
    /// report a worse pipeline than the one that ships.
    const FRAME: Size = SCAN_SIZE;

    /// One ticket, the same every run. The QR bit pattern follows the text, so
    /// a random key would make the blur ceiling a lottery and the floor test
    /// below flaky on the boundary.
    fn fixed_ticket() -> LiveTicket {
        let secret = iroh::SecretKey::from_bytes(&[7u8; 32]);
        LiveTicket::new(secret.public(), "pi-zero")
    }

    /// A ticket as a webcam sees the Pi Zero's e-paper: the code drawn at
    /// `module_pixels` per module in the middle of a scan-sized frame, white
    /// around it. Three pixels per module is the panel itself; a webcam at arm's
    /// length sees each module across a handful of its own pixels.
    fn ticket_code(module_pixels: u32) -> (LiveTicket, Luma) {
        let ticket = fixed_ticket();
        let code = qrcode::QrCode::new(ticket.to_string()).expect("a ticket fits in a QR code");
        let modules = u32::try_from(code.width()).expect("at most 177 modules");
        let colors = code.to_colors();
        let side = modules * module_pixels;
        assert!(
            side + 2 * QUIET_MODULES * module_pixels <= FRAME.height,
            "{module_pixels} px per module does not fit the frame"
        );
        let left0 = (FRAME.width - side) / 2;
        let top0 = (FRAME.height - side) / 2;
        let mut data = vec![u8::MAX; (FRAME.width * FRAME.height) as usize];
        for row in 0..modules {
            for column in 0..modules {
                if colors[(row * modules + column) as usize] != qrcode::Color::Dark {
                    continue;
                }
                let top = top0 + row * module_pixels;
                let left = left0 + column * module_pixels;
                for y in top..top + module_pixels {
                    let start = (y * FRAME.width + left) as usize;
                    data[start..start + module_pixels as usize].fill(0);
                }
            }
        }
        (
            ticket,
            Luma {
                width: FRAME.width,
                height: FRAME.height,
                data,
            },
        )
    }

    /// The largest blur, in units of one module's width, at which `decoder`
    /// still reads a ticket rendered at `module_pixels` per module.
    ///
    /// Found by bisection to a tenth of a module. Decoding is monotonic enough
    /// in blur for that to hold: a picture the decoder reads at one sigma it
    /// reads at every smaller sigma, up to the pixel-phase noise a tenth of a
    /// module absorbs.
    fn blur_ceiling_with(module_pixels: u32, decoder: Decoder) -> f64 {
        let (ticket, sharp) = ticket_code(module_pixels);
        let reads = |tenths: u32| {
            let sigma = f64::from(tenths) / 10.0 * f64::from(module_pixels);
            let image = blur(&sharp, sigma);
            decoder(&image).and_then(|text| text.parse::<LiveTicket>().ok()) == Some(ticket.clone())
        };
        if !reads(0) {
            return 0.0;
        }
        // Invariant: `reads(low)` and `!reads(high)`.
        let (mut low, mut high) = (0u32, 30u32);
        if reads(high) {
            return f64::from(high) / 10.0;
        }
        while high - low > 1 {
            let mid = (low + high) / 2;
            match reads(mid) {
                true => low = mid,
                false => high = mid,
            }
        }
        f64::from(low) / 10.0
    }

    /// What Franz saw: a laptop webcam has to get close to the Pi Zero's panel,
    /// and close means out of focus. A blur whose sigma is half a module is
    /// what that looks like at the module sizes a webcam delivers, and the
    /// decoder has to read through it.
    ///
    /// One point per module size rather than a search: the floor is a check,
    /// and the search that finds the ceiling is `blur_ceiling_report`. Half a
    /// module is one bisection step under the 0.6 the shipped pipeline measures
    /// at every size in a release build; plain `rqrr` on the raw plane gave up
    /// at 0.2 to 0.3.
    #[test]
    fn a_ticket_decodes_through_the_blur_a_close_webcam_produces() {
        const FLOOR_MODULES: f64 = 0.5;
        for module_pixels in [4, 6, 8, 10] {
            let (ticket, sharp) = ticket_code(module_pixels);
            let image = blur(&sharp, FLOOR_MODULES * f64::from(module_pixels));
            let read = decode(&image).and_then(|text| text.parse::<LiveTicket>().ok());
            assert_eq!(
                read.as_ref(),
                Some(&ticket),
                "at {module_pixels} px per module the scanner does not read through a blur \
                 of {FLOOR_MODULES} modules, which a close webcam produces",
            );
        }
    }

    /// What one decode of a scan-sized frame costs, which is what the
    /// scanner pays every `DECODE_INTERVAL`. Meaningful in a release build:
    /// `cargo test --release -p iroh-live-cli blur_ceiling_report -- --ignored --nocapture`.
    fn decode_cost(image: &Luma) -> Duration {
        let started = Instant::now();
        let _ = decode(image);
        started.elapsed()
    }

    /// Prints the blur ceiling per module size and the cost of one decode, for
    /// tuning rather than for passing. Run with `--ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check; run by hand to see the ceiling"]
    fn blur_ceiling_report() {
        let candidates: [(&str, Decoder); 3] = [
            ("rqrr raw", decode_rqrr),
            ("rxing raw", decode_rxing),
            ("shipped", decode),
        ];
        println!("blur ceiling in modules, per decoder and module size:");
        print!("{:>14}", "px/module");
        for module_pixels in [3, 4, 5, 6, 8, 10] {
            print!("{module_pixels:>6}");
        }
        println!();
        for (name, decoder) in candidates {
            print!("{name:>14}");
            for module_pixels in [3, 4, 5, 6, 8, 10] {
                print!("{:>6.1}", blur_ceiling_with(module_pixels, decoder));
            }
            println!();
        }
        let (_, frame) = ticket_code(6);
        println!(
            "one decode of a {}x{} frame: {:?} (release build numbers are the real ones)",
            frame.width,
            frame.height,
            decode_cost(&frame)
        );
    }

    #[test]
    fn a_ticket_survives_the_round_trip_through_a_qr_code() {
        let ticket = LiveTicket::new(iroh::SecretKey::generate().public(), "hello");
        let text = decode(&render(&ticket.to_string())).expect("the code is there to be found");
        assert_eq!(
            text.parse::<LiveTicket>().expect("it decoded as printed"),
            ticket
        );
    }

    #[test]
    fn a_qr_code_that_is_not_a_ticket_decodes_but_does_not_parse() {
        let text = decode(&render("https://example.com")).expect("the code is still a QR code");
        assert_eq!(text, "https://example.com");
        assert!(text.parse::<LiveTicket>().is_err());
    }

    /// The flag takes the grammar `--video` takes, so a device id that works
    /// for publishing works for scanning, on every platform the same way.
    #[test]
    fn the_scan_camera_takes_a_camera_by_id() {
        assert_eq!(
            camera_spec(Some("cam")).expect("a camera"),
            Some(VideoSourceSpec::Camera(None))
        );
        assert_eq!(
            camera_spec(Some("cam:/dev/video2")).expect("a camera"),
            Some(VideoSourceSpec::Camera(Some("/dev/video2".into())))
        );
        assert_eq!(camera_spec(None).expect("no flag is fine"), None);
    }

    /// A scanner needs pixels, so a source that is not a camera is refused at
    /// the flag rather than opened and stared at.
    #[test]
    fn the_scan_camera_refuses_what_is_not_a_camera() {
        for spec in ["screen", "test", "file:clip.mp4", "none"] {
            let err = camera_spec(Some(spec)).expect_err(spec);
            assert!(err.contains("--scan-camera"), "{spec}: {err}");
            assert!(err.contains(spec), "{spec}: {err}");
        }
    }

    /// The Pi camera can only mean its raw pictures here: the H.264 it can
    /// produce instead is not something a QR decoder can read.
    #[cfg(all(target_os = "linux", feature = "rpicam"))]
    #[test]
    fn the_pi_camera_is_always_raw_for_scanning() {
        use crate::source_spec::RpicamMode;
        for spec in ["rpicam", "rpicam:raw", "picam"] {
            assert_eq!(
                camera_spec(Some(spec)).expect(spec),
                Some(VideoSourceSpec::Rpicam(RpicamMode::Raw)),
                "{spec}"
            );
        }
    }

    #[test]
    fn a_held_off_ticket_is_skipped_until_its_wait_runs_out() {
        let ticket = LiveTicket::new(iroh::SecretKey::generate().public(), "hello");
        let skip = Skip {
            ticket: ticket.clone(),
            until: Instant::now() + Duration::from_secs(60),
        };
        let left = still_skipped(Some(&skip), &ticket).expect("the wait has not run out");
        assert!(left <= Duration::from_secs(60));

        let expired = Skip {
            ticket: ticket.clone(),
            until: Instant::now() - Duration::from_secs(1),
        };
        assert!(still_skipped(Some(&expired), &ticket).is_none());
    }

    #[test]
    fn a_different_ticket_is_never_held_off() {
        let refused = LiveTicket::new(iroh::SecretKey::generate().public(), "hello");
        let other = LiveTicket::new(iroh::SecretKey::generate().public(), "hello");
        let skip = Skip {
            ticket: refused,
            until: Instant::now() + Duration::from_secs(60),
        };
        assert!(still_skipped(Some(&skip), &other).is_none());
        assert!(still_skipped(None, &other).is_none());
    }

    #[test]
    fn a_picture_with_no_qr_code_in_it_finds_nothing() {
        let blank = Luma {
            width: 320,
            height: 240,
            data: vec![u8::MAX; 320 * 240],
        };
        assert!(decode(&blank).is_none());
    }
}
