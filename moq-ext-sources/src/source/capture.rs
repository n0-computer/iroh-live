//! Camera, display and window capture, on a thread of its own.
//!
//! The device opens on a dedicated thread and never leaves it, because Apple
//! capture streams are not `Send`. Only its format and its frames cross.

use std::{sync::Arc, time::Duration};

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

/// The rate assumed for a device that reports none.
fn default_rate() -> video::FrameRate {
    video::FrameRate::new(30, 1).expect("30/1 is a valid rate")
}

/// Opens the device `config` names and starts reading it into `slot`.
///
/// Returns once the first frame has arrived, with the format the device opened
/// at and the task that owns it.
pub(super) async fn open(
    config: video::capture::Config,
    slot: FrameSlot,
    stop: CancellationToken,
) -> Result<(VideoFormat, LocalTask), Error> {
    let patience = match config.source {
        video::capture::Source::Camera(_) => CAMERA_FIRST_FRAME_PATIENCE,
        _ => FIRST_FRAME_PATIENCE,
    };
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

    match opened.await {
        Ok(Ok(format)) => Ok((format, task)),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(Error::device_msg(
            "the capture thread stopped before the device opened",
        )),
    }
}
