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

/// The camera captures of this process that may still hold a device.
///
/// Opening a camera that a dropped capture still holds fails as busy on V4L2,
/// so a camera open waits here for the captures dropped before it.
static CAMERAS: Cameras = Cameras::new();

/// Camera captures from their open until their thread has closed the device.
///
/// A dropped capture of any camera is waited for, not only one of the same
/// name: the default camera, a device path and a PipeWire node can all name
/// the same device, and a release takes no more than a frame interval.
#[derive(Debug)]
struct Cameras {
    captures: Mutex<Vec<Arc<Capture>>>,
}

/// One camera capture.
#[derive(Debug)]
struct Capture {
    /// Cancelled once the capture's source is dropped.
    stop: CancellationToken,
    /// Cancelled once its thread has finished and the device is closed.
    released: CancellationToken,
}

impl Cameras {
    const fn new() -> Self {
        Self {
            captures: Mutex::new(Vec::new()),
        }
    }

    /// Remembers a camera capture until its thread has released the device.
    fn track(&self, stop: CancellationToken, released: CancellationToken) {
        let mut captures = self.captures.lock().expect("poisoned");
        captures.retain(|capture| !capture.released.is_cancelled());
        captures.push(Arc::new(Capture { stop, released }));
    }

    /// Waits at most `patience` for every dropped capture to release its device.
    ///
    /// A capture that still runs is not waited for, since an application may
    /// use two cameras at once. A dropped capture that does not let go in time
    /// is stuck in a read: it is forgotten with a warning, so later opens do
    /// not wait for it again, and its thread may hold the device until the
    /// read returns.
    async fn settle(&self, patience: Duration) {
        let dropped: Vec<Arc<Capture>> = {
            let mut captures = self.captures.lock().expect("poisoned");
            captures.retain(|capture| !capture.released.is_cancelled());
            captures
                .iter()
                .filter(|capture| capture.stop.is_cancelled())
                .cloned()
                .collect()
        };
        if dropped.is_empty() {
            return;
        }
        debug!(
            count = dropped.len(),
            "waiting for a dropped camera to release its device"
        );
        let deadline = tokio::time::Instant::now() + patience;
        for capture in dropped {
            if tokio::time::timeout_at(deadline, capture.released.cancelled())
                .await
                .is_ok()
            {
                continue;
            }
            warn!(
                ?patience,
                "a dropped camera still holds its device, opening without it"
            );
            self.captures
                .lock()
                .expect("poisoned")
                .retain(|tracked| !Arc::ptr_eq(tracked, &capture));
        }
        self.captures
            .lock()
            .expect("poisoned")
            .retain(|capture| !capture.released.is_cancelled());
    }
}

/// The rate assumed for a device that reports none.
fn default_rate() -> video::FrameRate {
    video::FrameRate::new(30, 1).expect("30/1 is a valid rate")
}

/// Waits at most [`RELEASE_PATIENCE`] for the dropped cameras to release their devices.
pub(super) async fn settle() {
    CAMERAS.settle(RELEASE_PATIENCE).await;
}

/// Opens the device `config` names and starts reading it into `slot`.
///
/// A camera is tracked in [`CAMERAS`] until its thread has closed it. Returns
/// once the first frame has arrived, with the format the device opened at and
/// the task that owns it.
pub(super) async fn open(
    config: video::capture::Config,
    slot: FrameSlot,
    stop: CancellationToken,
) -> Result<(VideoFormat, LocalTask), Error> {
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
    if camera {
        CAMERAS.track(stop, task.released());
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

    use super::Cameras;
    use crate::local_task;

    /// Reopening a camera waits until the dropped capture has closed its device.
    #[tokio::test]
    async fn an_open_waits_for_a_dropped_camera_to_release() {
        let cameras = Cameras::new();
        let stop = CancellationToken::new();
        let (release, released) = std::sync::mpsc::sync_channel::<()>(1);
        let task = local_task::spawn("test-slow-release", stop.clone(), move |_stop| async move {
            // Closing the device takes until the test lets go.
            let _ = released.recv();
        })
        .expect("the thread starts");
        cameras.track(stop, task.released());
        drop(task);

        let settle = cameras.settle(Duration::from_secs(10));
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
            cameras.captures.lock().expect("poisoned").is_empty(),
            "a released capture is forgotten"
        );
    }

    /// A camera that still runs is not waited for.
    #[tokio::test(start_paused = true)]
    async fn a_running_camera_does_not_delay_another_open() {
        let cameras = Cameras::new();
        cameras.track(CancellationToken::new(), CancellationToken::new());
        let started = tokio::time::Instant::now();
        cameras.settle(Duration::from_secs(3)).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    /// A capture thread stuck in a read is given up on, once.
    ///
    /// This is a camera whose read blocks without a timeout, such as a Media
    /// Foundation camera that delivers neither a frame nor an error.
    #[tokio::test(start_paused = true)]
    async fn a_stuck_camera_delays_the_next_open_once() {
        let cameras = Cameras::new();
        let stop = CancellationToken::new();
        let (unstick, stuck) = std::sync::mpsc::sync_channel::<()>(1);
        let task = local_task::spawn("test-stuck", stop.clone(), move |_stop| async move {
            // Ignores the stop, like a read blocked in `poll(-1)`.
            let _ = stuck.recv();
        })
        .expect("the thread starts");
        cameras.track(stop, task.released());
        drop(task);

        let started = tokio::time::Instant::now();
        cameras.settle(Duration::from_secs(3)).await;
        assert!(started.elapsed() >= Duration::from_secs(3));

        let started = tokio::time::Instant::now();
        cameras.settle(Duration::from_secs(3)).await;
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "a forgotten capture delays no later open"
        );
        let _ = unstick.send(());
    }
}
