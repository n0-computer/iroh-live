//! The selector: which rendition should play, under which decoder settings.
//!
//! Turns the rendition mode, the catalog, the network and the decoders'
//! failures into one [`Desired`] value for the video supervisor. It never
//! opens a decoder.

use std::{sync::Arc, time::Duration};

use moq_ext_adaptive::{
    Adaptation, Backoffs, Bound, EXCLUSION, Mode, PinProblem, REVIVE_AFTER, Target, VANISH_GRACE,
};
use moq_ext_media_core::{Error, SlotState, send_if_changed};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tracing::{debug, info, trace};

use super::{Controls, Latency, PlayerStatus};
use crate::{RemoteBroadcast, catalog::VideoRenditions as _};

/// What the decoder of a target is built with.
#[derive(derive_more::Debug, Clone)]
pub(crate) struct DecodeSettings {
    /// The broadcast to read.
    #[debug(skip)]
    pub consumer: moq_net::broadcast::Consumer,
    /// The backend to open.
    pub decoder: moq_video::decode::Kind,
    /// The oldest media still worth decoding.
    pub max_age: Duration,
}

/// What the video supervisor should play.
#[derive(Debug, Clone)]
pub(crate) struct Desired {
    /// The rendition and decoder configuration, compared by value.
    pub target: Target,
    /// What that configuration builds a decoder with.
    pub settings: DecodeSettings,
    /// The catalog's description of the rendition.
    pub config: hang::catalog::VideoConfig,
    /// Whether automatic selection is stepping down from the rendition on screen.
    ///
    /// Such a switch does not overlap. On a saturated link the incumbent's
    /// track would keep taking the bandwidth the replacement needs, and the
    /// replacement's groups would age out before they arrive. The supervisor
    /// drops the incumbent, and its last picture stays up until the
    /// replacement's first.
    pub step_down: bool,
}

/// A decoder that failed, as the supervisor reports it.
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    /// The rendition and configuration it was for.
    pub target: Target,
    /// Whether it only changed the decoder configuration of the rendition playing.
    ///
    /// Such a failure means the configuration is broken, not the rendition.
    /// The incumbent keeps playing under the working configuration. Excluding
    /// the rendition would only walk the ladder down under the broken one.
    pub config_only: bool,
    /// Whether the rendition is backed off before it is tried again.
    ///
    /// A first decoder that timed out is asked for again at once. With nothing
    /// on screen there is nothing better to play, and a slow link does not
    /// mean a broken rendition.
    pub exclude: bool,
}

/// What the supervisor tells the selector about the decoders it ran.
#[derive(Debug, Clone)]
pub(crate) enum Report {
    /// A decoder did not open, missed its deadline, or gave up on its track.
    Failed(Failure),
    /// A track ended cleanly with nothing to take over from it.
    ///
    /// The publisher replaced or withdrew its video, or the route changed. A
    /// replacement's catalog can arrive before the old track's end, so the
    /// selector also asks for the target again after a backoff.
    Ended(Target),
}

/// The selector's inputs.
pub(crate) struct Inputs {
    pub broadcast: RemoteBroadcast,
    pub controls: Arc<Controls>,
    pub status: watch::Sender<PlayerStatus>,
    /// What happened to the decoders, reported by the supervisor.
    pub reports: mpsc::Receiver<Report>,
    /// The target on screen, as the supervisor reports it.
    pub playing: watch::Receiver<Option<Target>>,
    pub desired: watch::Sender<Option<Desired>>,
    /// The player's playout clock, started over on a new route.
    pub clock: super::PlayoutClock,
    /// The thresholds and timers the choice follows.
    pub adaptation: Adaptation,
}

/// The decoder configuration a target was last built under.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    decoder: moq_video::decode::Kind,
    max_age: Duration,
    /// The generation of the broadcast consumer, bumped on a new route.
    epoch: u64,
    /// Bumped to rebuild a video that ended.
    restart: u64,
}

/// Runs the selector until the player is dropped.
pub(crate) async fn run(inputs: Inputs) {
    let Inputs {
        broadcast,
        controls,
        status,
        mut reports,
        mut playing,
        desired,
        clock,
        adaptation,
    } = inputs;
    let mut mode = controls.mode.subscribe();
    let mut latency = controls.latency.subscribe();
    let mut decoder = controls.decoder.subscribe();
    let mut catalog = broadcast.catalog();
    let mut epoch = broadcast.epoch();
    let mut player = status.subscribe();
    let network = broadcast.network();

    let mut ticker = tokio::time::interval(adaptation.tick);
    let mut bound = Bound::new(adaptation);
    let mut backoffs = Backoffs::default();
    let mut generation = 0u64;
    let mut last_config: Option<Config> = None;
    let mut restart = 0u64;
    // The target whose track ended with nothing to follow it, and when to ask
    // for it again if neither the catalog nor the route changes first.
    let mut ended: Option<Target> = None;
    let mut revive_at: Option<Instant> = None;
    // Since when the catalog has had nothing to play while something was asked for.
    let mut vanished_since: Option<Instant> = None;
    // The pin error this selector last wrote, so it replaces only its own
    // errors and leaves a failed switch's alone.
    let mut last_why: Option<Arc<Error>> = None;
    // The last target the supervisor gave up on. Choosing the same target
    // again leaves the desired value unchanged, so this forces a resend.
    let mut failed: Option<Failure> = None;
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // The first pass decides at once. A catalog that arrived before the player
    // started wakes nothing below, and without audio or network signals
    // nothing else would.
    let mut first = true;
    loop {
        let auto = matches!(*mode.borrow(), Mode::Auto { .. });
        let ticking = (auto && network.is_some())
            || backoffs.any_excluded(Instant::now().into_std())
            || vanished_since.is_some();
        if !std::mem::take(&mut first) {
            tokio::select! {
                // Returning drops the desired sender. That ends the video task
                // and the player's frames, so a reader in `next()` sees the end.
                () = broadcast.closed() => {
                    debug!("the broadcast closed");
                    return;
                }
                changed = mode.changed() => if changed.is_err() { return },
                changed = latency.changed() => if changed.is_err() { return },
                changed = decoder.changed() => if changed.is_err() { return },
                changed = playing.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if playing.borrow_and_update().is_some() {
                        ended = None;
                        revive_at = None;
                    }
                }
                changed = catalog.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    // A new catalog means the publisher republished, so a
                    // video that ended is rebuilt.
                    if ended.take().is_some() {
                        restart += 1;
                        revive_at = None;
                    }
                }
                changed = epoch.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    clock.restart();
                }
                changed = player.changed() => if changed.is_err() { return },
                () = async { tokio::time::sleep_until(revive_at.expect("guarded")).await },
                    if revive_at.is_some() =>
                {
                    revive_at = None;
                    // Ask again only if nothing changed meanwhile. A new route
                    // or catalog has already asked for something.
                    let still = desired
                        .borrow()
                        .as_ref()
                        .is_some_and(|desired| Some(&desired.target) == ended.as_ref());
                    if ended.take().is_some() && still && playing.borrow().is_none() {
                        debug!("asking again for video that ended");
                        restart += 1;
                    }
                }
                report = reports.recv() => match report {
                    None => return,
                    Some(Report::Ended(target)) => {
                        debug!(rendition = %target.rendition, "video ended, asking again shortly");
                        revive_at = Some(Instant::now() + REVIVE_AFTER);
                        ended = Some(target);
                    }
                    Some(Report::Failed(reported)) => {
                        let rendition = &reported.target.rendition;
                        if reported.exclude && !reported.config_only {
                            backoffs.fail(rendition, Instant::now().into_std());
                            info!(%rendition, after = ?EXCLUSION, "leaving a failing rendition alone");
                        }
                        failed = Some(reported);
                    }
                },
                _ = ticker.tick(), if ticking => {}
            }
        }

        let now = Instant::now();
        let std_now = now.into_std();

        let mode = mode.borrow().clone();
        let Some(catalog) = catalog.borrow_and_update().clone() else {
            continue;
        };
        let (route, consumer) = {
            let epoch = epoch.borrow_and_update();
            (epoch.generation, epoch.consumer.clone())
        };
        let Some(consumer) = consumer else {
            continue;
        };

        let latency: Latency = *latency.borrow();
        let config = Config {
            decoder: decoder.borrow().clone(),
            max_age: latency.max,
            epoch: route,
            restart,
        };
        if last_config.as_ref() != Some(&config) {
            generation += 1;
            debug!(generation, ?config, "decoder configuration changed");
            last_config = Some(config);
            // A rendition that failed under the old configuration gets a try
            // under the new one.
            backoffs.clear();
        }
        let settings = DecodeSettings {
            consumer,
            decoder: decoder.borrow().clone(),
            max_age: latency.max,
        };

        let sample = network.as_ref().map(|network| network());
        let on_screen = status.borrow().rendition.clone();
        let nothing_playing = on_screen.is_none();
        // The bound weighs its target against what this selector last asked
        // for, on screen or on its way. Weighed against the old rendition still
        // on screen, each pass would restart the hold and undo the switch.
        let current = desired
            .borrow()
            .as_ref()
            .map(|desired| desired.target.rendition.clone())
            .or_else(|| on_screen.clone());

        let rungs = crate::catalog::rungs(&catalog);
        let chosen = moq_ext_adaptive::choose(
            &mode,
            &rungs,
            &backoffs.excluded(std_now),
            current.as_deref(),
            on_screen.as_deref(),
            sample.as_ref(),
            &mut bound,
            std_now,
        );
        let choice = chosen.rendition;
        let why = chosen.pin.map(|problem| pin_error(problem, &catalog));
        let why_changed = !same_error(&last_why, &why);
        send_if_changed(&status, |status| {
            if why_changed {
                status.switch_error = why.clone().map(Into::into);
            }
            match (&mode, &status.video) {
                (Mode::Off, _) => status.video = SlotState::Off,
                (_, SlotState::Off) => status.video = SlotState::Starting,
                // Video came back to a catalog that had none.
                (_, SlotState::Ended) if choice.is_some() => status.video = SlotState::Starting,
                _ => {}
            }
        });
        if why_changed {
            last_why = why;
        }
        let step_down =
            moq_ext_adaptive::step_down(&mode, &rungs, choice.as_deref(), on_screen.as_deref());
        let next = choice.and_then(|name| {
            let config = catalog.video.renditions.get(&name)?.clone();
            Some(Desired {
                target: Target::new(name, generation),
                settings: settings.clone(),
                config,
                step_down,
            })
        });
        // A catalog that never had video counts too, so an audio-only
        // broadcast ends its video instead of starting forever.
        let vanished = next.is_none()
            && !matches!(mode, Mode::Off)
            && status.borrow().video != SlotState::Ended;
        if vanished {
            let since = *vanished_since.get_or_insert(now);
            if now.duration_since(since) < VANISH_GRACE {
                continue;
            }
            debug!("the catalog has had no video for a while, so the video is over");
            send_if_changed(&status, |status| status.video = SlotState::Ended);
        }
        vanished_since = None;
        // Ask again for a target given up on once its backoff is over. A
        // configuration that failed beside a working one waits until nothing
        // plays.
        let retry = match (&failed, &next) {
            (Some(failed), Some(next)) => {
                failed.target == next.target
                    && !backoffs.is_excluded(&failed.target.rendition, std_now)
                    && (!failed.config_only || nothing_playing)
            }
            _ => false,
        };
        desired.send_if_modified(|desired| {
            let changed = desired.as_ref().map(|wanted| &wanted.target) != next.as_ref().map(|next| &next.target);
            if changed || retry {
                trace!(desired = ?next.as_ref().map(|next| &next.target), retry, "desired rendition");
                *desired = next;
                failed = None;
            }
            changed || retry
        });
    }
}

/// Reports whether two optional errors say the same thing.
///
/// Compares messages, because the selector builds a fresh error on every pass.
fn same_error(left: &Option<Arc<Error>>, right: &Option<Arc<Error>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.to_string() == right.to_string(),
        (None, None) => true,
        _ => false,
    }
}

/// Explains a pin that could not be honoured.
fn pin_error(problem: PinProblem, catalog: &hang::catalog::Catalog) -> Arc<Error> {
    Arc::new(match problem {
        PinProblem::Excluded(name) => Error::decoder_msg(format!(
            "the decoder for {name} failed, so another rendition plays until it is retried"
        )),
        PinProblem::Unknown(name) => catalog
            .video_rendition(&name)
            .err()
            .unwrap_or_else(|| Error::decoder_msg(format!("no video rendition named {name}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A running selector and what drives it.
    struct Running {
        _broadcast: moq_ext_publisher::LocalBroadcast,
        reports: mpsc::Sender<Report>,
        desired: watch::Receiver<Option<Desired>>,
        _playing: watch::Sender<Option<Target>>,
        _task: n0_future::task::AbortOnDropHandle<()>,
    }

    /// Starts a selector over a one-rendition broadcast.
    fn selector() -> Running {
        let broadcast = moq_ext_publisher::LocalBroadcast::new();
        broadcast
            .set_video(
                moq_ext_sources::VideoSource::test_pattern(
                    moq_video::Size::new(320, 180),
                    moq_video::FrameRate::new(30, 1).expect("a valid rate"),
                ),
                moq_ext_publisher::VideoEncoding {
                    prefer_hardware: false,
                    ..moq_ext_publisher::VideoEncoding::single(
                        moq_ext_publisher::VideoRendition::new("video"),
                    )
                },
            )
            .expect("a valid encoding");
        let (reports_tx, reports) = mpsc::channel(8);
        // Nothing is ever on screen, which a revival requires.
        let (playing_tx, playing) = watch::channel(None);
        let (desired, desired_rx) = watch::channel(None);
        let inputs = Inputs {
            broadcast: RemoteBroadcast::from_consumer(broadcast.consume()),
            controls: Arc::new(Controls {
                mode: watch::Sender::new(Mode::auto()),
                latency: watch::Sender::new(Latency::default()),
                decoder: watch::Sender::new(moq_video::decode::Kind::Software),
                volume: watch::Sender::new(1.0),
            }),
            status: watch::Sender::default(),
            reports,
            playing,
            desired,
            clock: super::super::PlayoutClock::new(Duration::from_millis(100)),
            adaptation: Adaptation::default(),
        };
        let task = n0_future::task::AbortOnDropHandle::new(n0_future::task::spawn(run(inputs)));
        Running {
            _broadcast: broadcast,
            reports: reports_tx,
            desired: desired_rx,
            _playing: playing_tx,
            _task: task,
        }
    }

    /// Waits until the selector asks for a target other than `not`.
    async fn next_target(
        desired: &mut watch::Receiver<Option<Desired>>,
        not: Option<&Target>,
    ) -> Target {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(target) = desired
                    .borrow_and_update()
                    .as_ref()
                    .map(|wanted| wanted.target.clone())
                    && Some(&target) != not
                {
                    return target;
                }
                desired.changed().await.expect("the selector runs");
            }
        })
        .await
        .expect("the selector asked for nothing new")
    }

    /// An ended track is asked for again after a backoff, with no catalog update.
    #[tokio::test]
    async fn a_track_that_ended_is_asked_for_again() {
        // Bound whole: a field left out of a pattern would drop at once and
        // stop the selector.
        let running = selector();
        let Running {
            reports, desired, ..
        } = &running;
        let mut desired = desired.clone();
        let first = next_target(&mut desired, None).await;
        reports
            .send(Report::Ended(first.clone()))
            .await
            .expect("the selector runs");
        let again = next_target(&mut desired, Some(&first)).await;
        assert_eq!(again.rendition, first.rendition);
        assert!(again.config > first.config, "{again:?} after {first:?}");
    }
}
