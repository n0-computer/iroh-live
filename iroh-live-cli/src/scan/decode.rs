//! Finding a QR code in a camera picture.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use eframe::egui;
use iroh_live::{
    BroadcastTicket,
    media::video::{self, Surface},
};
use tokio::sync::watch;
use tracing::debug;

use super::{ScanState, Skip, report, still_skipped};

/// The QR decoder, on its own thread.
///
/// A decode of a 720p frame takes about 175 ms on a Raspberry Pi 4. On the
/// frame loop it would freeze the preview for that long.
///
/// Only one plane is in flight. The frame loop checks [`is_idle`] and skips
/// frames while the worker is busy, since a queued frame would show the same
/// code.
///
/// [`is_idle`]: Self::is_idle
pub(super) struct Decoder {
    /// Sends planes to the worker, at most one queued.
    pub(super) planes: std::sync::mpsc::SyncSender<Luma>,
    /// Receives the ticket, once.
    pub(super) found: tokio::sync::mpsc::Receiver<BroadcastTicket>,
    /// Whether the worker has finished the last plane.
    pub(super) idle: Arc<AtomicBool>,
}

impl Decoder {
    /// Starts the worker thread, which runs until the [`Decoder`] is dropped.
    pub(super) fn spawn(
        state: watch::Sender<ScanState>,
        ctx: egui::Context,
        skip: Option<Skip>,
    ) -> Self {
        let (planes, incoming) = std::sync::mpsc::sync_channel::<Luma>(1);
        let (report_found, found) = tokio::sync::mpsc::channel::<BroadcastTicket>(1);
        let idle = Arc::new(AtomicBool::new(true));
        let worker_idle = Arc::clone(&idle);
        std::thread::Builder::new()
            .name("qr-decode".into())
            .spawn(move || {
                while let Ok(luma) = incoming.recv() {
                    let decoding = Instant::now();
                    let found = decode(&luma);
                    // Logs every decode's cost, which limits the scan rate on
                    // small ARM boards.
                    debug!(
                        took_ms = decoding.elapsed().as_millis() as u64,
                        found = found.is_some(),
                        "looked for a QR code"
                    );
                    if let Some(text) = found {
                        match text.parse::<BroadcastTicket>() {
                            Ok(ticket) => {
                                if let Some(left) = still_skipped(skip.as_ref(), &ticket) {
                                    report(&state, &ctx, ScanState::Waiting(left));
                                } else if report_found.blocking_send(ticket).is_ok() {
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

    /// Returns whether the worker can take a plane.
    pub(super) fn is_idle(&self) -> bool {
        self.idle.load(Ordering::Acquire)
    }

    /// Hands a plane to the worker.
    ///
    /// Call only after [`is_idle`](Self::is_idle) returns true. A plane handed
    /// over while the queue is full is dropped.
    pub(super) fn look_at(&self, luma: Luma) {
        self.idle.store(false, Ordering::Release);
        if self.planes.try_send(luma).is_err() {
            self.idle.store(true, Ordering::Release);
        }
    }

    /// Waits for the ticket the worker finds.
    ///
    /// Stays pending forever once the worker has exited.
    pub(super) async fn found(&mut self) -> BroadcastTicket {
        match self.found.recv().await {
            Some(ticket) => ticket,
            None => std::future::pending().await,
        }
    }
}

/// A grayscale image with one byte per pixel and tightly packed rows.
pub(super) struct Luma {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) data: Vec<u8>,
}

/// Copies the luma plane out of `surface`.
///
/// A GPU surface, such as a macOS `CVPixelBuffer`, is downloaded first.
///
/// # Errors
///
/// Fails if a GPU surface cannot be downloaded.
pub(super) fn luma(surface: &Surface) -> Result<Luma, video::Error> {
    let i420 = surface.to_i420()?;
    Ok(Luma {
        width: i420.width(),
        height: i420.height(),
        data: i420.y().to_vec(),
    })
}

/// The sharpening radius as a fraction of the image's shorter side.
///
/// The radius should be about one module. A hand-held code fills a fifth to a
/// half of a 720p frame at 33 modules, so a module is 4 to 10 px. This gives
/// 2.4 px at 720p. Measured: it raises the readable blur from a third of a
/// module to over half, on both decoders.
pub(super) const SHARPEN_RADIUS_FRACTION: f64 = 1.0 / 300.0;

/// How much of the recovered detail the sharpening pass adds back.
///
/// Above about 2 the mask turns sensor noise into false modules. 1.5 is where
/// the blur harness peaks.
pub(super) const SHARPEN_AMOUNT: f64 = 1.5;

/// Reads the QR code in `image`, if it can.
///
/// A laptop webcam must get close to the Pi Zero's small e-paper panel, which
/// puts the code out of focus. The image is sharpened once, then `rxing` tries
/// first because it reads through about twice the blur `rqrr` does. `rqrr`
/// runs if `rxing` finds nothing, since the two fail on different images.
/// `blur_ceiling_report` measures both.
pub(super) fn decode(image: &Luma) -> Option<String> {
    let radius = f64::from(image.width.min(image.height)) * SHARPEN_RADIUS_FRACTION;
    let sharpened = sharpen(image, radius, SHARPEN_AMOUNT);
    decode_rxing(&sharpened).or_else(|| decode_rqrr(&sharpened))
}

/// Decodes with `rxing`, the zxing port.
///
/// `TryHarder` turns on a slower finder search that copes with soft edges.
pub(super) fn decode_rxing(image: &Luma) -> Option<String> {
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

/// Decodes with `rqrr`, a pure-Rust port of quirc.
pub(super) fn decode_rqrr(image: &Luma) -> Option<String> {
    let width = image.width as usize;
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, image.height as usize, |x, y| {
            image.data[y * width + x]
        });
    // A partly hidden code can detect as a grid and still fail to decode, so
    // take the first grid that decodes.
    prepared
        .detect_grids()
        .into_iter()
        .find_map(|grid| grid.decode().ok())
        .map(|(_meta, text)| text)
}

/// Applies an unsharp mask with a Gaussian of `sigma` pixels.
///
/// Adds `amount` times the difference between the image and its blur. This
/// partly undoes a defocus and also amplifies noise.
pub(super) fn sharpen(image: &Luma, sigma: f64, amount: f64) -> Luma {
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

/// Applies a separable Gaussian blur of `sigma` pixels.
///
/// The kernel spans three sigma each side, and edge pixels repeat past the
/// border.
pub(super) fn gaussian_blur(image: &Luma, sigma: f64) -> Luma {
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
                        if horizontal {
                            weight * at(source, x + offset, y)
                        } else {
                            weight * at(source, x, y + offset)
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
    use std::time::Duration;

    use iroh_live::{layout::LiveLayout, media::video::Size};

    use super::*;
    use crate::{scan::camera::SCAN_SIZE, ui::QrPixels};

    /// Pixels per QR module in the rendered test images.
    const MODULE_PIXELS: usize = 8;

    /// Renders `text` as a QR code, dark on light, with a quiet zone.
    fn render(text: &str) -> Luma {
        let pixels = QrPixels::render(text).expect("the text fits in a QR code");
        let side = pixels.side() * MODULE_PIXELS;
        let side_u32 = u32::try_from(side).expect("a QR code is at most 185 modules wide");
        Luma {
            width: side_u32,
            height: side_u32,
            data: pixels.place(MODULE_PIXELS, side, side),
        }
    }

    /// A decoder the harness compares.
    type Decoder = fn(&Luma) -> Option<String>;

    /// Simulates an out-of-focus lens.
    fn blur(image: &Luma, sigma: f64) -> Luma {
        gaussian_blur(image, sigma)
    }

    /// The frame size the scanner reads.
    ///
    /// The sharpen radius scales with it. A code-sized image would under-sharpen.
    const FRAME: Size = SCAN_SIZE;

    /// Returns the same ticket on every run.
    ///
    /// The QR pattern depends on the text, so a random key would make the blur
    /// tests flaky at the boundary.
    fn fixed_ticket() -> BroadcastTicket {
        let secret = iroh::SecretKey::from_bytes(&[7u8; 32]);
        LiveLayout::default()
            .ticket(secret.public(), "pi-zero")
            .expect("a valid broadcast name")
    }

    /// Renders a ticket centered in a white scan-sized frame.
    fn ticket_code(module_pixels: u32) -> (BroadcastTicket, Luma) {
        let ticket = fixed_ticket();
        let pixels = QrPixels::render(&ticket.to_string()).expect("a ticket fits in a QR code");
        let data = pixels.place(
            module_pixels as usize,
            FRAME.width as usize,
            FRAME.height as usize,
        );
        (
            ticket,
            Luma {
                width: FRAME.width,
                height: FRAME.height,
                data,
            },
        )
    }

    /// Returns the largest blur, in modules, that `decoder` reads through.
    ///
    /// Bisects to a tenth of a module, which assumes decoding is monotonic in
    /// blur at that precision.
    fn blur_ceiling_with(module_pixels: u32, decoder: Decoder) -> f64 {
        let (ticket, sharp) = ticket_code(module_pixels);
        let reads = |tenths: u32| {
            let sigma = f64::from(tenths) / 10.0 * f64::from(module_pixels);
            let image = blur(&sharp, sigma);
            decoder(&image).and_then(|text| text.parse::<BroadcastTicket>().ok())
                == Some(ticket.clone())
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
            if reads(mid) { low = mid } else { high = mid }
        }
        f64::from(low) / 10.0
    }

    /// A ticket decodes through a blur of half a module.
    ///
    /// That is what a webcam close to the Pi Zero's panel sees. The shipped
    /// pipeline measures 0.6 at every size in a release build, so 0.5 leaves one
    /// bisection step of margin.
    #[test]
    fn a_ticket_decodes_through_the_blur_a_close_webcam_produces() {
        const FLOOR_MODULES: f64 = 0.5;
        for module_pixels in [4, 6, 8, 10] {
            let (ticket, sharp) = ticket_code(module_pixels);
            let image = blur(&sharp, FLOOR_MODULES * f64::from(module_pixels));
            let read = decode(&image).and_then(|text| text.parse::<BroadcastTicket>().ok());
            assert_eq!(
                read.as_ref(),
                Some(&ticket),
                "at {module_pixels} px per module the scanner does not read through a blur \
                 of {FLOOR_MODULES} modules, which a close webcam produces",
            );
        }
    }

    /// Returns how long one decode of `image` takes.
    fn decode_cost(image: &Luma) -> Duration {
        let started = Instant::now();
        let _ = decode(image);
        started.elapsed()
    }

    /// Prints the blur ceiling per decoder and module size, and the decode cost.
    ///
    /// Run in release:
    /// `cargo test --release -p iroh-live-cli blur_ceiling_report -- --ignored --nocapture`.
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
        let ticket = LiveLayout::default()
            .ticket(iroh::SecretKey::generate().public(), "hello")
            .expect("a valid broadcast name");
        let text = decode(&render(&ticket.to_string())).expect("the code is there to be found");
        assert_eq!(
            text.parse::<BroadcastTicket>()
                .expect("it decoded as printed"),
            ticket
        );
    }

    #[test]
    fn a_qr_code_that_is_not_a_ticket_decodes_but_does_not_parse() {
        let text = decode(&render("https://example.com")).expect("the code is still a QR code");
        assert_eq!(text, "https://example.com");
        assert!(text.parse::<BroadcastTicket>().is_err());
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
