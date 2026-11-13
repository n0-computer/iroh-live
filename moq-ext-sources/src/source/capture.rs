//! Camera, display and window capture, on a thread of its own.
//!
//! The device opens on a dedicated thread and never leaves it, because Apple
//! capture streams are not `Send`. Only its format and its frames cross.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{Driver, FIRST_FRAME_PATIENCE, VideoFormat};
use crate::{
    Error,
    frames::FrameSlot,
    local_task::{self, LocalTask},
    video,
};

/// How long an opened camera may take over its first frame.
///
/// A webcam streams within a second or two of opening. Until the first frame
/// arrives, a caller that closed its old source shows a still picture, so a
/// camera that will never stream should fail sooner than the general
/// [`FIRST_FRAME_PATIENCE`].
const CAMERA_FIRST_FRAME_PATIENCE: Duration = Duration::from_secs(10);

/// How long a camera open waits for a dropped camera to release its device.
///
/// A stopped capture lets go within a frame interval, and a V4L2 read that
/// waits for a frame gives up after a second. A capture that takes longer is
/// stuck in a read, and waiting longer would not help.
const RELEASE_PATIENCE: Duration = Duration::from_secs(3);

/// The camera captures of this process that were dropped but may still hold a device.
///
/// moq-video's capture stream closes its device when it drops, which happens
/// on the capture thread after the source is gone. Opening a camera that is
/// still held fails as busy on V4L2, so a camera open waits for these first.
static DROPPED: Releases = Releases::new();

/// The release tokens of dropped camera captures.
///
/// Every dropped camera is waited for, not only one of the same name: the
/// default camera, a device path and a PipeWire node can all name the same
/// device, and a release takes no more than a frame interval. Each entry is a
/// child of a capture thread's release token, so cancelling it forgets the
/// capture without telling anyone else that the device is free.
#[derive(Debug)]
struct Releases(Mutex<Vec<CancellationToken>>);

impl Releases {
    const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    /// Remembers a dropped camera capture until its thread has released the device.
    fn push(&self, released: &CancellationToken) {
        let mut pending = self.0.lock().expect("poisoned");
        pending.retain(|token| !token.is_cancelled());
        pending.push(released.child_token());
    }

    /// Waits at most `patience` for every dropped capture to release its device.
    ///
    /// A dropped capture that does not let go in time is stuck in a read: it
    /// is forgotten with a warning, so later opens do not wait for it again,
    /// and its thread may hold the device until the read returns.
    async fn settle(&self, patience: Duration) {
        let pending: Vec<CancellationToken> = {
            let mut pending = self.0.lock().expect("poisoned");
            pending.retain(|token| !token.is_cancelled());
            pending.clone()
        };
        if pending.is_empty() {
            return;
        }
        debug!(
            count = pending.len(),
            "waiting for a dropped camera to release its device"
        );
        let deadline = tokio::time::Instant::now() + patience;
        for token in pending {
            if tokio::time::timeout_at(deadline, token.cancelled())
                .await
                .is_err()
            {
                warn!(
                    ?patience,
                    "a dropped camera still holds its device, opening without it"
                );
                token.cancel();
            }
        }
    }
}

/// A camera capture's thread, which joins [`DROPPED`] once the source drops it.
#[derive(Debug)]
pub(super) struct Camera {
    task: LocalTask,
    dropped: &'static Releases,
}

impl Camera {
    /// Returns a token that is cancelled once the capture thread has closed the device.
    pub(super) fn released(&self) -> CancellationToken {
        self.task.released()
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        // Registered before the task's own drop stops the thread, so an open
        // that follows this drop always finds it.
        self.dropped.push(&self.task.released());
    }
}

/// Returns the rate assumed for a device that reports none.
fn default_rate() -> video::FrameRate {
    video::FrameRate::new(30, 1).expect("30/1 is a valid rate")
}

/// Returns the PipeWire source to try before `source`, if `source` is a default.
///
/// The default camera becomes PipeWire's default camera, and the default
/// display becomes a screen picked in the desktop portal. Sources that name a
/// device open exactly that device.
#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub(super) fn pipewire_default(source: &video::capture::Source) -> Option<video::capture::Source> {
    use video::capture::{
        Source,
        portal::{Kind, Selection},
    };
    match source {
        Source::Camera(None) => Some(Source::Camera(Some("pipewire".to_string()))),
        Source::Display(None) => Some(Source::Portal(Selection::new(Kind::Screen))),
        _ => None,
    }
}

/// Returns whether `source` should still open after its PipeWire default failed with `err`.
///
/// A camera always falls back to V4L2. A display falls back to X11 only on an
/// X11 session, and not when the user declined the portal's dialog: on
/// Wayland, X11 capture sees only XWayland's windows, and the main display is
/// mostly black.
#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub(super) fn falls_back(source: &video::capture::Source, err: &Error) -> bool {
    match source {
        video::capture::Source::Display(_) => !wayland_session() && !declined(err),
        _ => true,
    }
}

/// Returns the source to open after the PipeWire default of `source` failed.
///
/// moq-video opens PipeWire's camera for the default camera whenever PipeWire
/// lists one, so retrying the default would ask PipeWire again. The fallback
/// names the first V4L2 device, which is what the default means without
/// PipeWire.
#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub(super) fn fallback(source: video::capture::Source) -> video::capture::Source {
    use video::capture::Source;
    match source {
        Source::Camera(None) => Source::Camera(Some(V4L2_DEFAULT.to_string())),
        source => source,
    }
}

/// The V4L2 device moq-video opens for the default camera without PipeWire.
#[cfg(all(target_os = "linux", feature = "pipewire"))]
const V4L2_DEFAULT: &str = "/dev/video0";

/// Returns whether this process runs in a Wayland session.
///
/// Uses the test moq-video's X11 backend applies before it refuses a display.
#[cfg(all(target_os = "linux", feature = "pipewire"))]
fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Returns whether `err` is the user declining a permission dialog.
#[cfg(all(target_os = "linux", feature = "pipewire"))]
fn declined(err: &Error) -> bool {
    match err {
        Error::Device { source, .. } => matches!(
            source.downcast_ref::<video::Error>(),
            Some(video::Error::PermissionDenied(_))
        ),
        _ => false,
    }
}

/// Waits at most [`RELEASE_PATIENCE`] for the dropped cameras to release their devices.
pub(super) async fn settle() {
    DROPPED.settle(RELEASE_PATIENCE).await;
}

/// Opens the device `config` names and starts reading it into `slot`.
///
/// Returns once the first frame has arrived, with the format the device
/// opened at and the driver that owns it. A camera's driver joins [`DROPPED`]
/// when it drops, also when the open fails or is cancelled.
pub(super) async fn open(
    config: video::capture::Config,
    slot: FrameSlot,
    stop: CancellationToken,
) -> Result<(VideoFormat, Driver), Error> {
    let camera = matches!(config.source, video::capture::Source::Camera(_));
    let patience = if camera {
        CAMERA_FIRST_FRAME_PATIENCE
    } else {
        FIRST_FRAME_PATIENCE
    };
    let (opened_tx, opened) = oneshot::channel::<Result<VideoFormat, Error>>();
    let task = local_task::spawn("video-capture", stop.clone(), move |stop| async move {
        let opened = tokio::select! {
            opened = video::capture::open(&config) => opened,
            () = stop.cancelled() => return,
        };
        let mut stream = match opened {
            Ok(stream) => stream,
            Err(err) => {
                let _ = opened_tx.send(Err(Error::device(err)));
                return;
            }
        };
        let size = video::Size::new(stream.width(), stream.height());
        let rate = config
            .framerate
            .or_else(|| stream.framerate())
            .unwrap_or_else(default_rate);
        let format = VideoFormat { size, rate };

        // Only the first frame proves the device works. A Pi's Unicam node
        // opens and returns nothing, because only libcamera can drive its raw
        // Bayer, and would otherwise look like a slow camera.
        let first = tokio::select! {
            first = tokio::time::timeout(patience, stream.read()) => first,
            () = stop.cancelled() => return,
        };
        match first {
            Ok(Ok(Some(frame))) => slot.send(Arc::new(frame)),
            Ok(Ok(None)) => {
                let _ = opened_tx.send(Err(Error::device_msg(
                    "the capture device closed before its first frame",
                )));
                return;
            }
            Ok(Err(err)) => {
                let _ = opened_tx.send(Err(Error::device(err)));
                return;
            }
            Err(_) => {
                let _ = opened_tx.send(Err(Error::device_msg(format!(
                    "the capture device opened but produced no frame within {}s",
                    patience.as_secs()
                ))));
                return;
            }
        }
        info!(%size, %rate, "video capture opened");
        if opened_tx.send(Ok(format)).is_err() {
            return;
        }

        loop {
            let frame = tokio::select! {
                read = stream.read() => read,
                () = stop.cancelled() => break,
            };
            match frame {
                Ok(Some(frame)) => slot.send(Arc::new(frame)),
                Ok(None) => {
                    debug!("video capture ended");
                    return;
                }
                Err(err) => {
                    warn!(error = %err, "video capture failed");
                    slot.fail(Arc::new(Error::device(err)));
                    return;
                }
            }
        }
        debug!("video capture stopped");
    })?;
    let driver = if camera {
        Driver::Camera(Camera {
            task,
            dropped: &DROPPED,
        })
    } else {
        Driver::Local(task)
    };

    match opened.await {
        Ok(Ok(format)) => Ok((format, driver)),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(Error::device_msg(
            "the capture thread stopped before the device opened",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::{Camera, Releases};
    use crate::local_task;

    /// Starts a capture thread that holds its device until the returned sender is used or dropped.
    fn holding(name: &str) -> (local_task::LocalTask, std::sync::mpsc::SyncSender<()>) {
        let (release, released) = std::sync::mpsc::sync_channel::<()>(1);
        let task = local_task::spawn(name, CancellationToken::new(), move |_stop| async move {
            // Ignores the stop, like a read blocked in `poll(-1)`, until the
            // test lets go.
            let _ = released.recv();
        })
        .expect("the thread starts");
        (task, release)
    }

    /// Reopening a camera waits until the dropped capture has closed its device.
    #[tokio::test]
    async fn an_open_waits_for_a_dropped_camera_to_release() {
        static DROPPED: Releases = Releases::new();
        let (task, release) = holding("test-slow-release");
        drop(Camera {
            task,
            dropped: &DROPPED,
        });

        let settle = DROPPED.settle(Duration::from_secs(10));
        tokio::pin!(settle);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut settle)
                .await
                .is_err(),
            "the open went ahead while the device was still held"
        );
        release.send(()).expect("the task is still running");
        tokio::time::timeout(Duration::from_secs(5), settle)
            .await
            .expect("the open goes ahead once the device is released");
        assert!(
            DROPPED
                .0
                .lock()
                .expect("poisoned")
                .iter()
                .all(CancellationToken::is_cancelled),
            "a released capture is forgotten"
        );
    }

    /// A camera that still runs is not waited for.
    #[tokio::test(start_paused = true)]
    async fn a_running_camera_does_not_delay_another_open() {
        static DROPPED: Releases = Releases::new();
        let (task, release) = holding("test-running");
        let running = Camera {
            task,
            dropped: &DROPPED,
        };
        let started = tokio::time::Instant::now();
        DROPPED.settle(Duration::from_secs(3)).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
        drop(running);
        let _ = release.send(());
    }

    /// A capture thread stuck in a read is given up on, once.
    ///
    /// This is a camera whose read blocks without a timeout, such as a Media
    /// Foundation camera that delivers neither a frame nor an error.
    #[tokio::test(start_paused = true)]
    async fn a_stuck_camera_delays_the_next_open_once() {
        let dropped = Releases::new();
        let (task, unstick) = holding("test-stuck");
        dropped.push(&task.released());
        drop(task);

        let started = tokio::time::Instant::now();
        dropped.settle(Duration::from_secs(3)).await;
        assert!(started.elapsed() >= Duration::from_secs(3));

        let started = tokio::time::Instant::now();
        dropped.settle(Duration::from_secs(3)).await;
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "a forgotten capture delays no later open"
        );
        let _ = unstick.send(());
    }
}
