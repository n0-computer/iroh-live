//! The player's video task: decoders, the rendition swap, and pacing.
//!
//! One decoder plays at a time. A switch or a decoder change opens the
//! replacement beside it and hands over once the replacement has caught up
//! with the picture on screen, so the picture neither goes blank nor steps
//! backwards across a change. The state machine for that lives in
//! [`switch`](super::switch); this is the loop that drives it with real
//! decoders.
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
    future::Future,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};

use n0_future::task::{AbortOnDropHandle, spawn};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, error_span, info, warn};

use super::{
    Abandon, Controls, PlaybackRecorder, PlayoutClock, StatusCell, SwitchEvent,
    select::{DecodeSettings, Desired, Failure, Report},
    switch::{Abandoned, Outcome, Switcher, Target, Verdict},
};
use crate::{
    SlotState,
    error::Error,
    frames::FrameSlot,
    stats::{FrameTiming, MediaKind, RateMeter, Smoothed, VideoPlaybackStats},
};

/// How many decoded frames a reader may run ahead of the supervisor.
///
/// Small on purpose: the supervisor only paces and forwards, so a backlog here
/// would be latency rather than throughput. Two slots absorb a scheduling
/// hiccup without letting the decoder race ahead of the clock.
const READ_AHEAD: usize = 2;

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

/// How often the decode cadence is logged.
///
/// A picture that is starving looks, from every log line above this one, like
/// a picture that is fine: the transport signals say what arrived and the
/// decoder says nothing unless it fails. One line every few seconds with the
/// frame rate that actually decoded is what lets a goodput reading in a trace
/// be matched to what the viewer saw.
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
    pub status: StatusCell,
    pub events: broadcast::Sender<SwitchEvent>,
    /// Where decoders that failed or ended are reported, for the selector's
    /// backoff and revival.
    pub reports: mpsc::Sender<Report>,
    /// The target on screen, for the selector.
    pub playing: watch::Sender<Option<Target>>,
    pub clock: PlayoutClock,
    pub stats: PlaybackRecorder,
    /// How long a replacement decoder has to take over; see
    /// [`Adaptation::switch_deadline`](super::Adaptation::switch_deadline).
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
    due: Pin<Box<dyn Future<Output = bool> + Send>>,
}

/// Forwards frames to the player's output and swaps decoders when the
/// selector asks for another rendition or configuration.
///
/// Every await sits in the `select!` itself and none in an arm body: pacing a
/// picture is a future the loop keeps across iterations rather than one it
/// waits on, so a request that arrives while a picture is held for the clock
/// is acted on at once, not when the picture is due.
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
    let mut pacing = Pacing::default();

    loop {
        let deadline = switcher.deadline();
        let delivering = delivery.is_some();
        // Why a decoder's track stopped, read off the reader before the
        // switcher drops it: a clean end and a failure call for different
        // things.
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
                    // The selector stopped, which it does when the broadcast
                    // closes: the video ended, whatever its tracks said yet.
                    status.update(|status| {
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
                        // Video turned off, or nothing left to play: drop both
                        // decoders and keep the last picture where it is.
                        switcher = VideoSwitcher::new(switch_deadline);
                        delivery = None;
                        stats.video.update(|video| *video = None);
                        status.update(|status| {
                            // Off was set by the selector; anything else means
                            // the catalog has no video left to play.
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
                        Err(Error::decoder(std::io::Error::other(format!(
                            "the decoder open task failed: {err}"
                        ))))
                    });
                    switcher.opened(generation, result)
                }
                Event::Replacement(Some(frame)) => {
                    match switcher.replacement_frame(frame.timestamp.into(), tokio::time::Instant::now()) {
                        (Verdict::Promote, outcome) => {
                            // Whatever the incumbent was about to show is older
                            // than what takes over, so it goes.
                            delivery = Some(pacing.pace(frame, &clock, &controls, &stats));
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
                    delivery = Some(pacing.pace(frame, &clock, &controls, &stats));
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
        status.update(|status| status.switching_to = switching);
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
                status.update(|status| {
                    status.video = SlotState::Running;
                    status.failed_rendition = None;
                    status.rendition = Some(target.rendition.clone());
                    status.decoder = Some(decoder.clone());
                });
                let _ = events.send(SwitchEvent::Landed(target.rendition));
            }
            // A replacement whose track ended cleanly before it took over is
            // not a broken decoder: the publisher replaced the track or the
            // route changed. The selector asks for it again.
            Outcome::Abandoned(target, Abandoned::Ended) if replacement_failure.is_none() => {
                debug!(rendition = %target.rendition, "replacement's track ended before it took over");
                if switcher.current().is_none() {
                    status.update(|status| status.video = SlotState::Starting);
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
                        // With nothing on screen there is nothing better to
                        // play meanwhile, and a slow link is not a broken
                        // rendition: it is asked for again at once.
                        exclude = playing.is_some();
                        Abandon::Failed(Arc::new(Error::decoder(std::io::Error::other(format!(
                            "the decoder for {rendition} did not produce a picture within {}s",
                            switch_deadline.as_secs()
                        )))))
                    }
                };
                if let Abandon::Failed(err) = &abandon {
                    let config_only = playing
                        .as_ref()
                        .is_some_and(|playing| playing.rendition == target.rendition);
                    status.update(|status| {
                        status.switch_error = Some(err.clone());
                        // Nothing on screen and nothing on its way: the video
                        // failed, until the selector finds something to try.
                        if playing.is_none() && status.switching_to.is_none() {
                            status.clear_video(SlotState::Failed(err.clone()));
                            status.failed_rendition = Some(rendition.clone());
                        }
                    });
                    // Full means a failure is already being reported; one more
                    // for the same backoff is not worth waiting for.
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
                    // The reader gave up on its track: a decoder or transport
                    // failure, reported like a failed switch so the selector
                    // backs off from the rendition and tries another.
                    Some((target, Some(err))) => {
                        warn!(error = %err, rendition = %target.rendition, "video failed");
                        status.update(|status| {
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
                    // comes next rather than calling the video over; only the
                    // broadcast closing, or a catalog with no video left, is
                    // the end.
                    Some((target, None)) => {
                        info!(rendition = %target.rendition, "video track ended, waiting for what follows");
                        status.update(|status| status.clear_video(SlotState::Starting));
                        let _ = reports.try_send(Report::Ended(target));
                    }
                    None => status.update(|status| status.clear_video(SlotState::Starting)),
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
    // Abort-on-drop, not a bare handle: a superseded open is dropped with its
    // replacement, and a detached one would keep a track subscription alive for
    // as long as the peer took to answer.
    AbortOnDropHandle::new(spawn(async move {
        spawn_reader(&settings, &config, &name, stats).await
    }))
}

/// Waits for whichever decoder has something to say first.
///
/// One future over every part of the switcher, so the `select!` above holds a
/// single borrow of it. The incumbent is not read while a picture is waiting
/// for the clock: its channel is bounded, which is what keeps the decoder from
/// running ahead of playout.
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

/// The shown-frame rate, counted over a window.
#[derive(Debug, Default)]
struct Pacing {
    meter: RateMeter,
}

impl Pacing {
    /// Starts pacing one frame against the playout clock.
    ///
    /// Reads the latency on every frame, so a change of the pacing mode
    /// reaches the picture at once.
    fn pace(
        &mut self,
        frame: moq_video::Frame,
        clock: &PlayoutClock,
        controls: &Controls,
        stats: &PlaybackRecorder,
    ) -> Delivery {
        let pts = frame.timestamp.into();
        let size = frame.size();
        let rate = self.meter.tick(0);
        stats.video.update(|video| {
            let video = video.get_or_insert_with(VideoPlaybackStats::default);
            video.frames += 1;
            video.size = Some(size);
            if let Some((fps, _)) = rate {
                video.fps = Some(fps.round() as u32);
            }
        });
        let paced = controls.latency.borrow().paced();
        let due: Pin<Box<dyn Future<Output = bool> + Send>> = match paced {
            true => {
                clock.received(pts);
                let clock = clock.clone();
                Box::pin(async move { clock.wait_async(pts).await })
            }
            false => Box::pin(std::future::ready(true)),
        };
        Delivery {
            frame,
            decoded: Instant::now(),
            due,
        }
    }
}

/// One decoder plus the task reading it.
struct Reader {
    /// The backend that opened, for a status line: which decoder is running is
    /// the first thing anyone asks when playback looks wrong on a device.
    decoder: String,
    frames: mpsc::Receiver<moq_video::Frame>,
    /// Why the reader gave up on its track, if it did rather than reaching a
    /// clean end.
    failure: Arc<OnceLock<Arc<Error>>>,
    /// Set once this reader's pictures are the ones on screen.
    ///
    /// Only that reader writes the playback stats: a replacement warming up
    /// beside the incumbent would otherwise write the same figures for as long
    /// as the switch takes.
    on_screen: Arc<AtomicBool>,
    /// Dropping this aborts the read loop, which drops the decoder with it.
    _task: AbortOnDropHandle<()>,
}

/// The decode options a player's settings imply.
fn decode_options(settings: &DecodeSettings) -> moq_video::decode::Options {
    let mut options = moq_video::decode::Options::new();
    options.decoder.kind = settings.decoder.clone();
    // Left on the GPU: a player's frames go to a renderer, which imports a
    // shared decode surface without a round trip through system memory, and
    // a frame converts to CPU pixels on demand for anything that reads them.
    options.decoder.output = moq_video::Output::Native;
    options.max_age = settings.max_age;
    // This is a player, so the groups a track still holds are behind the live
    // edge by definition. A decoder rebuilt on a backend change, or opened on a
    // rendition switched away from and back to, would otherwise walk that whole
    // backlog at decode speed before catching up.
    options.start = moq_video::decode::Start::Latest;
    options
}

/// Subscribes to a rendition, opens its decoder, and starts reading it.
///
/// Returns once the decoder is open, so a caller can tell an unusable rendition
/// from a slow one before committing to a switch.
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
                        // Covers the transport read as well as the decode: the
                        // two happen inside one `read`, with no earlier point
                        // to attribute arrival to.
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
                    // `Codec` is the one error about the bytes of a single
                    // picture, which the next keyframe makes good. The others
                    // describe the track, and reading again fails the same way.
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
                            let _ = gave_up.set(Arc::new(Error::decoder(std::io::Error::other(
                                format!("the decoder refused {failures} access units in a row: {err}"),
                            ))));
                            return;
                        }
                        // Once per run: a lost reference chain fails every
                        // picture until the next keyframe, and the first of
                        // those says everything the rest would.
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

/// A decoder failure, as the crate reports it.
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
    use n0_watcher::Watcher as _;

    use super::{super::PlaybackRecorder, *};
    use crate::RemoteBroadcast;

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
        _catalog: moq_mux::catalog::Producer,
        _import: moq_mux::codec::h264::Import,
        /// Cancels every decode task on drop, so the reader stops with it.
        _remote: RemoteBroadcast,
    }

    /// Encodes [`PICTURES`] pictures of a moving pattern as H.264 access units.
    ///
    /// Moving rather than flat so the inter-coded pictures carry residuals: a
    /// static picture codes to almost nothing and a decoder can conceal its way
    /// through a break in it without ever reporting one.
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

        // The whole of the first group, so the only live edge a reader can open
        // at is inside it, whichever way its start policy resolves. Split by
        // presentation time rather than by count, for the reason `feed` gives.
        let boundary =
            units.partition_point(|unit| (unit.timestamp.as_micros() as u64) < pts(GOP.into()));
        feed(&mut import, &mut split, &units[..boundary], None)?;

        // The latency ceiling would otherwise have the container consumer skip
        // ahead of the break rather than deliver it.
        let settings = DecodeSettings {
            consumer: consumer.clone(),
            decoder: moq_video::decode::Kind::Software,
            max_age: Duration::from_secs(60),
        };
        let remote = RemoteBroadcast::from_moq(consumer);
        // The catalog travels on a track of its own, so the rendition that
        // first SPS filled in may not have arrived with the first snapshot.
        let mut snapshots = remote.catalog();
        let config = loop {
            if let Some(known) = snapshots.get()
                && let Some(config) = known.video.renditions.get("video")
            {
                break config.clone();
            }
            snapshots
                .updated()
                .await
                .map_err(|_| "the catalog ended before it carried a video rendition")?;
        };
        let mut reader =
            spawn_reader(&settings, &config, "video", PlaybackRecorder::default()).await?;

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
    /// not reach the failure counter: openh264 absorbs a truncated access unit
    /// and the ones that lost their reference to it by producing no picture,
    /// rather than by returning an error, so `read` never fails here.
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
}
