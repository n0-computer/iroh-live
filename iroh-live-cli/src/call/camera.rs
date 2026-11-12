//! Who holds the camera, and handing it back to the publisher after a scan.

use std::{task::Poll, time::Duration};

use eframe::egui;
use iroh_live::media::{LocalBroadcast, SlotState, VideoSource};
use n0_error::{Result, anyerr};
use tracing::{debug, info, warn};

use super::{screens::Screen, window::CallApp};
use crate::{args::CaptureArgs, scan::ScanView, source_spec::VideoSourceSpec as Spec, ui::Pending};

/// Who holds the camera while nothing is being scanned.
///
/// A capture device does not open twice. When the publisher already has the
/// camera, the scan screen reads its frames instead of opening it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Camera {
    /// The publisher has the camera, and the scan screen reads or borrows it.
    Publisher,
    /// Nothing here has the camera, so the scan screen opens it itself.
    Free,
}

impl Camera {
    /// Works out who holds the camera, from `--video` and `--scan-camera`.
    ///
    /// A scan camera on another device leaves the publisher alone.
    pub(super) fn of(args: &CaptureArgs, scan: Option<&Spec>) -> Self {
        let publishing = args.video_source();
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
pub(super) fn same_device(left: &Spec, right: &Spec) -> bool {
    match (left, right) {
        (Spec::Camera(left), Spec::Camera(right)) => {
            left.is_none() || right.is_none() || left == right
        }
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        (Spec::Rpicam(_), Spec::Rpicam(_)) => true,
        _ => false,
    }
}

/// How many times the camera is tried while the scan screen releases it.
pub(super) const RESTORE_ATTEMPTS: u32 = 5;

/// How long one attempt has to get the video running.
///
/// Setting a source only spawns it. `rpicam-vid` finds out the sensor is
/// busy only once it runs.
pub(super) const RESTORE_PATIENCE: Duration = Duration::from_secs(10);

/// The pause between two attempts.
pub(super) const RESTORE_DELAY: Duration = Duration::from_secs(1);

/// The outcome of one attempt to reopen the publisher's camera.
pub(super) enum Restored {
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
pub(super) async fn restore_once(
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

impl CallApp {
    /// Switches to the scan screen.
    ///
    /// A publisher holding the camera lends the scan screen its frames and
    /// keeps publishing. Otherwise the scan screen opens the camera itself.
    pub(super) fn enter_scan(&mut self, ctx: &egui::Context) {
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
                    self.scan_released.clone(),
                )
            }
            _ => ScanView::new(
                ctx,
                self.render_state.as_ref(),
                None,
                self.scan_camera.clone(),
                self.scan_released.clone(),
            ),
        };
        self.screen = Screen::Scanning(Box::new(view));
    }

    /// Closes the scan screen and shows the waiting screen.
    ///
    /// Dropping the view releases a camera it opened.
    /// [`Self::settle_camera`] gives the publisher its camera back.
    pub(super) fn leave_scan(&mut self) {
        if !matches!(self.screen, Screen::Scanning(_)) {
            return;
        }
        self.screen = Screen::waiting();
    }

    /// Gives the publisher its camera back once the scan screen is gone.
    pub(super) fn settle_camera(&mut self, ctx: &egui::Context) {
        if self.camera_owed && !matches!(self.screen, Screen::Scanning(_)) {
            self.camera_owed = false;
            self.restore_camera(ctx);
        }
    }

    /// Reopens the publisher's camera, retrying while the scan screen releases it.
    ///
    /// An attempt counts only once the video runs. A new `rpicam-vid` that
    /// finds the sensor busy exits, which shows as a failed video, not as
    /// an error from setting the source.
    pub(super) fn restore_camera(&mut self, ctx: &egui::Context) {
        let broadcast = self.broadcast.clone();
        let capture = self.capture.clone();
        let epoch = self.camera_epoch.clone();
        let mine = *epoch.lock().expect("poisoned");
        let restore = async move {
            let mut attempt = 0;
            loop {
                attempt += 1;
                let outcome = restore_once(&broadcast, &capture, &epoch, mine).await;
                match outcome {
                    Ok(Restored::Running(video)) => break Ok(video),
                    Ok(Restored::Superseded) => {
                        break Err(anyerr!("the camera restore was abandoned"));
                    }
                    Err(err) if attempt >= RESTORE_ATTEMPTS => break Err(err),
                    Err(err) => {
                        debug!(
                            error = format_args!("{err:#}"),
                            attempt, "the camera is not free yet"
                        );
                        tokio::time::sleep(RESTORE_DELAY).await;
                    }
                }
            }
        };
        self.restoring = Some(Pending::spawn(ctx, restore, drop));
    }

    /// Collects the result of reopening the publisher's camera.
    pub(super) fn poll_restore(&mut self) {
        let Some(restoring) = self.restoring.as_mut() else {
            return;
        };
        let Poll::Ready(result) = restoring.poll() else {
            return;
        };
        let result = result.unwrap_or_else(|| Err(anyerr!("the camera restore was abandoned")));
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
}

#[cfg(test)]
mod tests {
    use super::{Camera, CaptureArgs};

    /// Returns capture flags with the given `--video` specifier.
    fn capture(video: &str) -> CaptureArgs {
        CaptureArgs {
            video: video.parse().expect("a valid specifier"),
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
}
