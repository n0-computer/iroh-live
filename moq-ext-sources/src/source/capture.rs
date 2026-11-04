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

use super::{FIRST_FRAME_PATIENCE, VideoFormat};
use crate::{Error, frames::FrameSlot, local_task::LocalTask, video};

/// How long an opened camera may take over its first frame.
///
/// A webcam streams within a second or two of opening. Until the first frame
/// arrives, a caller that closed its old source shows a still picture, so a
/// camera that will never stream should fail sooner than the general
/// [`FIRST_FRAME_PATIENCE`].
const CAMERA_FIRST_FRAME_PATIENCE: Duration = Duration::from_secs(10);

/// How long a camera open waits for an earlier capture to release its device.
///
/// A stopped capture lets go within a frame interval, and a V4L2 read that
/// waits for a frame gives up after a second once moq-video sets a read
/// timeout (moq-dev/moq#7878). A capture that takes longer is stuck in a read,
/// and waiting longer would not help.
const RELEASE_PATIENCE: Duration = Duration::from_secs(3);

/// The camera captures of this process.
///
/// A camera is a process-wide resource, so a new open waits here for the
/// captures stopped before it. Opening while one still holds the device fails
/// with `EBUSY` on V4L2.
static CAMERAS: Releases = Releases::new();

/// Capture threads that may still hold a device.
#[derive(Debug)]
struct Releases {
    state: Mutex<ReleasesState>,
}

#[derive(Debug)]
struct ReleasesState {
    next_id: u64,
    tasks: Vec<Tracked>,
}

/// One capture thread, from its open until it has finished.
#[derive(Debug, Clone)]
struct Tracked {
    id: u64,
    /// The device, for the warning about a capture that does not let go.
    label: String,
    /// Cancelled once the capture was told to stop.
    stop: CancellationToken,
    /// Cancelled once its thread has finished and the device is closed.
    released: CancellationToken,
}

impl Releases {
    const fn new() -> Self {
        Self {
            state: Mutex::new(ReleasesState {
                next_id: 0,
                tasks: Vec::new(),
            }),
        }
    }

    /// Remembers a capture until its thread has finished.
    fn track(&self, label: String, stop: CancellationToken, released: CancellationToken) {
        let mut state = self.state.lock().expect("poisoned");
        state.tasks.retain(|task| !task.released.is_cancelled());
        let id = state.next_id;
        state.next_id += 1;
        state.tasks.push(Tracked {
            id,
            label,
            stop,
            released,
        });
    }

    /// Waits until every stopped capture has released its device, for at most
    /// `patience`.
    ///
    /// A capture that still runs is not waited for: the application may use
    /// two cameras at once. A stopped capture that does not finish in time is
    /// forgotten with a warning, so later opens do not wait for it again. Its
    /// thread may keep the device open until its read returns, which on a
    /// moq-video without a V4L2 read timeout is never.
    async fn settle(&self, patience: Duration) {
        let stopped: Vec<Tracked> = {
            let mut state = self.state.lock().expect("poisoned");
            state.tasks.retain(|task| !task.released.is_cancelled());
            state
                .tasks
                .iter()
                .filter(|task| task.stop.is_cancelled())
                .cloned()
                .collect()
        };
        if stopped.is_empty() {
            return;
        }
        debug!(
            count = stopped.len(),
            "waiting for a stopped capture to release its device"
        );
        let deadline = tokio::time::Instant::now() + patience;
        for task in stopped {
            if tokio::time::timeout_at(deadline, task.released.cancelled())
                .await
                .is_ok()
            {
                continue;
            }
            warn!(
                device = %task.label,
                patience = ?patience,
                "a stopped capture still holds its device; opening without it"
            );
            self.state
                .lock()
                .expect("poisoned")
                .tasks
                .retain(|tracked| tracked.id != task.id);
        }
        self.state
            .lock()
            .expect("poisoned")
            .tasks
            .retain(|task| !task.released.is_cancelled());
    }
}

/// The rate assumed for a device that reports none.
fn default_rate() -> video::FrameRate {
    video::FrameRate::new(30, 1).expect("30/1 is a valid rate")
}

/// Opens the device `config` names and starts reading it into `slot`.
///
/// A camera first waits, for at most [`RELEASE_PATIENCE`], until the cameras
/// stopped before it have released their devices. Returns once the first frame
/// has arrived, with the format the device opened at and the task that owns
/// it.
pub(super) async fn open(
    config: video::capture::Config,
    slot: FrameSlot,
    stop: CancellationToken,
) -> Result<(VideoFormat, LocalTask), Error> {
    let camera = match &config.source {
        video::capture::Source::Camera(id) => {
            Some(id.as_deref().unwrap_or("the default camera").to_string())
        }
        _ => None,
    };
    let patience = match camera {
        Some(_) => CAMERA_FIRST_FRAME_PATIENCE,
        None => FIRST_FRAME_PATIENCE,
    };
    if camera.is_some() {
        CAMERAS.settle(RELEASE_PATIENCE).await;
    }
    let (opened_tx, opened) = oneshot::channel::<Result<VideoFormat, Error>>();
    let task = crate::local_task::spawn("video-capture", stop.clone(), move |stop| async move {
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
    if let Some(label) = camera {
        CAMERAS.track(label, stop, task.released());
    }

    match opened.await {
        Ok(Ok(format)) => Ok((format, task)),
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

    use super::Releases;
    use crate::local_task;

    /// A switch waits until the capture it replaces has closed its device.
    #[tokio::test]
    async fn a_switch_waits_for_the_previous_capture_to_release() {
        let releases = Releases::new();
        let stop = CancellationToken::new();
        let (release, released) = std::sync::mpsc::sync_channel::<()>(1);
        let task = local_task::spawn("test-slow-release", stop.clone(), move |_stop| async move {
            // Closing the device takes a moment after the stop.
            let _ = released.recv();
            std::thread::sleep(Duration::from_millis(100));
        })
        .expect("the thread starts");
        releases.track("slow".into(), stop, task.released());
        drop(task);
        release.send(()).expect("the task is still running");

        let started = std::time::Instant::now();
        releases.settle(Duration::from_secs(10)).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the release took far longer than the thread"
        );
        assert!(
            releases.state.lock().expect("poisoned").tasks.is_empty(),
            "a released capture is forgotten"
        );
    }

    /// A capture that still runs is not waited for.
    #[tokio::test(start_paused = true)]
    async fn a_running_capture_does_not_delay_another_open() {
        let releases = Releases::new();
        let running = CancellationToken::new();
        releases.track("running".into(), running, CancellationToken::new());
        let started = tokio::time::Instant::now();
        releases.settle(Duration::from_secs(3)).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    /// A capture thread stuck in a read is given up on, once.
    ///
    /// This is a V4L2 camera that opened and never streams, on a moq-video
    /// without a read timeout: its thread never finishes.
    #[tokio::test(start_paused = true)]
    async fn a_stuck_capture_delays_the_next_open_once() {
        let releases = Releases::new();
        let stop = CancellationToken::new();
        let (unstick, stuck) = std::sync::mpsc::sync_channel::<()>(1);
        let task = local_task::spawn("test-stuck", stop.clone(), move |_stop| async move {
            // Ignores the stop, like a read blocked in `poll(-1)`.
            let _ = stuck.recv();
        })
        .expect("the thread starts");
        releases.track("stuck".into(), stop, task.released());
        drop(task);

        let started = tokio::time::Instant::now();
        releases.settle(Duration::from_secs(3)).await;
        assert!(started.elapsed() >= Duration::from_secs(3));

        let started = tokio::time::Instant::now();
        releases.settle(Duration::from_secs(3)).await;
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "a forgotten capture delays no later open"
        );
        let _ = unstick.send(());
    }
}
