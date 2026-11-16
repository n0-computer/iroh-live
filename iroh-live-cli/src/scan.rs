//! Reads a [`BroadcastTicket`] off a QR code held up to the camera.
//!
//! Built for a Raspberry Pi with a touchscreen and no keyboard. The QR decoder
//! runs on its own thread because it is too slow for a runtime worker.

use std::time::{Duration, Instant};

use eframe::egui;
use iroh_live::{BroadcastTicket, media::VideoFrames};
use moq_ext_egui::FrameView;
use n0_future::task::AbortOnDropHandle;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::source_spec::VideoSourceSpec;

mod camera;
mod decode;

pub(crate) use self::camera::camera_spec;
use self::{
    camera::{Camera, CameraKind, REOPEN_DELAY},
    decode::{Decoder, luma},
};

/// How long the scanner waits between two decodes.
///
/// A decode takes about 175 ms on a Raspberry Pi 4 (see [`Decoder`]), and
/// someone holding up a code holds it for a second or more. Three decodes a
/// second find it as fast as thirty.
const DECODE_INTERVAL: Duration = Duration::from_millis(333);

/// How many frames pass between two preview rate log lines.
const PREVIEW_REPORT_EVERY: u64 = 100;

/// How long a camera may take to deliver a frame before it counts as broken.
///
/// A webcam can take a second or two to power up and settle its exposure.
const FIRST_FRAME_GRACE: Duration = Duration::from_secs(5);

/// A ticket the scanner ignores until a deadline.
///
/// After a failed dial the same code is usually still in front of the camera.
/// Skipping it lets the wait happen behind a live preview, and a different code
/// still connects at once.
#[derive(Debug, Clone)]
pub(crate) struct Skip {
    /// The ticket to ignore.
    pub(crate) ticket: BroadcastTicket,
    /// When the scanner may report it again.
    pub(crate) until: Instant,
}

/// The instruction on the scan screen.
const PROMPT: &str = "Hold a ticket QR code up to the camera";

/// The scanner's progress.
#[derive(Debug, Clone)]
enum ScanState {
    /// The camera is open and no QR code has decoded yet.
    Looking,
    /// A QR code decoded but is not a ticket.
    ///
    /// Shown so a wrong code does not look like no code.
    NotATicket,
    /// A ticket decoded and the camera is released.
    Found(Box<BroadcastTicket>),
    /// The skipped ticket is in view, with the time left on its wait.
    Waiting(Duration),
    /// The camera did not open, or it stopped.
    Failed(String),
}

/// Where the scanner's frames come from.
enum Pictures {
    /// A camera the scanner opens, and reopens if it fails.
    Open(Option<VideoSourceSpec>),
    /// The frames of a source opened elsewhere.
    Borrowed(VideoFrames),
}

/// The scan screen: a live camera picture and the ticket read from it.
///
/// Dropping it stops the scan and releases a camera it opened.
#[derive(Debug)]
pub(crate) struct ScanView {
    view: FrameView,
    /// The frames being drawn, once a camera has opened.
    frames: Option<VideoFrames>,
    /// The frames of each camera the scan opens.
    opened: watch::Receiver<Option<VideoFrames>>,
    state: watch::Receiver<ScanState>,
    _scan: AbortOnDropHandle<()>,
}

impl ScanView {
    /// Opens the camera and starts looking for a ticket.
    ///
    /// Returns at once. A camera that does not open is reported on the screen.
    pub(crate) fn new(
        ctx: &egui::Context,
        render_state: Option<&moq_ext_egui::egui_wgpu::RenderState>,
        skip: Option<Skip>,
        camera: Option<VideoSourceSpec>,
    ) -> Self {
        Self::start(ctx, render_state, skip, Pictures::Open(camera))
    }

    /// Starts looking for a ticket in `frames` from a camera opened elsewhere.
    ///
    /// Use this when the window already publishes the camera, since a device
    /// cannot be opened twice.
    pub(crate) fn from_frames(
        ctx: &egui::Context,
        render_state: Option<&moq_ext_egui::egui_wgpu::RenderState>,
        skip: Option<Skip>,
        frames: VideoFrames,
    ) -> Self {
        Self::start(ctx, render_state, skip, Pictures::Borrowed(frames))
    }

    /// Spawns the scan task over `pictures` and returns the handle that reads its state.
    fn start(
        ctx: &egui::Context,
        render_state: Option<&moq_ext_egui::egui_wgpu::RenderState>,
        skip: Option<Skip>,
        pictures: Pictures,
    ) -> Self {
        let (opened_tx, opened) = watch::channel(None);
        let (state_tx, state) = watch::channel(ScanState::Looking);
        let view = FrameView::new(ctx, "scan", render_state);
        let ctx = ctx.clone();
        let scan = AbortOnDropHandle::new(tokio::spawn(async move {
            scan(pictures, &opened_tx, &state_tx, &ctx, skip.as_ref()).await;
        }));
        Self {
            view,
            frames: None,
            opened,
            state,
            _scan: scan,
        }
    }

    /// Returns the ticket, once the scanner has found one.
    pub(crate) fn ticket(&self) -> Option<BroadcastTicket> {
        match &*self.state.borrow() {
            ScanState::Found(ticket) => Some((**ticket).clone()),
            _ => None,
        }
    }

    /// Draws the camera picture filling `ui`, with the instruction over it.
    pub(crate) fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.opened.has_changed().unwrap_or(false) {
            self.frames = self.opened.borrow_and_update().clone();
        }
        if let Some(frames) = self.frames.as_mut()
            && let Some(frame) = frames.try_next()
        {
            self.view.render_frame(&frame);
        }

        let available = ui.available_size();
        let image = self.view.image();
        ui.centered_and_justified(|ui| ui.add_sized(available, image));
        self.banner(&ctx);
    }

    /// Draws the instruction and the scanner's status over the picture.
    fn banner(&self, ctx: &egui::Context) {
        let note = match &*self.state.borrow() {
            ScanState::Looking => None,
            ScanState::NotATicket => Some("that QR code is not an iroh-live ticket".to_string()),
            ScanState::Found(ticket) => Some(format!("connecting to {}", ticket.path())),
            ScanState::Waiting(left) => Some(format!(
                "that ticket did not connect, trying it again in {}s",
                left.as_secs() + 1
            )),
            ScanState::Failed(err) => Some(err.clone()),
        };

        egui::Area::new(egui::Id::unique("scan-banner"))
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

/// Reads frames until they yield a ticket.
///
/// A camera the scanner opened is reopened whenever it fails, because a device
/// can be busy for a moment, for example after a USB replug. Borrowed frames
/// that end are reported as a failure.
async fn scan(
    pictures: Pictures,
    opened: &watch::Sender<Option<VideoFrames>>,
    state: &watch::Sender<ScanState>,
    ctx: &egui::Context,
    skip: Option<&Skip>,
) {
    let choice = match pictures {
        Pictures::Borrowed(frames) => {
            opened.send_replace(Some(frames.clone()));
            report(state, ctx, ScanState::Looking);
            match look(frames, "the camera", CameraKind::Capture, state, ctx, skip).await {
                Ok(ticket) => found(state, ctx, ticket),
                Err(problem) => {
                    warn!(%problem, "the scan camera is unusable");
                    report(state, ctx, ScanState::Failed(problem));
                }
            }
            return;
        }
        Pictures::Open(choice) => choice,
    };
    let mut said_so = false;
    loop {
        let problem = match Camera::open(choice.as_ref()).await {
            Ok(camera) => {
                let size = camera.source.format().size;
                info!(
                    device = camera.label,
                    width = size.width,
                    height = size.height,
                    "scanning for a ticket QR code"
                );
                opened.send_replace(Some(camera.source.frames()));
                report(state, ctx, ScanState::Looking);
                let frames = camera.source.frames();
                // Returning drops the camera, so the window dials without it.
                match look(frames, &camera.label, camera.kind, state, ctx, skip).await {
                    Ok(ticket) => return found(state, ctx, ticket),
                    Err(problem) => problem,
                }
            }
            Err(problem) => problem,
        };

        // Warn once. The screen shows every failure.
        if said_so {
            debug!(%problem, "the scan camera is still unusable");
        } else {
            warn!(%problem, "the scan camera is unusable");
        }
        said_so = true;
        report(state, ctx, ScanState::Failed(problem));
        tokio::time::sleep(REOPEN_DELAY).await;
    }
}

/// Reports a ticket the scan read.
fn found(state: &watch::Sender<ScanState>, ctx: &egui::Context, ticket: BroadcastTicket) {
    info!(
        peer = %ticket.peer().fmt_short(),
        broadcast = %ticket.path(),
        "ticket scanned"
    );
    report(state, ctx, ScanState::Found(Box::new(ticket)));
}

/// Reads `frames` until the decoder finds a ticket in one.
///
/// `label` and `kind` describe the camera for the error message.
///
/// # Errors
///
/// Returns a message for the screen if the camera stops delivering frames.
async fn look(
    mut frames: VideoFrames,
    label: &str,
    kind: CameraKind,
    state: &watch::Sender<ScanState>,
    ctx: &egui::Context,
    skip: Option<&Skip>,
) -> Result<BroadcastTicket, String> {
    let opened = Instant::now();
    let mut seen_a_frame = false;
    let mut delivered = 0u64;
    let mut next_look = Instant::now();
    let mut decoder = Decoder::spawn(state.clone(), ctx.clone(), skip.cloned());
    loop {
        // The timeout turns a silent camera, like a Pi's `/dev/video0`, into
        // an error instead of a black screen.
        let read = tokio::select! {
            read = tokio::time::timeout(FIRST_FRAME_GRACE, frames.next()) => read,
            found = decoder.found() => return Ok(found),
        };
        let frame = match read {
            Ok(Some(frame)) => frame,
            Ok(None) => return Err("the camera stopped".to_string()),
            Err(_) if !seen_a_frame => return Err(no_frames(label, kind)),
            // A stall after frames arrived, which the reopen loop handles.
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
            debug!(
                frames = delivered,
                fps = format_args!("{:.1}", delivered as f64 / opened.elapsed().as_secs_f64()),
                "scan camera delivering"
            );
        }
        // The view reads its own frames handle. This only wakes it.
        ctx.request_repaint();

        if Instant::now() >= next_look && decoder.is_idle() {
            match luma(&frame.surface) {
                Ok(luma) => decoder.look_at(luma),
                // The interval also applies after a failure, since a failed
                // download is likely to fail again.
                Err(err) => warn!(error = %err, "could not read the frame's luma plane"),
            }
            // Measured from the hand-over, so a slow decode delays the next one
            // instead of queueing frames.
            next_look = Instant::now() + DECODE_INTERVAL;
        }
    }
}

/// Returns the time left on `skip` if it names `ticket` and has not expired.
fn still_skipped(skip: Option<&Skip>, ticket: &BroadcastTicket) -> Option<Duration> {
    let skip = skip?;
    if skip.ticket != *ticket {
        return None;
    }
    skip.until.checked_duration_since(Instant::now())
}

/// Returns the error for a camera that opened and delivered no frames.
fn no_frames(device: &str, kind: CameraKind) -> String {
    match kind {
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        CameraKind::Rpicam => format!(
            "{device} opened but sent no pictures within {}s. Check the ribbon cable",
            FIRST_FRAME_GRACE.as_secs()
        ),
        CameraKind::Capture => format!(
            "{device} opened but sent no pictures within {}s. A Raspberry Pi camera is not \
             reachable this way: pass --scan-camera rpicam",
            FIRST_FRAME_GRACE.as_secs()
        ),
    }
}

/// Publishes `next` and wakes the window.
fn report(state: &watch::Sender<ScanState>, ctx: &egui::Context, next: ScanState) {
    state.send_replace(next);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use iroh_live::layout::LiveLayout;

    use super::*;

    #[test]
    fn a_held_off_ticket_is_skipped_until_its_wait_runs_out() {
        let ticket = LiveLayout::default()
            .ticket(iroh::SecretKey::generate().public(), "hello")
            .expect("a valid broadcast name");
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
        let refused = LiveLayout::default()
            .ticket(iroh::SecretKey::generate().public(), "hello")
            .expect("a valid broadcast name");
        let other = LiveLayout::default()
            .ticket(iroh::SecretKey::generate().public(), "hello")
            .expect("a valid broadcast name");
        let skip = Skip {
            ticket: refused,
            until: Instant::now() + Duration::from_secs(60),
        };
        assert!(still_skipped(Some(&skip), &other).is_none());
        assert!(still_skipped(None, &other).is_none());
    }
}
