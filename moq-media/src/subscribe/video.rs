//! The video decode task, and the rendition swap it supervises.
//!
//! One decoder runs at a time. A switch opens the replacement alongside it and
//! hands over on the replacement's first frame, so the picture never goes blank
//! across a rendition change. That overlap is the whole reason this is a
//! supervisor rather than a plain read loop.
//!
//! Each decoder is read by its own task rather than from a `select!` arm.
//! `moq_video::decode::Consumer` reads through a `Sink`, which is documented as
//! not cancel-safe: dropping a `read` future poisons the decoder for good. A
//! `select!` cancels every arm it does not pick, so the read has to live
//! somewhere nothing cancels it and reach the supervisor over a channel.

use std::{sync::Arc, time::Duration};

use n0_error::{Result, e};
use n0_future::task::{AbortOnDropHandle, JoinHandle, spawn};
use n0_watcher::Watchable;
use tokio::sync::{mpsc, watch};
use tracing::{Instrument, debug, error_span, info, warn};

use super::{
    DecodeContext, RemoteBroadcast, SubscribeError, VideoControl, VideoTrack, is_synced,
    video_decode_config,
};
use crate::frame_channel::{FrameSender, frame_channel};

/// How many decoded frames a reader may run ahead of the supervisor.
///
/// Small on purpose: the supervisor only paces and forwards, so a backlog here
/// would be latency rather than throughput. Two slots absorb a scheduling
/// hiccup without letting the decoder race ahead of the clock.
const READ_AHEAD: usize = 2;

/// Opens `rendition` and starts decoding it.
pub(super) async fn open(
    broadcast: &RemoteBroadcast,
    rendition: &str,
) -> Result<VideoTrack, SubscribeError> {
    let reader = spawn_reader(broadcast, rendition).await?;

    let (frames_tx, frames_rx) = frame_channel();
    let decoder = Watchable::new(reader.decoder.clone());
    let current = Watchable::new(rendition.to_string());
    let (requested_tx, requested_rx) = watch::channel(None);

    let task = spawn(
        supervise(
            broadcast.clone(),
            reader,
            frames_tx,
            current.clone(),
            decoder.clone(),
            requested_tx.clone(),
            requested_rx,
        )
        .instrument(error_span!("video", broadcast = %broadcast.name())),
    );

    Ok(VideoTrack {
        frames: Arc::new(frames_rx),
        rendition: current,
        decoder,
        control: Arc::new(VideoControl {
            broadcast: broadcast.clone(),
            requested: requested_tx,
            _task: AbortOnDropHandle::new(task),
            adaptation: Default::default(),
        }),
    })
}

/// One decoder plus the task reading it.
struct Reader {
    /// The backend that opened, for a status line: which decoder is running is
    /// the first thing anyone asks when playback looks wrong on a device.
    decoder: String,
    frames: mpsc::Receiver<moq_video::Frame>,
    /// Dropping this aborts the read loop, which drops the decoder with it.
    _task: AbortOnDropHandle<()>,
}

/// Subscribes to a rendition, opens its decoder, and starts reading it.
///
/// Returns once the decoder is open, so a caller can tell an unusable rendition
/// from a slow one before committing to a switch.
async fn spawn_reader(
    broadcast: &RemoteBroadcast,
    rendition: &str,
) -> Result<Reader, SubscribeError> {
    let catalog = broadcast.catalog();
    let config = catalog.video().get(rendition).cloned().ok_or_else(|| {
        e!(SubscribeError::NoRendition {
            name: rendition.to_string(),
        })
    })?;
    let decode = video_decode_config(&broadcast.playback_policy());
    let mut consumer =
        moq_video::decode::Consumer::new(broadcast.consumer(), &config, rendition, decode).await?;
    let decoder = consumer.name().to_string();
    broadcast.stats().render.decoder.set(&decoder);
    broadcast.stats().render.rendition.set(rendition);
    info!(rendition, decoder = %decoder, "video decoding");

    let (tx, frames) = mpsc::channel(READ_AHEAD);
    let name = rendition.to_string();
    let stats = broadcast.stats().clone();
    let task = spawn(
        async move {
            loop {
                let started = std::time::Instant::now();
                match consumer.read().await {
                    Ok(Some(frame)) => {
                        // Covers the transport read as well as the decode: the
                        // two happen inside one `read`, with no earlier point
                        // to attribute arrival to.
                        stats
                            .render
                            .decode_ms
                            .record(started.elapsed().as_secs_f64() * 1000.0);
                        if tx.send(frame).await.is_err() {
                            debug!("nobody is reading this rendition any more");
                            return;
                        }
                    }
                    Ok(None) => {
                        debug!("video track ended");
                        return;
                    }
                    Err(err) => {
                        warn!(error = %err, "video decode failed");
                        return;
                    }
                }
            }
        }
        .instrument(error_span!("decode", rendition = %name)),
    );

    Ok(Reader {
        decoder,
        frames,
        _task: AbortOnDropHandle::new(task),
    })
}

/// Forwards frames to the renderer and swaps decoders when a switch is asked for.
async fn supervise(
    broadcast: RemoteBroadcast,
    mut reader: Reader,
    frames: FrameSender<moq_video::Frame>,
    current: Watchable<String>,
    decoder: Watchable<String>,
    withdraw: watch::Sender<Option<String>>,
    mut requested: watch::Receiver<Option<String>>,
) {
    let context = broadcast.decode_context();
    let synced = is_synced(&context.policy);
    // The replacement, from the moment its decoder opens until its first frame
    // arrives. Holding both is what keeps the picture up across the swap.
    let mut pending: Option<(String, Reader)> = None;
    // The open in flight. It runs as its own task rather than inside a `select!`
    // arm, because opening a decoder means a network round trip and a codec
    // open, and awaiting that in an arm body stops the incumbent from being
    // forwarded for exactly the window the overlap exists to hide.
    let mut opening: Option<(String, JoinHandle<Result<Reader, SubscribeError>>)> = None;
    // The last frame's arrival, for the frame rate the overlay draws.
    let mut previous: Option<std::time::Instant> = None;

    loop {
        tokio::select! {
            biased;

            _ = context.shutdown.cancelled() => {
                debug!("video decode cancelled");
                return;
            }

            changed = requested.changed() => {
                if changed.is_err() {
                    return;
                }
                let Some(name) = requested.borrow_and_update().clone() else { continue };
                let already = pending.as_ref().is_some_and(|(open, _)| *open == name)
                    || opening.as_ref().is_some_and(|(open, _)| *open == name);
                if name == current.get() || already {
                    continue;
                }
                debug!(rendition = %name, "opening replacement decoder");
                let opened = spawn({
                    let broadcast = broadcast.clone();
                    let name = name.clone();
                    async move { spawn_reader(&broadcast, &name).await }
                });
                // A switch requested while another was opening supersedes it;
                // dropping the handle aborts the open we no longer want.
                opening = Some((name, opened));
            }

            // The replacement's decoder is open. Hold it until it produces a
            // frame, so the swap never shows an empty picture.
            opened = async { (&mut opening.as_mut().expect("guarded").1).await },
                if opening.is_some() =>
            {
                let (name, _) = opening.take().expect("guarded");
                match opened {
                    Ok(Ok(replacement)) => pending = Some((name, replacement)),
                    Ok(Err(err)) => {
                        warn!(error = %err, rendition = %name, "replacement failed to open");
                        clear_request(&withdraw, &name);
                    }
                    Err(err) => {
                        warn!(error = %err, rendition = %name, "replacement open task failed");
                        clear_request(&withdraw, &name);
                    }
                }
            }

            // The replacement's first frame: hand over.
            frame = async { pending.as_mut().expect("guarded").1.frames.recv().await },
                if pending.is_some() =>
            {
                let (name, replacement) = pending.take().expect("guarded");
                match frame {
                    Some(frame) => {
                        info!(rendition = %name, decoder = %replacement.decoder, "switched rendition");
                        decoder.set(replacement.decoder.clone()).ok();
                        reader = replacement;
                        current.set(name).ok();
                        deliver(&frames, frame, &context, synced, &mut previous).await;
                    }
                    None => {
                        debug!(rendition = %name, "replacement ended before its first frame");
                        clear_request(&withdraw, &name);
                    }
                }
            }

            frame = reader.frames.recv() => match frame {
                Some(frame) => deliver(&frames, frame, &context, synced, &mut previous).await,
                None => {
                    debug!("video decode ended");
                    return;
                }
            },
        }
    }
}

/// Withdraws a switch request that did not land.
///
/// The adaptation loop holds off while a request is outstanding, so leaving a
/// failed one set would turn adaptation off for the rest of the session, and it
/// would happen on the first downgrade under congestion, which is exactly when
/// it is needed. Only withdraws the request we tried, so a newer one placed
/// meanwhile survives.
fn clear_request(requested: &watch::Sender<Option<String>>, tried: &str) {
    // `false` from the closure suppresses the change notification: withdrawing
    // a request is not itself a request, and waking the supervisor for it would
    // only make it re-read a value it just wrote.
    requested.send_if_modified(|current| {
        if current.as_deref() == Some(tried) {
            *current = None;
        }
        false
    });
}

/// Paces one frame against the playout clock and hands it to the renderer.
async fn deliver(
    frames: &FrameSender<moq_video::Frame>,
    frame: moq_video::Frame,
    context: &DecodeContext,
    synced: bool,
    previous: &mut Option<std::time::Instant>,
) {
    let pts = Duration::from_micros(frame.timestamp.as_micros() as u64);
    // The metric smooths whatever value it is handed, so it wants the
    // instantaneous rate, not a tick. Timed at arrival rather than from the
    // presentation timestamps, because a stall shows up here and not there.
    let now = std::time::Instant::now();
    if let Some(previous) = previous.replace(now) {
        let gap = now.duration_since(previous).as_secs_f64();
        if gap > 0.0 {
            context.stats.render.fps.record(1.0 / gap);
        }
    }
    if synced {
        context.sync.received(pts);
        if !context.sync.wait_async(pts).await {
            return;
        }
    }
    frames.send(frame);
}
