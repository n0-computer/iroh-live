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
//!
//! An access unit the decoder refuses is skipped rather than fatal. A live
//! stream loses pictures to a skipped group or a truncated access unit, and a
//! decoder without its reference chain refuses every picture until the next
//! keyframe, so a reader that stopped on the first of those would turn a
//! recoverable break into a permanent freeze. The reader gives up only on a run
//! long enough that no keyframe is coming, and says so.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use n0_error::{Result, e};
use n0_future::task::{AbortOnDropHandle, spawn};
use n0_watcher::Watchable;
use tokio::sync::{mpsc, watch};
use tracing::{Instrument, debug, error, error_span, info, warn};

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

/// How long failures are counted over before the share that decoded is judged.
const FAILURE_WINDOW: Duration = Duration::from_secs(10);

/// The share of access units that must decode for a track to count as playing.
///
/// A decoder taking only keyframes off a two second GOP lands near a fiftieth,
/// and an ordinary stream recovering from one skipped group is far above this
/// within a window.
const MIN_DECODED_SHARE: f64 = 0.5;

/// How long a replacement decoder may take over its first picture.
///
/// It has to cover a real handover: the replacement subscribes to another
/// rendition and waits for that track's next keyframe, which on a two second
/// GOP over an impaired link is already seconds. Beyond that it is not slow,
/// it is not coming, and the incumbent keeps playing either way.
const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(15);

/// How many access units in a row may fail to decode before the reader stops.
///
/// One failure is not a broken stream. A group skipped under congestion, a
/// truncated access unit, or a decoder that ran out of picture buffers all cost
/// the reference chain, and a decoder without it refuses every picture until the
/// next keyframe. So the threshold has to span a keyframe interval, or a
/// stream a keyframe was about to repair would be ended a frame into the
/// break, which is the freeze this exists to prevent.
///
/// Publishers key every two seconds by default, which is 120 access units at
/// 60fps and 60 at 30. Three hundred spans several of those at any rate we
/// publish, and still reports a decoder that will never produce another picture
/// within about ten seconds rather than reading a track forever.
const MAX_CONSECUTIVE_DECODE_FAILURES: u32 = 300;

/// What a failed read is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadFailure {
    /// One access unit the decoder would not take, which the next keyframe
    /// makes good.
    Picture,
    /// The track itself rather than anything in it: the transport dropped it,
    /// or the container will not parse. Reading again fails the same way.
    Track,
}

impl From<&moq_video::Error> for ReadFailure {
    fn from(err: &moq_video::Error) -> Self {
        // `Codec` is what a decode backend returns, and the only variant that
        // is about the bytes of one picture. Transport and container errors
        // describe the track, and retrying one of those is a spin.
        match err {
            moq_video::Error::Codec(_) => Self::Picture,
            _ => Self::Track,
        }
    }
}

/// What to do about an access unit the decoder would not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterFailure {
    /// Skip it and keep reading. The next keyframe restores the picture.
    Skip,
    /// Stop reading: no keyframe is going to arrive that this decoder can use.
    GiveUp,
}

/// The run of decode failures since the last picture.
#[derive(Debug)]
struct DecodeFailures {
    /// The run since the last picture, which is what the give-up threshold
    /// counts.
    run: u32,
    /// Failures and pictures over the window below, which is what catches a
    /// decoder that keeps producing just enough to reset the run.
    window: Window,
}

/// Failures and pictures since the window opened.
#[derive(Debug)]
struct Window {
    started: Instant,
    failed: u32,
    decoded: u32,
    reported: bool,
}

/// How often the decode cadence is logged.
///
/// A picture that is starving looks, from every log line above this one, like
/// a picture that is fine: the transport signals say what arrived and the
/// decoder says nothing unless it fails. One line every few seconds with the
/// frame rate that actually decoded is what lets a goodput reading in a trace
/// be matched to what the viewer saw.
const CADENCE_EVERY: Duration = Duration::from_secs(5);

/// Pictures decoded since the cadence was last reported.
#[derive(Debug)]
struct Cadence {
    since: Instant,
    decoded: u32,
}

impl Cadence {
    /// Counts a picture, and returns the frame rate since the last report
    /// once [`CADENCE_EVERY`] has passed.
    fn decoded(&mut self, now: Instant) -> Option<f64> {
        self.decoded += 1;
        let elapsed = now.duration_since(self.since);
        if elapsed < CADENCE_EVERY {
            return None;
        }
        let fps = f64::from(self.decoded) / elapsed.as_secs_f64();
        self.since = now;
        self.decoded = 0;
        Some(fps)
    }
}

impl Default for DecodeFailures {
    fn default() -> Self {
        Self {
            run: 0,
            window: Window {
                started: Instant::now(),
                failed: 0,
                decoded: 0,
                reported: false,
            },
        }
    }
}

impl DecodeFailures {
    /// Records a failure and says whether to carry on.
    fn failed(&mut self) -> AfterFailure {
        self.run += 1;
        self.window.failed += 1;
        match self.run >= MAX_CONSECUTIVE_DECODE_FAILURES {
            true => AfterFailure::GiveUp,
            false => AfterFailure::Skip,
        }
    }

    /// Records a picture, ending whatever run was going on.
    ///
    /// Returns how long the run was, so a recovery can report what it cost.
    fn decoded(&mut self) -> u32 {
        self.window.decoded += 1;
        std::mem::take(&mut self.run)
    }

    /// The length of the run so far.
    fn len(&self) -> u32 {
        self.run
    }

    /// Returns the share of access units that decoded, once a window has
    /// closed on a track that is mostly failing.
    ///
    /// The run counter alone cannot see this. A decoder that takes a keyframe
    /// and refuses everything after it resets the run at every group, so on a
    /// two second GOP the longest run is sixty and the threshold of three
    /// hundred is never approached, while what reaches the screen is one
    /// picture every two seconds. That is a broken decoder, and it is worth
    /// one line saying so rather than a slideshow nobody can explain.
    ///
    /// Reports rather than gives up: half a frame a second is a poor picture
    /// and no picture is a worse one, and the reader has no better decoder to
    /// switch to on its own.
    fn mostly_failing(&mut self) -> Option<f64> {
        if self.window.reported || self.window.started.elapsed() < FAILURE_WINDOW {
            return None;
        }
        let total = self.window.failed + self.window.decoded;
        let share = f64::from(self.window.decoded) / f64::from(total.max(1));
        self.window.started = Instant::now();
        self.window.failed = 0;
        self.window.decoded = 0;
        if share >= MIN_DECODED_SHARE {
            return None;
        }
        self.window.reported = true;
        Some(share)
    }
}

/// Opens `rendition` and starts decoding it.
pub(super) async fn open(
    broadcast: &RemoteBroadcast,
    rendition: &str,
) -> Result<VideoTrack, SubscribeError> {
    let reader = spawn_reader(broadcast, rendition).await?;

    let (frames_tx, frames_rx) = frame_channel();
    let decoder = Watchable::new(reader.decoder.clone());
    let current = Watchable::new(rendition.to_string());
    // Labelled here rather than in `spawn_reader`, so an open that is
    // superseded before it plays never names itself in the overlay.
    broadcast.stats().render.decoder.set(&reader.decoder);
    broadcast.stats().render.rendition.set(rendition);
    let (requested_tx, requested_rx) = watch::channel(None);
    let (reopen_tx, reopen_rx) = watch::channel(0);

    let task = spawn(
        supervise(
            broadcast.clone(),
            reader,
            frames_tx,
            current.clone(),
            decoder.clone(),
            requested_tx.clone(),
            requested_rx,
            reopen_rx,
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
            reopen: reopen_tx,
            _task: AbortOnDropHandle::new(task),
            adaptation: Default::default(),
        }),
    })
}

/// An open in flight: the rendition it is for, and the task doing it.
struct Opening {
    name: String,
    task: AbortOnDropHandle<Result<Reader, SubscribeError>>,
    /// Whether this open is a decoder rebuild rather than a rendition switch.
    ///
    /// The two share one slot and are cancelled by different things. Asking for
    /// the rendition already playing cancels a switch, because that is what
    /// un-pinning means, and it must not cancel a rebuild: the rebuild is for
    /// that same rendition, so the name it carries is the one being asked for.
    ///
    /// Set through [`Opening::switch`] and [`Opening::rebuild`] rather than a
    /// literal, so the arm that spawns a rebuild cannot tag it as a switch. It
    /// did once, and the guard below never fired.
    rebuild: bool,
}

impl Opening {
    /// A rendition switch: an open for a rendition other than the one playing.
    fn switch(name: String, task: AbortOnDropHandle<Result<Reader, SubscribeError>>) -> Self {
        Self {
            name,
            task,
            rebuild: false,
        }
    }

    /// A decoder rebuild: an open for the rendition already playing, under a
    /// changed policy.
    fn rebuild(name: String, task: AbortOnDropHandle<Result<Reader, SubscribeError>>) -> Self {
        Self {
            name,
            task,
            rebuild: true,
        }
    }

    /// Whether a request for the rendition already playing leaves this open
    /// alone.
    ///
    /// A switch is what such a request cancels, since un-pinning a rendition
    /// means "stop the swap". A rebuild is for that same rendition and is the
    /// only record left of a decoder change, so cancelling it would drop the
    /// change with nothing to retry it.
    fn survives_repin(&self) -> bool {
        self.rebuild
    }
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
    info!(rendition, decoder = %decoder, "video decoding");

    let (tx, frames) = mpsc::channel(READ_AHEAD);
    let name = rendition.to_string();
    let stats = broadcast.stats().clone();
    let task = spawn(
        async move {
            let mut failures = DecodeFailures::default();
            let mut cadence = Cadence {
                since: Instant::now(),
                decoded: 0,
            };
            loop {
                let started = std::time::Instant::now();
                match consumer.read().await {
                    Ok(Some(frame)) => {
                        let skipped = failures.decoded();
                        if skipped > 0 {
                            info!(skipped, "video decoding recovered");
                        }
                        if let Some(fps) = cadence.decoded(Instant::now()) {
                            debug!(fps = format_args!("{fps:.1}"), "video decoding cadence");
                        }
                        if let Some(share) = failures.mostly_failing() {
                            error!(
                                decoded = format_args!("{:.0}%", share * 100.0),
                                over = ?FAILURE_WINDOW,
                                "this decoder is refusing most of the stream, so the picture \
                                 is a fraction of the frame rate it should be",
                            );
                        }
                        // Covers the transport read as well as the decode: the
                        // two happen inside one `read`, with no earlier point
                        // to attribute arrival to.
                        stats.render.decode_ms.record_ms(started.elapsed());
                        if tx.send(frame).await.is_err() {
                            debug!("nobody is reading this rendition any more");
                            return;
                        }
                    }
                    Ok(None) => {
                        debug!("video track ended");
                        return;
                    }
                    Err(err) if ReadFailure::from(&err) == ReadFailure::Track => {
                        warn!(error = %err, "video track failed");
                        return;
                    }
                    Err(err) => match failures.failed() {
                        AfterFailure::Skip => {
                            // Once per run rather than once per access unit: a
                            // lost reference chain fails every picture until
                            // the next keyframe, and the first of those says
                            // everything the rest would.
                            if failures.len() == 1 {
                                warn!(error = %err, "video decode failed, skipping the access unit");
                            } else {
                                debug!(error = %err, skipped = failures.len(), "video decode failed");
                            }
                        }
                        AfterFailure::GiveUp => {
                            error!(
                                error = %err,
                                failures = failures.len(),
                                "giving up on this rendition: no access unit has decoded for a long time",
                            );
                            return;
                        }
                    },
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
#[allow(
    clippy::too_many_arguments,
    reason = "one handle per thing the supervisor drives, all owned by the track"
)]
async fn supervise(
    broadcast: RemoteBroadcast,
    reader: Reader,
    frames: FrameSender<moq_video::Frame>,
    current: Watchable<String>,
    decoder: Watchable<String>,
    withdraw: watch::Sender<Option<String>>,
    mut requested: watch::Receiver<Option<String>>,
    mut reopen: watch::Receiver<u64>,
) {
    let context = broadcast.decode_context();
    let synced = is_synced(&context.policy);
    // The replacement, from the moment its decoder opens until its first frame
    // arrives. Holding both is what keeps the picture up across the swap.
    // `None` while the incumbent has ended and a replacement is still opening.
    // Nothing is playing in that window, so every exit below has to check
    // whether anything is left before parking on it.
    let mut reader: Option<Reader> = Some(reader);
    // The replacement, and the moment it stops being worth waiting for. A
    // decoder that opens and never produces a picture would otherwise be waited
    // on for the rest of the session: its reader stays blocked on the track, so
    // the channel never closes and the arm below never fires.
    let mut pending: Option<(String, Reader, tokio::time::Instant)> = None;
    // The open in flight. It runs as its own task rather than inside a `select!`
    // arm, because opening a decoder means a network round trip and a codec
    // open, and awaiting that in an arm body stops the incumbent from being
    // forwarded for exactly the window the overlap exists to hide.
    let mut opening: Option<Opening> = None;
    // The last frame's arrival, for the frame rate the overlay draws.
    // One rate for the whole reader: frames arriving per second, counted
    // rather than derived from the gap between two of them.
    let rate = crate::stats::Rate::default();

    loop {
        // Read out before the select, because another arm borrows `pending`
        // mutably and the two cannot overlap.
        let stalled = pending.as_ref().map(|(_, _, deadline)| *deadline);
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
                // Re-requesting what is already playing cancels a swap that has
                // not landed, which is what un-pinning a rendition means.
                if name == current.get() {
                    pending = None;
                    // A rebuild survives: it is an open for this same rendition
                    // with a new decoder, so cancelling it here would throw away
                    // a decoder change because the user afterwards pinned the
                    // rendition it was already playing, with nothing left to
                    // retry it.
                    if opening.as_ref().is_some_and(|open| !open.survives_repin()) {
                        opening = None;
                    }
                    if reader.is_none() && opening.is_none() {
                        debug!("the only replacement was cancelled with nothing playing");
                        return;
                    }
                    continue;
                }
                let already = pending.as_ref().is_some_and(|(open, _, _)| *open == name)
                    || opening.as_ref().is_some_and(|open| open.name == name);
                if already {
                    continue;
                }
                debug!(rendition = %name, "opening replacement decoder");
                let task = spawn({
                    let broadcast = broadcast.clone();
                    let name = name.clone();
                    async move { spawn_reader(&broadcast, &name).await }
                });
                // Abort-on-drop, not a bare handle: dropping a `JoinHandle`
                // detaches, so a superseded open would run to completion and
                // keep a track subscription and a broadcast clone alive for as
                // long as the peer took to answer.
                opening = Some(Opening::switch(name, AbortOnDropHandle::new(task)));
            }

            // The policy changed under a track already playing: open the
            // rendition again so the decoder is built from it.
            changed = reopen.changed() => {
                if changed.is_err() {
                    return;
                }
                // A switch already in flight names the rendition to open, so a
                // decoder change during one does not undo it. Whatever that
                // switch had opened is dropped either way: it was built from the
                // policy this rebuild supersedes.
                let name = match (opening.take(), pending.take()) {
                    (Some(superseded), _) => superseded.name,
                    (None, Some((superseded, _, _))) => superseded,
                    (None, None) => current.get(),
                };
                debug!(rendition = %name, "rebuilding the decoder");
                let task = spawn({
                    let broadcast = broadcast.clone();
                    let name = name.clone();
                    async move { spawn_reader(&broadcast, &name).await }
                });
                opening = Some(Opening::rebuild(name, AbortOnDropHandle::new(task)));
            }

            // The replacement's decoder is open. Hold it until it produces a
            // frame, so the swap never shows an empty picture.
            opened = async { (&mut opening.as_mut().expect("guarded").task).await },
                if opening.is_some() =>
            {
                let Opening { name, .. } = opening.take().expect("guarded");
                match opened {
                    Ok(Ok(replacement)) => {
                        let deadline = tokio::time::Instant::now() + FIRST_FRAME_DEADLINE;
                        pending = Some((name, replacement, deadline));
                    }
                    Ok(Err(err)) => {
                        warn!(error = %err, rendition = %name, "replacement failed to open");
                        clear_request(&withdraw, &name);
                    }
                    Err(err) => {
                        warn!(error = %err, rendition = %name, "replacement open task failed");
                        clear_request(&withdraw, &name);
                    }
                }
                if reader.is_none() && pending.is_none() {
                    debug!("nothing left to decode after the replacement failed");
                    return;
                }
            }

            // The replacement's first frame: hand over.
            frame = async { pending.as_mut().expect("guarded").1.frames.recv().await },
                if pending.is_some() =>
            {
                let (name, replacement, _) = pending.take().expect("guarded");
                match frame {
                    Some(frame) => {
                        info!(rendition = %name, decoder = %replacement.decoder, "switched rendition");
                        context.stats.render.decoder.set(&replacement.decoder);
                        context.stats.render.rendition.set(&name);
                        decoder.set(replacement.decoder.clone()).ok();
                        reader = Some(replacement);
                        current.set(name).ok();
                        deliver(&frames, frame, &context, synced, &rate).await;
                    }
                    None => {
                        debug!(rendition = %name, "replacement ended before its first frame");
                        clear_request(&withdraw, &name);
                        if reader.is_none() && opening.is_none() {
                            debug!("nothing left to decode after the replacement ended");
                            return;
                        }
                    }
                }
            }

            // The replacement took too long over its first picture. Waiting on
            // it forever is the worse failure: a rendition request that never
            // lands leaves the adaptation loop with nothing to do for the rest
            // of the session, and both tracks subscribed, so a downgrade under
            // loss ends up carrying more than before it.
            // Inside an `async` block like its neighbours: `select!` evaluates
            // every branch's expression before it polls anything, so a bare
            // `expect` here would fire on the passes where the branch is
            // disabled.
            () = async { tokio::time::sleep_until(stalled.expect("guarded")).await },
                if stalled.is_some() =>
            {
                let (name, _, _) = pending.take().expect("guarded");
                warn!(
                    rendition = %name,
                    after = ?FIRST_FRAME_DEADLINE,
                    "the replacement decoder opened but produced no picture, giving it up",
                );
                clear_request(&withdraw, &name);
                if reader.is_none() && opening.is_none() {
                    debug!("nothing left to decode after the replacement stalled");
                    return;
                }
            }

            frame = async { reader.as_mut().expect("guarded").frames.recv().await },
                if reader.is_some() =>
            match frame {
                Some(frame) => deliver(&frames, frame, &context, synced, &rate).await,
                None => {
                    // A replacement already open takes over rather than being
                    // discarded: the incumbent ending is exactly when a
                    // downgrade is most likely to be in flight.
                    match pending.take() {
                        Some((name, replacement, _)) => {
                            info!(rendition = %name, "incumbent ended, promoting the replacement");
                            context.stats.render.decoder.set(&replacement.decoder);
                            context.stats.render.rendition.set(&name);
                            decoder.set(replacement.decoder.clone()).ok();
                            reader = Some(replacement);
                            current.set(name).ok();
                        }
                        None if opening.is_some() => {
                            debug!("incumbent ended while a replacement is opening");
                            // Nothing is playing until it lands. Its arm is
                            // disabled meanwhile, and every path that gives up
                            // on the open returns rather than parking here.
                            reader = None;
                        }
                        None => {
                            debug!("video decode ended");
                            return;
                        }
                    }
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
    rate: &crate::stats::Rate,
) {
    let pts = Duration::from_micros(frame.timestamp.as_micros() as u64);
    // The metric smooths whatever value it is handed, so it wants the
    // instantaneous rate, not a tick. Timed at arrival rather than from the
    // presentation timestamps, because a stall shows up here and not there.
    if let Some(rate) = rate.tick() {
        context.stats.render.fps.record(rate);
    }
    if synced {
        context.sync.received(pts);
        if !context.sync.wait_async(pts).await {
            return;
        }
    }
    frames.send(frame);
}

#[cfg(test)]
mod tests {
    /// The cadence is reported once per interval, as a rate over that interval
    /// rather than a count, and starts over after each report.
    #[test]
    fn the_decode_cadence_is_a_rate_per_interval() {
        let start = std::time::Instant::now();
        let mut cadence = super::Cadence {
            since: start,
            decoded: 0,
        };
        for _ in 0..149 {
            assert_eq!(cadence.decoded(start + super::CADENCE_EVERY / 2), None);
        }
        let fps = cadence
            .decoded(start + super::CADENCE_EVERY)
            .expect("the interval has passed");
        assert!(
            (fps - 30.0).abs() < 0.01,
            "150 frames in 5 s is 30 fps, got {fps}"
        );
        assert_eq!(cadence.decoded(start + super::CADENCE_EVERY), None);
    }

    use std::collections::BTreeSet;

    use moq_video::{Size, Surface, encode};
    use n0_watcher::Watcher as _;

    use super::*;
    use crate::{catalog::Catalog, playout::PlaybackPolicy};

    /// The test stream's geometry. Small, so encoding thirty pictures in a unit
    /// test costs nothing.
    const SIZE: Size = Size {
        width: 320,
        height: 240,
    };

    /// Pictures per test stream, and the keyframe interval within it. Three
    /// groups, so a break in the first still leaves two keyframes to recover on.
    const PICTURES: u64 = 30;
    const GOP: u32 = 10;

    /// The interval between pictures at the 30fps the stream is encoded for.
    const FRAME_MICROS: u64 = 33_333;

    /// Whatever a step of these tests can fail with, which is one error type
    /// per crate in the media stack and not worth enumerating.
    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    /// The producers a subscribed broadcast needs alive. Dropping any of them
    /// closes the broadcast under the subscriber.
    struct Published {
        _broadcast: moq_net::broadcast::Producer,
        _catalog: moq_mux::catalog::Producer<crate::catalog::IrohLiveExt>,
        _import: moq_mux::codec::h264::Import<crate::catalog::IrohLiveExt>,
        /// Cancels every decode task on drop, so the reader stops with it.
        _remote: RemoteBroadcast,
    }

    /// Encodes [`PICTURES`] pictures of a moving pattern as H.264 access units.
    ///
    /// Moving rather than flat so the inter-coded pictures carry residuals: a
    /// static picture codes to almost nothing and a decoder can conceal its way
    /// through a break in it without ever reporting one.
    fn encoded_stream() -> Vec<encode::Encoded> {
        let mut config = encode::Config::new(SIZE.width, SIZE.height, 30);
        config.kind = encode::Kind::Software;
        config.gop = GOP;
        let mut encoder = encode::Encoder::new(&config).expect("the software encoder always opens");

        let mut units = Vec::new();
        for index in 0..PICTURES {
            if index % u64::from(GOP) == 0 {
                encoder.keyframe();
            }
            let mut rgba = vec![0u8; SIZE.pixels() as usize * 4];
            for (offset, byte) in rgba.iter_mut().enumerate() {
                *byte = (offset / 4 + index as usize * 37) as u8;
            }
            let surface = Surface::rgba(&rgba, SIZE).expect("the buffer matches the size");
            let timestamp = moq_net::Timestamp::from_micros(index * FRAME_MICROS)
                .expect("the stream is a second long");
            units.extend(
                encoder
                    .encode(&moq_video::Frame::new(surface, timestamp))
                    .expect("the software encoder takes every frame"),
            );
        }
        units
    }

    /// The presentation time of `picture`, in the microseconds a timestamp
    /// carries.
    fn pts(picture: u64) -> u64 {
        picture * FRAME_MICROS
    }

    /// Feeds `units` into `import`, truncating the access unit whose
    /// presentation time is `broken` to a third of its bytes.
    ///
    /// A truncated access unit is what a decoder sees after a group is skipped
    /// under congestion: the slice data stops mid-picture, the reference chain
    /// breaks, and nothing decodes again until the next keyframe.
    ///
    /// The break is named by presentation time rather than by position in
    /// `units`, because the two are not the same thing. openh264's rate
    /// control drops a picture from this pattern, so the encoder emits
    /// twenty-nine access units for thirty pictures and every index past the
    /// drop names a later picture than it looks like. Indexing cost a day: the
    /// break landed one picture further on than intended, and the two
    /// platforms then disagreed about whether that picture was concealed.
    fn feed(
        import: &mut moq_mux::codec::h264::Import<crate::catalog::IrohLiveExt>,
        split: &mut moq_mux::codec::h264::Split,
        units: &[encode::Encoded],
        broken: Option<u64>,
    ) -> TestResult {
        for unit in units {
            let mut frames = split.decode(&unit.payload, unit.timestamp)?;
            frames.extend(split.flush(unit.timestamp)?);
            if broken == Some(unit.timestamp.as_micros() as u64) {
                for frame in &mut frames {
                    frame.payload = frame.payload.slice(..frame.payload.len() / 3);
                }
            }
            import.decode(frames)?;
        }
        Ok(())
    }

    /// Publishes a stream whose access unit at `broken` is truncated, and opens
    /// a track that reads it.
    ///
    /// The order here is the point of the helper. A player opens its decoder at
    /// the live edge, so an access unit published before the reader subscribed
    /// is one the reader never sees. Publishing the whole stream up front left
    /// this test asserting only that pictures arrived from a group the reader
    /// had started *after*, which an intact stream satisfies just as well: it
    /// passed without the decoder ever meeting the break.
    ///
    /// So the first access unit goes out on its own, because the catalog
    /// rendition is derived from its SPS and there is nothing to open without
    /// it, and everything else follows once the reader is subscribed.
    async fn publish(broken: Option<u64>) -> TestResult<(Reader, Vec<u64>, Vec<u64>, Published)> {
        let mut broadcast = moq_net::broadcast::Info::new().produce();
        let consumer = broadcast.consume();
        let catalog = moq_mux::catalog::Producer::with_catalog(&mut broadcast, Catalog::default())?;
        let track = broadcast.create_track("video", Some(catalog.track_info()))?;
        let mut import =
            moq_mux::codec::h264::Import::new(track, catalog.reserve(), Default::default())?;
        let mut split = moq_mux::codec::h264::Split::new();
        let units = encoded_stream();
        let published: Vec<u64> = units
            .iter()
            .map(|unit| unit.timestamp.as_micros() as u64)
            .collect();

        // The whole of the first group, so the only live edge a reader can open
        // at is inside it, whichever way its start policy resolves. Split by
        // presentation time rather than by count, for the reason `feed` gives.
        let boundary =
            units.partition_point(|unit| (unit.timestamp.as_micros() as u64) < pts(GOP.into()));
        feed(&mut import, &mut split, &units[..boundary], None)?;

        // The latency ceiling would otherwise have the container consumer skip
        // ahead of the break rather than deliver it.
        let policy = PlaybackPolicy {
            max_latency: Duration::from_secs(60),
            decoder: moq_video::decode::Kind::Software,
            ..PlaybackPolicy::unmanaged()
        };
        let remote = RemoteBroadcast::with_playback_policy("test", consumer, policy).await?;
        // The catalog travels on a track of its own, so the rendition that
        // first SPS filled in has not necessarily arrived with the snapshot
        // `with_playback_policy` returned on.
        let mut updates = remote.catalog_watcher();
        while remote.catalog().video().is_empty() {
            updates
                .updated()
                .await
                .map_err(|_| "the catalog ended before it carried a video rendition")?;
        }
        let mut reader = spawn_reader(&remote, "video").await?;

        // Read one picture before publishing any more, and hand it back so the
        // caller can count it. Subscribing is not enough: the reader's cursor
        // is only fixed once it has actually read, so publishing the rest
        // first left it opening at whatever the live edge had become by then.
        // On this machine that was still the first group and the test passed;
        // on a slower one it was the last, the reader never met the break, and
        // macOS CI said so.
        let first = tokio::time::timeout(Duration::from_secs(10), reader.frames.recv())
            .await
            .map_err(|_| "the reader produced no picture from the first group")?
            .ok_or("the video track ended before its first picture")?;
        let first = first.timestamp.as_micros() as u64;
        assert_eq!(first, 0, "the reader opened past the first picture");

        feed(&mut import, &mut split, &units[boundary..], broken)?;
        import.finish()?;

        Ok((
            reader,
            published,
            vec![first],
            Published {
                _broadcast: broadcast,
                _catalog: catalog,
                _import: import,
                _remote: remote,
            },
        ))
    }

    /// A task standing in for a decoder open that never finishes, which is the
    /// state an open is in when a repin can race it.
    fn open_in_flight() -> AbortOnDropHandle<Result<Reader, SubscribeError>> {
        AbortOnDropHandle::new(spawn(std::future::pending()))
    }

    /// Regression: the reopen arm built its `Opening` with `rebuild: false`,
    /// so the guard written to keep a decoder change alive across a repin
    /// never fired. Pick Decoder = vaapi, then pin the rendition already
    /// playing while the new decoder was coming up, and the vaapi rebuild was
    /// gone with nothing left to retry it.
    #[tokio::test]
    async fn a_rebuild_survives_repinning_the_current_rendition() {
        let rebuild = Opening::rebuild("video".into(), open_in_flight());
        assert!(rebuild.survives_repin());
    }

    /// The other half: un-pinning is what a repin means for a switch, so a
    /// switch does not survive it.
    #[tokio::test]
    async fn a_switch_is_cancelled_by_repinning_the_current_rendition() {
        let switch = Opening::switch("video-360p".into(), open_in_flight());
        assert!(!switch.survives_repin());
    }

    /// The presentation time of every picture the reader decodes, in order.
    ///
    /// Read from the reader's own channel rather than through a `VideoTrack`.
    /// The track hands pictures over through a latest-wins slot, which drops
    /// whatever a slow consumer did not take: on this machine that cost one
    /// picture in thirty and on macOS CI it cost twenty-eight of them, so no
    /// assertion about *which* pictures decoded can be made through it. This
    /// channel is bounded and lossless, and the reader is what these two tests
    /// are about.
    async fn read_all(reader: &mut Reader) -> Vec<u64> {
        let mut seen = Vec::new();
        while let Some(frame) = reader.frames.recv().await {
            seen.push(frame.timestamp.as_micros() as u64);
        }
        seen
    }

    /// The picture whose access unit this pair truncates.
    ///
    /// Inside the second group: the first is published before anyone
    /// subscribes, so that the live edge a reader opens at is inside it, and a
    /// break there would be one the reader started after. The keyframe opening
    /// the third group is what repairs the damage.
    const BROKEN: u64 = GOP as u64 + 3;

    /// Publishes one stream and reads it to the end.
    ///
    /// Returns the pictures the encoder produced and the pictures the reader
    /// delivered, so a caller can compare the two rather than assume they
    /// match: the encoder does not emit one access unit per input picture.
    async fn read_stream(broken: Option<u64>) -> TestResult<(BTreeSet<u64>, BTreeSet<u64>)> {
        let (mut reader, published, mut seen, _published) = publish(broken).await?;
        seen.extend(read_all(&mut reader).await);
        Ok((published.into_iter().collect(), seen.into_iter().collect()))
    }

    /// Regression: one access unit the decoder refuses used to end the reader,
    /// which dropped the decoder and the subscription with it. A player showed
    /// a picture for a fraction of a second and then froze for good, with one
    /// warning in the log and nothing after it.
    ///
    /// What this covers is the reader carrying on through a break in the
    /// bitstream and delivering the pictures after the next keyframe. It does
    /// not reach [`DecodeFailures`]: openh264 absorbs a truncated access unit
    /// and the ones that lost their reference to it by producing no picture,
    /// rather than by returning an error, so `read` never fails here. Filling
    /// the same bytes with garbage instead is weaker still, because the decoder
    /// conceals it and every picture arrives. The give-up threshold and the run
    /// counter are covered by the unit tests below.
    #[tokio::test]
    async fn a_broken_access_unit_does_not_end_playback() -> TestResult {
        let (encoded, intact) = read_stream(None).await?;
        let (_, broken) = read_stream(Some(pts(BROKEN))).await?;

        // What the break cost, measured rather than predicted. How many
        // pictures a decoder conceals before giving up on a reference chain is
        // its own business and the platforms disagree: macOS hands over the
        // truncated picture and Linux drops it. Both agree on where the damage
        // stops, which is the claim worth making.
        let lost: Vec<u64> = intact.difference(&broken).copied().collect();
        let group = pts(BROKEN)..pts(u64::from(GOP) * 2);

        assert!(
            broken.iter().any(|&picture| picture < group.start),
            "the reader started after the break, so nothing here was exercised: got {broken:?}",
        );
        assert!(
            !lost.is_empty(),
            "the truncated access unit cost no picture, so there was nothing to \
             recover from: got {broken:?}",
        );
        for picture in &lost {
            assert!(
                group.contains(picture),
                "picture {picture} was lost outside the group holding the break, \
                 so the damage is not the break: lost {lost:?} of {encoded:?}",
            );
        }
        assert!(
            broken.contains(&pts(u64::from(GOP) * 2)),
            "the keyframe after the break never arrived: got {broken:?}",
        );
        assert!(
            broken.contains(encoded.last().expect("the encoder produced pictures")),
            "the reader stopped before the end: got {broken:?}",
        );
        Ok(())
    }

    /// The control: with nothing broken the reader delivers every picture the
    /// encoder produced.
    ///
    /// Exact rather than approximate, and it is what licenses the comparison
    /// above. `encoded` is what the encoder emitted, which is not one access
    /// unit per input picture: openh264 drops one under its own rate control,
    /// and reading that as damage is the mistake this pair is built to avoid.
    #[tokio::test]
    async fn an_intact_stream_plays_to_the_end() -> TestResult {
        let (encoded, seen) = read_stream(None).await?;
        assert_eq!(
            seen, encoded,
            "an intact stream delivers every picture that was encoded",
        );
        Ok(())
    }

    #[test]
    fn only_a_decode_failure_is_worth_reading_past() {
        // A retry cannot fix a track the transport dropped, and the reader would
        // spin through its whole allowance before saying so.
        assert_eq!(
            ReadFailure::from(&moq_video::Error::Net(moq_net::Error::Cancel)),
            ReadFailure::Track,
        );
        assert_eq!(
            ReadFailure::from(&moq_video::Error::Codec(
                n0_error::anyerr!("bad picture").into()
            )),
            ReadFailure::Picture,
        );
    }

    #[test]
    fn a_run_of_failures_ends_the_reader_only_once_no_keyframe_can_help() {
        let mut failures = DecodeFailures::default();
        for _ in 1..MAX_CONSECUTIVE_DECODE_FAILURES {
            assert_eq!(failures.failed(), AfterFailure::Skip);
        }
        assert_eq!(failures.failed(), AfterFailure::GiveUp);
    }

    #[test]
    fn a_decoded_picture_ends_the_run() {
        let mut failures = DecodeFailures::default();
        for _ in 0..MAX_CONSECUTIVE_DECODE_FAILURES - 1 {
            failures.failed();
        }
        assert_eq!(failures.decoded(), MAX_CONSECUTIVE_DECODE_FAILURES - 1);
        assert_eq!(failures.len(), 0);
        assert_eq!(
            failures.failed(),
            AfterFailure::Skip,
            "the run has to start over, or a stream that recovers every keyframe \
             still accumulates its way to a give-up",
        );
    }

    #[test]
    fn withdrawing_a_request_leaves_a_newer_one_alone() {
        let (requested, _rx) = watch::channel(Some("video-720p".to_string()));
        clear_request(&requested, "video-1080p");
        assert_eq!(
            requested.borrow().as_deref(),
            Some("video-720p"),
            "a request placed after the failed one has to survive",
        );

        clear_request(&requested, "video-720p");
        assert_eq!(requested.borrow().as_deref(), None);
    }

    #[test]
    fn withdrawing_a_request_does_not_wake_the_supervisor() {
        // The withdrawal is not itself a request, and waking the supervisor for
        // it would only make it re-read a value it had just written.
        let (requested, mut watcher) = watch::channel(Some("video-720p".to_string()));
        watcher.borrow_and_update();
        clear_request(&requested, "video-720p");
        assert!(!watcher.has_changed().expect("the sender is still alive"));
    }
}

#[cfg(test)]
mod failure_share_tests {
    use super::{DecodeFailures, FAILURE_WINDOW};

    /// A decoder taking one picture per group never builds a run long enough
    /// to give up on, so the run counter alone cannot see it. The share can.
    #[test]
    fn one_picture_a_group_is_reported() {
        let mut failures = DecodeFailures::default();
        failures.window.started = std::time::Instant::now() - FAILURE_WINDOW;
        // Sixty access units a group, one of which decodes.
        for _ in 0..59 {
            failures.failed();
        }
        failures.decoded();
        let share = failures
            .mostly_failing()
            .expect("one in sixty is well under the threshold");
        assert!(share < 0.05, "share was {share}");
    }

    /// A stream that recovers from one skipped group is far above the
    /// threshold inside a window, so it says nothing.
    #[test]
    fn an_ordinary_recovery_says_nothing() {
        let mut failures = DecodeFailures::default();
        failures.window.started = std::time::Instant::now() - FAILURE_WINDOW;
        for _ in 0..5 {
            failures.failed();
        }
        for _ in 0..295 {
            failures.decoded();
        }
        assert!(failures.mostly_failing().is_none());
    }

    /// Reported once, so a track that stays broken does not log every window.
    #[test]
    fn the_share_is_reported_once() {
        let mut failures = DecodeFailures::default();
        failures.window.started = std::time::Instant::now() - FAILURE_WINDOW;
        for _ in 0..59 {
            failures.failed();
        }
        failures.decoded();
        assert!(failures.mostly_failing().is_some());
        failures.window.started = std::time::Instant::now() - FAILURE_WINDOW;
        for _ in 0..59 {
            failures.failed();
        }
        failures.decoded();
        assert!(failures.mostly_failing().is_none());
    }
}
