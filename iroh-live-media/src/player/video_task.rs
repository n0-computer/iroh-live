//! The player's video task: decoders, the rendition swap, and pacing.
//!
//! One decoder plays at a time. A switch or a decoder change opens a
//! replacement beside it and hands over once the replacement has caught up
//! with the picture on screen. The state machine for that lives in
//! [`switch`](super::switch), and this module drives it with real decoders.
//!
//! Each decoder is read by its own task, not from a `select!` arm.
//! `moq_video::decode::Consumer` reads through a `Sink`, whose `read` is not
//! cancel-safe: dropping the future poisons the decoder. So the read runs in a
//! task and reaches the supervisor over a channel.
//!
//! An access unit the decoder refuses is skipped. After a skipped group or a
//! truncated access unit, a decoder refuses every picture until the next
//! keyframe. Stopping on the first refusal would turn that break into a
//! permanent freeze. The reader gives up only after
//! `MAX_CONSECUTIVE_DECODE_FAILURES` refusals in a row.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};

use n0_future::{
    FutureExt,
    boxed::BoxFuture,
    task::{AbortOnDropHandle, spawn},
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, error_span, info, warn};

use super::{
    Abandon, Controls, PlaybackRecorder, PlayerStatus, PlayoutClock, SwitchEvent,
    select::{DecodeSettings, Desired, Failure, Report},
    switch::{Abandoned, Outcome, Switcher, Target, Verdict},
};
use crate::{
    SlotState,
    error::Error,
    frames::FrameSlot,
    publish::status::send_if_changed,
    stats::{FrameTiming, MediaKind, RateMeter, Smoothed, VideoPlaybackStats},
};

/// How many decoded frames a reader may run ahead of the supervisor.
///
/// Two frames absorb a scheduling hiccup. More would only add latency.
const READ_AHEAD: usize = 2;

/// How many access units in a row may fail to decode before the reader stops.
///
/// A decoder that lost its reference chain refuses every picture until the
/// next keyframe, so the limit must span a keyframe interval. Publishers key
/// every two seconds by default, which is 120 access units at 60fps. Three
/// hundred covers that with room, and still reports a dead decoder within
/// about ten seconds.
const MAX_CONSECUTIVE_DECODE_FAILURES: u32 = 300;

/// How often the decode cadence is logged.
///
/// A starving picture looks fine in every other log line: the transport logs
/// what arrived, and the decoder is silent unless it fails. The decoded frame
/// rate shows what the viewer saw.
const CADENCE_EVERY: Duration = Duration::from_secs(5);

/// The task opening a replacement decoder.
type OpenTask = AbortOnDropHandle<Result<Reader, Error>>;

/// The supervisor's state machine, over real decoders.
type VideoSwitcher = Switcher<Reader, OpenTask>;

/// The video task's inputs.
pub(crate) struct Inputs {
    pub desired: watch::Receiver<Option<Desired>>,
    pub frames: FrameSlot,
    pub controls: Arc<Controls>,
    pub status: watch::Sender<PlayerStatus>,
    pub events: broadcast::Sender<SwitchEvent>,
    /// Where failed or ended decoders are reported, for the selector.
    pub reports: mpsc::Sender<Report>,
    /// The target on screen, for the selector.
    pub playing: watch::Sender<Option<Target>>,
    pub clock: PlayoutClock,
    pub stats: PlaybackRecorder,
    /// How long a replacement decoder has to take over.
    pub switch_deadline: Duration,
    pub shutdown: CancellationToken,
}

/// Something one of the decoders did.
enum Event {
    /// The replacement's open task finished.
    Opened(Result<Result<Reader, Error>, n0_future::task::JoinError>),
    /// The replacement decoded a picture, or its track ended.
    Replacement(Option<moq_video::Frame>),
    /// The incumbent decoded a picture, or its track ended.
    Incumbent(Option<moq_video::Frame>),
}

/// A picture waiting for the playout clock to say it is due.
struct Delivery {
    frame: moq_video::Frame,
    /// When the frame came out of its decoder, for the timeline.
    decoded: Instant,
    due: BoxFuture<bool>,
}

/// Forwards frames to the player's output and swaps decoders on request.
///
/// Every await sits in the `select!` itself, never in an arm body. Pacing is a
/// future the loop keeps across iterations, so a request that arrives while a
/// picture waits for the clock is handled at once.
pub(crate) async fn run(inputs: Inputs) {
    let Inputs {
        mut desired,
        frames,
        controls,
        status,
        events,
        reports,
        playing,
        clock,
        stats,
        switch_deadline,
        shutdown,
    } = inputs;
    let mut switcher = VideoSwitcher::new(switch_deadline);
    let mut delivery: Option<Delivery> = None;
    // The shown-frame rate.
    let mut shown = RateMeter::default();

    loop {
        let deadline = switcher.deadline();
        let delivering = delivery.is_some();
        // Why a decoder's track stopped, read before the switcher drops the
        // reader.
        let mut replacement_failure: Option<Arc<Error>> = None;
        let mut incumbent_end: Option<(Target, Option<Arc<Error>>)> = None;
        let outcome: Outcome<Error> = tokio::select! {
            biased;

            () = shutdown.cancelled() => {
                debug!("video stopped");
                return;
            }

            changed = desired.changed() => {
                if changed.is_err() {
                    // The selector stops when the broadcast closes, so the
                    // video has ended.
                    send_if_changed(&status, |status| {
                        let video = match &status.video {
                            SlotState::Running | SlotState::Starting => SlotState::Ended,
                            other => other.clone(),
                        };
                        status.clear_video(video);
                    });
                    return;
                }
                let next = desired.borrow_and_update().clone();
                match next {
                    Some(next) => {
                        let Desired { target, settings, config, step_down } = next;
                        let outcome = switcher.request(target, tokio::time::Instant::now(), |_, target| {
                            open_replacement(target, &settings, &config, &stats)
                        });
                        if step_down && switcher.switching_to().is_some() && switcher.current().is_some() {
                            info!(
                                from = ?switcher.current().map(|target| &target.rendition),
                                to = ?switcher.switching_to().map(|target| &target.rendition),
                                "stepping down: letting go of the rendition on screen so the next can arrive",
                            );
                            switcher.release_incumbent();
                        }
                        outcome
                    }
                    None => {
                        // Video is off or nothing is left to play. Drop both
                        // decoders and keep the last picture up.
                        switcher = VideoSwitcher::new(switch_deadline);
                        delivery = None;
                        stats.video.update(|video| *video = None);
                        send_if_changed(&status, |status| {
                            // The selector sets Off. Anything else means the
                            // catalog has no video left.
                            let video = match status.video {
                                SlotState::Off => SlotState::Off,
                                _ => SlotState::Ended,
                            };
                            status.clear_video(video);
                        });
                        Outcome::Idle
                    }
                }
            }

            () = async { tokio::time::sleep_until(deadline.expect("guarded")).await },
                if deadline.is_some() =>
            {
                switcher.expire(tokio::time::Instant::now())
            }

            due = async { delivery.as_mut().expect("guarded").due.as_mut().await },
                if delivering =>
            {
                let Delivery { frame, decoded, .. } = delivery.take().expect("guarded");
                if due {
                    stats.video_timeline.push(FrameTiming {
                        kind: MediaKind::Video,
                        pts: frame.timestamp.into(),
                        decoded,
                        presented: Instant::now(),
                    });
                    frames.send(Arc::new(frame));
                }
                Outcome::Idle
            }

            event = next_event(&mut switcher, delivering) => match event {
                Event::Opened(result) => {
                    let generation = switcher.replacement_generation().unwrap_or_default();
                    let result = result.unwrap_or_else(|err| {
                        Err(Error::decoder_msg(format!(
                            "the decoder open task failed: {err}"
                        )))
                    });
                    switcher.opened(generation, result)
                }
                Event::Replacement(Some(frame)) => {
                    match switcher.replacement_frame(frame.timestamp.into(), tokio::time::Instant::now()) {
                        (Verdict::Promote, outcome) => {
                            // The incumbent's pending picture is older, so it
                            // is replaced.
                            delivery = Some(pace(frame, &mut shown, &clock, &controls, &stats));
                            outcome
                        }
                        (Verdict::Discard, outcome) => outcome,
                    }
                }
                Event::Replacement(None) => {
                    replacement_failure = switcher
                        .warming_mut()
                        .and_then(|reader| reader.failure.get().cloned());
                    switcher.replacement_ended()
                }
                Event::Incumbent(Some(frame)) => {
                    switcher.incumbent_frame(frame.timestamp.into());
                    delivery = Some(pace(frame, &mut shown, &clock, &controls, &stats));
                    Outcome::Idle
                }
                Event::Incumbent(None) => {
                    let failure = switcher
                        .incumbent_mut()
                        .and_then(|reader| reader.failure.get().cloned());
                    incumbent_end = switcher.current().cloned().map(|target| (target, failure));
                    switcher.incumbent_ended()
                }
            },
        };

        // Written before any event goes out, so a waiter that reads the status
        // on an event sees the switch that follows it.
        let switching = switcher
            .switching_to()
            .map(|target| target.rendition.clone());
        let on_screen = switcher.current().cloned();
        playing.send_if_modified(|playing| {
            let changed = *playing != on_screen;
            if changed {
                playing.clone_from(&on_screen);
            }
            changed
        });
        send_if_changed(&status, |status| status.switching_to = switching);
        match outcome {
            Outcome::Idle => {}
            Outcome::Promoted(target) => {
                let decoder = switcher
                    .incumbent_mut()
                    .map(|reader| {
                        reader.on_screen.store(true, Ordering::Relaxed);
                        reader.decoder.clone()
                    })
                    .unwrap_or_default();
                info!(rendition = %target.rendition, %decoder, "rendition on screen");
                stats.video.update(|video| {
                    let video = video.get_or_insert_with(VideoPlaybackStats::default);
                    video.rendition = target.rendition.clone();
                    video.decoder = decoder.clone();
                });
                send_if_changed(&status, |status| {
                    status.video = SlotState::Running;
                    status.failed_rendition = None;
                    status.rendition = Some(target.rendition.clone());
                    status.decoder = Some(decoder.clone());
                });
                let _ = events.send(SwitchEvent::Landed(target.rendition));
            }
            // A replacement whose track ended cleanly is not a broken decoder.
            // The publisher replaced the track or the route changed, and the
            // selector asks for it again.
            Outcome::Abandoned(target, Abandoned::Ended) if replacement_failure.is_none() => {
                debug!(rendition = %target.rendition, "replacement's track ended before it took over");
                if switcher.current().is_none() {
                    send_if_changed(&status, |status| status.video = SlotState::Starting);
                }
                let _ = reports.try_send(Report::Ended(target));
            }
            Outcome::Abandoned(target, reason) => {
                let rendition = target.rendition.clone();
                let playing = switcher.current().cloned();
                let mut exclude = true;
                let abandon = match reason {
                    Abandoned::Superseded => {
                        debug!(%rendition, "replacement superseded");
                        Abandon::Superseded
                    }
                    Abandoned::Withdrawn => {
                        debug!(%rendition, "replacement withdrawn");
                        Abandon::Withdrawn
                    }
                    Abandoned::OpenFailed(err) => {
                        warn!(error = %err, %rendition, "replacement decoder failed to open");
                        Abandon::Failed(Arc::new(err))
                    }
                    Abandoned::Ended => {
                        let failure = replacement_failure
                            .take()
                            .unwrap_or_else(|| Arc::new(n0_error::e!(Error::Closed)));
                        warn!(error = %failure, %rendition, "replacement decoder gave up before it took over");
                        Abandon::Failed(failure)
                    }
                    Abandoned::TimedOut => {
                        warn!(%rendition, after = ?switch_deadline, "replacement did not take over in time");
                        // With nothing on screen, a slow link is no reason to
                        // exclude the rendition, so it is asked for again.
                        exclude = playing.is_some();
                        Abandon::Failed(Arc::new(Error::decoder_msg(format!(
                            "the decoder for {rendition} did not produce a picture within {}s",
                            switch_deadline.as_secs()
                        ))))
                    }
                };
                if let Abandon::Failed(err) = &abandon {
                    let config_only = playing
                        .as_ref()
                        .is_some_and(|playing| playing.rendition == target.rendition);
                    send_if_changed(&status, |status| {
                        status.switch_error = Some(err.clone());
                        // Nothing on screen and nothing on its way: the video
                        // has failed until the selector finds something to try.
                        if playing.is_none() && status.switching_to.is_none() {
                            status.clear_video(SlotState::Failed(err.clone()));
                            status.failed_rendition = Some(rendition.clone());
                        }
                    });
                    // A full channel already holds a failure, so dropping
                    // this one loses nothing.
                    let _ = reports.try_send(Report::Failed(Failure {
                        target: target.clone(),
                        config_only,
                        exclude,
                    }));
                }
                let _ = events.send(SwitchEvent::Abandoned(rendition, abandon));
            }
            Outcome::Ended => {
                delivery = None;
                stats.video.update(|video| *video = None);
                match incumbent_end {
                    // The reader gave up on its track. Report it like a failed
                    // switch, so the selector backs off and tries another
                    // rendition.
                    Some((target, Some(err))) => {
                        warn!(error = %err, rendition = %target.rendition, "video failed");
                        send_if_changed(&status, |status| {
                            status.clear_video(SlotState::Failed(err.clone()));
                            status.failed_rendition = Some(target.rendition.clone());
                            status.switch_error = Some(err.clone());
                        });
                        let _ = reports.try_send(Report::Failed(Failure {
                            target,
                            config_only: false,
                            exclude: true,
                        }));
                    }
                    // A clean end: the publisher replaced or withdrew the
                    // video, or the route changed. The player waits for what
                    // follows. Only the broadcast closing or a catalog without
                    // video ends the video.
                    Some((target, None)) => {
                        info!(rendition = %target.rendition, "video track ended, waiting for what follows");
                        send_if_changed(&status, |status| status.clear_video(SlotState::Starting));
                        let _ = reports.try_send(Report::Ended(target));
                    }
                    None => {
                        send_if_changed(&status, |status| status.clear_video(SlotState::Starting))
                    }
                }
            }
        }
    }
}

/// Starts opening a decoder for `target`.
fn open_replacement(
    target: &Target,
    settings: &DecodeSettings,
    config: &hang::catalog::VideoConfig,
    stats: &PlaybackRecorder,
) -> OpenTask {
    debug!(rendition = %target.rendition, "opening a decoder");
    let settings = settings.clone();
    let config = config.clone();
    let name = target.rendition.clone();
    let stats = stats.clone();
    // Aborted on drop, so a superseded open does not hold a track subscription
    // until the peer answers.
    AbortOnDropHandle::new(spawn(async move {
        spawn_reader(&settings, &config, &name, stats).await
    }))
}

/// Waits for the next event from any decoder.
///
/// Polls the whole switcher in one future, so the `select!` holds a single
/// borrow of it. The incumbent is not read while a picture waits for the
/// clock, and its bounded channel then keeps the decoder from running ahead of
/// playout.
async fn next_event(switcher: &mut VideoSwitcher, delivering: bool) -> Event {
    std::future::poll_fn(|cx| {
        if let Some(task) = switcher.opening_mut()
            && let Poll::Ready(result) = Pin::new(task).poll(cx)
        {
            return Poll::Ready(Event::Opened(result));
        }
        if let Some(reader) = switcher.warming_mut()
            && let Poll::Ready(frame) = reader.frames.poll_recv(cx)
        {
            return Poll::Ready(Event::Replacement(frame));
        }
        if !delivering
            && let Some(reader) = switcher.incumbent_mut()
            && let Poll::Ready(frame) = reader.frames.poll_recv(cx)
        {
            return Poll::Ready(Event::Incumbent(frame));
        }
        Poll::Pending
    })
    .await
}

/// Starts pacing one frame against the playout clock.
///
/// Counts the frame in `shown`. Reads the latency mode on every frame, so a
/// change takes effect at once.
fn pace(
    frame: moq_video::Frame,
    shown: &mut RateMeter,
    clock: &PlayoutClock,
    controls: &Controls,
    stats: &PlaybackRecorder,
) -> Delivery {
    let pts = frame.timestamp.into();
    let size = frame.size();
    let rate = shown.tick(0);
    stats.video.update(|video| {
        let video = video.get_or_insert_with(VideoPlaybackStats::default);
        video.frames += 1;
        video.size = Some(size);
        if let Some((fps, _)) = rate {
            video.fps = Some(fps.round() as u32);
        }
    });
    let paced = controls.latency.borrow().paced();
    let due = if paced {
        clock.received(pts);
        let clock = clock.clone();
        async move { clock.wait_async(pts).await }.boxed()
    } else {
        std::future::ready(true).boxed()
    };
    Delivery {
        frame,
        decoded: Instant::now(),
        due,
    }
}

/// One decoder plus the task reading it.
struct Reader {
    /// The name of the decoder backend, for status and logs.
    decoder: String,
    frames: mpsc::Receiver<moq_video::Frame>,
    /// Why the reader gave up on its track, unset after a clean end.
    failure: Arc<OnceLock<Arc<Error>>>,
    /// Set once this reader's pictures are on screen.
    ///
    /// Only that reader writes the playback stats, so a warming replacement
    /// does not overwrite them.
    on_screen: Arc<AtomicBool>,
    /// Dropping this aborts the read loop, which drops the decoder with it.
    _task: AbortOnDropHandle<()>,
}

/// Returns the decode options for a player's settings.
fn decode_options(settings: &DecodeSettings) -> moq_video::decode::Options {
    let mut options = moq_video::decode::Options::new();
    options.decoder.kind = settings.decoder.clone();
    // Frames stay on the GPU for the renderer to import. A frame converts to
    // CPU pixels on demand.
    options.decoder.output = moq_video::Output::Native;
    options.max_age = settings.max_age;
    // A player wants the live edge. Without this, a rebuilt decoder or one
    // reopened on an earlier rendition decodes the whole cached backlog first.
    options.start = moq_video::decode::Start::Latest;
    options
}

/// Subscribes to a rendition, opens its decoder, and starts reading it.
///
/// Returns once the decoder is open, so the caller can tell an unusable
/// rendition from a slow one.
async fn spawn_reader(
    settings: &DecodeSettings,
    config: &hang::catalog::VideoConfig,
    rendition: &str,
    stats: PlaybackRecorder,
) -> Result<Reader, Error> {
    let options = decode_options(settings);
    let mut consumer =
        moq_video::decode::Consumer::new(&settings.consumer, config, rendition, options)
            .await
            .map_err(decode_error)?;
    let decoder = consumer.name().to_string();
    info!(rendition, decoder = %decoder, "video decoding");

    let (tx, frames) = mpsc::channel(READ_AHEAD);
    let name = rendition.to_string();
    let on_screen = Arc::new(AtomicBool::new(false));
    let writes = on_screen.clone();
    let failure = Arc::new(OnceLock::new());
    let gave_up = failure.clone();
    let task = spawn(
        async move {
            // Access units refused since the last picture.
            let mut failures = 0u32;
            let mut timing = Smoothed::default();
            let mut cadence = RateMeter::over(CADENCE_EVERY);
            loop {
                let started = std::time::Instant::now();
                match consumer.read().await {
                    Ok(Some(frame)) => {
                        if failures > 0 {
                            info!(skipped = failures, "video decoding recovered");
                            failures = 0;
                        }
                        if let Some((fps, _)) = cadence.tick(0) {
                            debug!(fps = format_args!("{fps:.1}"), "video decoding cadence");
                        }
                        // Includes the transport read, since both happen
                        // inside one `read`.
                        let took = timing.record(started.elapsed());
                        if writes.load(Ordering::Relaxed) {
                            stats.video.update(|video| {
                                if let Some(video) = video.as_mut() {
                                    video.decode_time = Some(took);
                                }
                            });
                        }
                        if tx.send(frame).await.is_err() {
                            debug!("nobody is reading this rendition any more");
                            return;
                        }
                    }
                    Ok(None) => {
                        debug!("video track ended");
                        return;
                    }
                    // Only `Codec` is about a single picture, and the next
                    // keyframe repairs it. The other errors are about the
                    // track and repeat on every read.
                    Err(err) if !matches!(err, moq_video::Error::Codec(_)) => {
                        warn!(error = %err, "video track failed");
                        let _ = gave_up.set(Arc::new(decode_error(err)));
                        return;
                    }
                    Err(err) => {
                        if writes.load(Ordering::Relaxed) {
                            stats.video.update(|video| {
                                if let Some(video) = video.as_mut() {
                                    video.skipped += 1;
                                }
                            });
                        }
                        failures += 1;
                        if failures >= MAX_CONSECUTIVE_DECODE_FAILURES {
                            error!(error = %err, failures, "no access unit decoded for a long time, giving up");
                            let _ = gave_up.set(Arc::new(Error::decoder_msg(format!(
                                "the decoder refused {failures} access units in a row: {err}"
                            ))));
                            return;
                        }
                        // Warns once per run, since a lost reference chain
                        // fails every picture until the next keyframe.
                        if failures == 1 {
                            warn!(error = %err, "video decode failed, skipping the access unit");
                        } else {
                            debug!(error = %err, failures, "video decode failed");
                        }
                    }
                }
            }
        }
        .instrument(error_span!("decode", rendition = %name)),
    );

    Ok(Reader {
        decoder,
        frames,
        failure,
        on_screen,
        _task: AbortOnDropHandle::new(task),
    })
}

/// Converts a decoder error into the crate's error.
fn decode_error(err: moq_video::Error) -> Error {
    match err {
        moq_video::Error::NoDecoder(tried) => n0_error::e!(Error::NoDecoder { codec: tried }),
        moq_video::Error::UnknownDecoder { codec, .. } => n0_error::e!(Error::NoDecoder {
            codec: format!("{codec:?}")
        }),
        moq_video::Error::UnsupportedCodec(codec) => n0_error::e!(Error::NoDecoder { codec }),
        moq_video::Error::Net(err) => Error::broadcast(err),
        other => Error::decoder(other),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use moq_video::{Size, Surface, encode};

    use super::{super::PlaybackRecorder, *};
    use crate::RemoteBroadcast;

    /// The test stream's picture size, small to keep encoding fast.
    const SIZE: Size = Size {
        width: 320,
        height: 240,
    };

    /// Pictures per test stream: three groups of [`GOP`].
    const PICTURES: u64 = 30;
    /// The keyframe interval of the test stream.
    const GOP: u32 = 10;

    /// The interval between pictures at the 30fps the stream is encoded for.
    const FRAME_MICROS: u64 = 33_333;

    /// The result of a test step, boxing whatever error the media stack returns.
    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    /// The producers that keep a subscribed broadcast open.
    struct Published {
        _broadcast: moq_net::broadcast::Producer,
        _catalog: moq_mux::catalog::Producer,
        _import: moq_mux::codec::h264::Import,
        /// Cancels every decode task on drop, so the reader stops with it.
        _remote: RemoteBroadcast,
    }

    /// Encodes [`PICTURES`] pictures of a moving pattern as H.264 access units.
    ///
    /// The pattern moves so inter-coded pictures carry data. A decoder can
    /// conceal a break in a static picture, and the test would see no loss.
    fn encoded_stream() -> Vec<encode::Encoded> {
        let framerate = moq_video::Rate::new(30, 1).expect("a valid frame rate");
        let mut config = encode::Config::new(SIZE.width, SIZE.height, framerate);
        config.kind = encode::Kind::Software;
        config.gop = encode::Gop::Keyframe { interval: GOP };
        let mut encoder = encode::Encoder::new(&config).expect("the software encoder always opens");

        let mut units = Vec::new();
        for index in 0..PICTURES {
            if index % u64::from(GOP) == 0 {
                encoder.cut().expect("the software encoder can cut a group");
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

    /// Returns the presentation time of `picture`, in microseconds.
    fn pts(picture: u64) -> u64 {
        picture * FRAME_MICROS
    }

    /// Feeds `units` into `import`, cutting the access unit at `broken` to a third.
    ///
    /// A truncated access unit breaks the reference chain until the next
    /// keyframe, as a group skipped under congestion does.
    ///
    /// The break is named by presentation time, not by index. openh264's rate
    /// control drops a picture from this pattern, so an index past the drop
    /// names a later picture than it seems to.
    fn feed(
        import: &mut moq_mux::codec::h264::Import,
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

    /// Publishes a stream with a break at `broken`, and opens a reader on it.
    ///
    /// A reader opens at the live edge and never sees what was published before
    /// it read. So the first group goes out alone, the reader reads its first
    /// picture, and only then does the rest follow, break included. The first
    /// group also carries the SPS the catalog rendition comes from.
    async fn publish(broken: Option<u64>) -> TestResult<(Reader, Vec<u64>, Vec<u64>, Published)> {
        let mut broadcast = moq_net::broadcast::Info::new().produce();
        let consumer = broadcast.consume();
        let catalog = moq_mux::catalog::Producer::new(
            &mut broadcast,
            moq_mux::catalog::Config::default().with_catalog(hang::catalog::Catalog::default()),
        )?;
        let track = broadcast.create_track(
            "video",
            Some(catalog.track_info(hang::catalog::PRIORITY.video)),
        )?;
        let mut import =
            moq_mux::codec::h264::Import::new(track, catalog.reserve(), Default::default())?;
        let mut split = moq_mux::codec::h264::Split::new();
        let units = encoded_stream();
        let published: Vec<u64> = units
            .iter()
            .map(|unit| unit.timestamp.as_micros() as u64)
            .collect();

        // Publish only the first group, so the reader opens inside it. Split by
        // presentation time, as `feed` explains.
        let boundary =
            units.partition_point(|unit| (unit.timestamp.as_micros() as u64) < pts(GOP.into()));
        feed(&mut import, &mut split, &units[..boundary], None)?;

        // A long max age, so the consumer delivers the break instead of
        // skipping past it.
        let settings = DecodeSettings {
            consumer: consumer.clone(),
            decoder: moq_video::decode::Kind::Software,
            max_age: Duration::from_secs(60),
        };
        let remote = RemoteBroadcast::from_moq(consumer);
        // The catalog has its own track, so the first snapshot may not have
        // the rendition yet.
        let mut snapshots = remote.catalog();
        let config = snapshots
            .wait_for(|known| {
                known
                    .as_ref()
                    .is_some_and(|known| known.video.renditions.contains_key("video"))
            })
            .await
            .map_err(|_| "the catalog ended before it carried a video rendition")?
            .as_ref()
            .and_then(|known| known.video.renditions.get("video").cloned())
            .expect("waited for it");
        let mut reader =
            spawn_reader(&settings, &config, "video", PlaybackRecorder::default()).await?;

        // Read one picture before publishing the rest, and hand it back so the
        // caller counts it. The reader's position is fixed only once it has
        // read, not when it subscribes.
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

    /// Returns the presentation time of every picture the reader decodes.
    ///
    /// Reads the reader's own channel, which is bounded and lossless. A
    /// latest-wins frame slot drops pictures a slow consumer misses, so it
    /// cannot show which pictures decoded.
    async fn read_all(reader: &mut Reader) -> Vec<u64> {
        let mut seen = Vec::new();
        while let Some(frame) = reader.frames.recv().await {
            seen.push(frame.timestamp.as_micros() as u64);
        }
        seen
    }

    /// The picture whose access unit the broken stream truncates.
    ///
    /// It lies in the second group, after the reader's start in the first. The
    /// keyframe opening the third group repairs it.
    const BROKEN: u64 = GOP as u64 + 3;

    /// Publishes one stream and reads it to the end.
    ///
    /// Returns the pictures the encoder produced and the pictures the reader
    /// delivered. The encoder does not emit one access unit per input picture,
    /// so compare the two instead of assuming they match.
    async fn read_stream(broken: Option<u64>) -> TestResult<(BTreeSet<u64>, BTreeSet<u64>)> {
        let (mut reader, published, mut seen, _published) = publish(broken).await?;
        seen.extend(read_all(&mut reader).await);
        Ok((published.into_iter().collect(), seen.into_iter().collect()))
    }

    /// The reader carries on through a broken access unit to the next keyframe.
    ///
    /// This does not reach the failure counter: openh264 drops the broken
    /// pictures without returning an error.
    #[tokio::test]
    async fn a_broken_access_unit_does_not_end_playback() -> TestResult {
        let (encoded, intact) = read_stream(None).await?;
        let (_, broken) = read_stream(Some(pts(BROKEN))).await?;

        // Measure the loss instead of predicting it. Platforms conceal
        // differently: macOS delivers the truncated picture and Linux drops it.
        // Both confine the loss to the broken group.
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

    /// An intact stream delivers every picture the encoder produced.
    ///
    /// This exact match is what makes the loss check in the broken test valid.
    /// openh264 drops a picture under rate control, so compare against what it
    /// emitted.
    #[tokio::test]
    async fn an_intact_stream_plays_to_the_end() -> TestResult {
        let (encoded, seen) = read_stream(None).await?;
        assert_eq!(
            seen, encoded,
            "an intact stream delivers every picture that was encoded",
        );
        Ok(())
    }
}
