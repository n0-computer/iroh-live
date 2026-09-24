//! Camera, display and window capture, on a thread of its own.
//!
//! moq's native capture backends are not all `Send`: an Apple camera or screen
//! stream holds AVFoundation objects, so neither the stream nor a future
//! holding one can go to a work-stealing executor. The device is opened on a
//! dedicated thread with a current-thread runtime and never leaves it; only
//! its geometry and its frames cross.

use std::{sync::Arc, time::Duration};

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{FIRST_FRAME_PATIENCE, OPEN_PATIENCE, VideoFormat};
use crate::{error::Error, frames::FrameSlot, local_task::LocalTask, video};

/// The first wait before trying a busy device again.
const RETRY_FIRST: Duration = Duration::from_millis(100);

/// The rate assumed for a device that reports none.
fn default_rate() -> video::Rate {
    video::Rate::new(30, 1).expect("30/1 is a valid rate")
}

/// Opens the device `config` names and starts reading it into `slot`.
///
/// Returns once the first frame arrived, with the format the device opened at
/// and the task that owns it.
pub(super) async fn open(
    config: video::capture::Config,
    slot: FrameSlot,
    stop: CancellationToken,
) -> Result<(VideoFormat, LocalTask), Error> {
    let (opened_tx, opened) = oneshot::channel::<Result<VideoFormat, Error>>();
    let task = crate::local_task::spawn("video-capture", stop.clone(), move |stop| async move {
        let mut stream = match open_with_retry(&config, &stop).await {
            Ok(stream) => stream,
            Err(err) => {
                let _ = opened_tx.send(Err(err));
                return;
            }
        };
        let size = video::Size::new(stream.width(), stream.height());
        let rate = config
            .framerate
            .or_else(|| stream.framerate())
            .unwrap_or_else(default_rate);
        let format = VideoFormat { size, rate };

        // The first frame is what proves the device works: a node that opens
        // and hands back nothing (a Pi's Unicam node, whose raw Bayer only
        // libcamera can drive) otherwise looks exactly like a slow camera.
        let first = tokio::select! {
            first = tokio::time::timeout(FIRST_FRAME_PATIENCE, stream.read()) => first,
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
                let _ = opened_tx.send(Err(capture_error(err)));
                return;
            }
            Err(_) => {
                let _ = opened_tx.send(Err(Error::device_msg(format!(
                    "the capture device opened but produced no frame within {}s",
                    FIRST_FRAME_PATIENCE.as_secs()
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
                    slot.close(None);
                    return;
                }
                Err(err) => {
                    warn!(error = %err, "video capture failed");
                    slot.close(Some(Arc::new(capture_error(err))));
                    return;
                }
            }
        }
        debug!("video capture stopped");
    })?;

    match opened.await {
        Ok(Ok(format)) => Ok((format, task)),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(Error::device_msg(
            "the capture thread stopped before the device opened",
        )),
    }
}

/// Opens the device, trying a busy one again for a moment.
///
/// A device is busy far more often than it is broken, and the two look the
/// same from here: `EBUSY` from a camera another part of this program is just
/// handing over looks like one that will never open. A short patience covers
/// the handover without hiding a device that is gone.
async fn open_with_retry(
    config: &video::capture::Config,
    stop: &CancellationToken,
) -> Result<video::capture::Stream, Error> {
    let started = tokio::time::Instant::now();
    let mut backoff = RETRY_FIRST;
    loop {
        let result = tokio::select! {
            opened = video::capture::open(config) => opened,
            () = stop.cancelled() => return Err(n0_error::e!(Error::Closed)),
        };
        let err = match result {
            Ok(stream) => return Ok(stream),
            Err(err) => err,
        };
        if started.elapsed() + backoff > OPEN_PATIENCE {
            return Err(capture_error(err));
        }
        debug!(error = %err, retry_in = ?backoff, "the capture device would not open, trying again");
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            () = stop.cancelled() => return Err(n0_error::e!(Error::Closed)),
        }
        backoff *= 2;
    }
}

/// A capture failure, as the crate reports it.
fn capture_error(err: video::Error) -> Error {
    Error::device(err)
}
