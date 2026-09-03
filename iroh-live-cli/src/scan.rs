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
    time::{Duration, Instant},
};

use eframe::egui;
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
    ) -> Self {
        let (frame_tx, frames) = frame_channel::<Frame>();
        let (state_tx, state) = watch::channel(ScanState::Looking);
        let ctx = ctx.clone();
        let view = FrameView::new_wgpu(&ctx, "scan", render_state);
        let capture = local_task::spawn("qr-scan", move |shutdown| async move {
            tokio::select! {
                () = scan(&frame_tx, &state_tx, &ctx) => {}
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

    /// Returns the ticket, once the camera has read one.
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

/// The capture configuration the scan camera opens with.
///
/// Always the default camera. `moq_video::capture` routes
/// [`capture::Source::Camera`] through V4L2 on Linux whichever features are
/// compiled in: its PipeWire backend serves the xdg-desktop-portal ScreenCast
/// interface, which only ever hands back a display, so the `pipewire` feature
/// changes nothing on this path. A Raspberry Pi camera has to reach V4L2 to be
/// scannable, which is what the `bcm2835-v4l2` shim and `libcamerify` are for.
fn camera_config() -> capture::Config {
    let mut config = capture::Config::default();
    config.source = capture::Source::Camera(None);
    config.width = Some(SCAN_SIZE.width);
    config.height = Some(SCAN_SIZE.height);
    config.framerate = Some(SCAN_FRAMERATE);
    config
}

/// Reads the camera until it yields a ticket, reopening it whenever it fails.
///
/// The scan screen has no way forward other than a working camera, so a device
/// that is momentarily busy (another process letting go of it, a USB camera
/// settling after a replug) is worth waiting for rather than reporting once and
/// giving up.
async fn scan(frames: &FrameSender<Frame>, state: &watch::Sender<ScanState>, ctx: &egui::Context) {
    let mut said_so = false;
    loop {
        let problem = match look(frames, state, ctx).await {
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
) -> Result<LiveTicket, String> {
    let config = camera_config();
    let mut stream = capture::open(&config)
        .await
        .map_err(|err| format!("the camera would not open: {err}"))?;
    info!(
        device = stream.label(),
        width = stream.width(),
        height = stream.height(),
        "scanning for a ticket QR code"
    );
    report(state, ctx, ScanState::Looking);

    let mut next_look = Instant::now();
    loop {
        let surface = match stream.read().await {
            Ok(Some(surface)) => surface,
            Ok(None) => return Err("the camera stopped".to_string()),
            Err(err) => return Err(format!("the camera failed: {err}")),
        };

        // Taken before the frame is handed over, because handing it over gives
        // up ownership and reading the pixels needs it.
        let (surface, luma) = match Instant::now() >= next_look {
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

        // Drawn before the decode runs. Both share this thread, and locating a
        // grid takes long enough on a small ARM core that holding the picture
        // back for it would show as a stutter.
        //
        // The timestamp is zero because nothing reads it: the preview draws
        // whichever frame is in the slot when the window next paints, with no
        // presentation clock in between.
        frames.send(Frame::new(surface, Timestamp::ZERO));
        ctx.request_repaint();

        let Some(luma) = luma else { continue };
        let found = decode(&luma);

        // Measured from here rather than from before the decode, so the
        // interval is a gap between decodes rather than a period each one has
        // to fit inside. A decode slower than the interval would otherwise
        // leave the next frame already past the deadline, and the duty cycle
        // would reach 100% on exactly the hardware slow enough to need the
        // throttle.
        next_look = Instant::now() + DECODE_INTERVAL;

        let Some(text) = found else { continue };
        match text.parse::<LiveTicket>() {
            // Returning here drops the stream, so the camera is released while
            // the window dials rather than held open behind it.
            Ok(ticket) => return Ok(ticket),
            Err(err) => {
                debug!(error = %err, bytes = text.len(), "the QR code is not a ticket");
                report(state, ctx, ScanState::NotATicket);
            }
        }
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
/// `rqrr` is the decoder: a pure-Rust port of quirc that takes a grayscale
/// callback, so the Y plane goes straight in with no format conversion and no
/// `image` dependency. The alternative, `bardecoder`, decodes an
/// `image::DynamicImage` and would pull that crate in for pixels we already
/// hold as bytes.
fn decode(image: &Luma) -> Option<String> {
    let width = image.width as usize;
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, image.height as usize, |x, y| {
            image.data[y * width + x]
        });
    // A picture can hold several codes, and a partly obscured one detects as a
    // grid that will not decode, so this takes the first that reads rather than
    // the first that was found.
    prepared
        .detect_grids()
        .into_iter()
        .find_map(|grid| grid.decode().ok())
        .map(|(_meta, text)| text)
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
