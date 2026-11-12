//! The selector: which rendition should play, under which decoder settings.
//!
//! Turns the rendition mode, the catalog, the network and the decoders'
//! failures into one [`Desired`] value for the video supervisor. It never
//! opens a decoder.

use std::{sync::Arc, time::Duration};

use moq_ext_adaptive::{
    Adaptation, Backoffs, Bound, Constraints, PinProblem, RenditionMode, Target,
};
use moq_ext_sources::{Error, SlotState, send_if_changed};
use moq_net::track::Delay;
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tracing::{debug, info, trace};

use super::{Controls, PlayerStatus};
use crate::RemoteBroadcast;

/// How long a rendition whose decoder failed is left alone.
const EXCLUSION: Duration = Duration::from_secs(10);

/// How long after a track ended with no successor the target is asked for again.
const REVIVE_AFTER: Duration = Duration::from_secs(2);

/// How long a catalog may lack video before the video counts as over.
///
/// A publisher replacing its video removes the old renditions before it adds
/// the new ones. A closing publisher empties its catalog before its broadcast
/// ends, and another route may serve the path right after. Both gaps are brief.
const VANISH_GRACE: Duration = Duration::from_secs(3);

/// What the decoder of a target is built with.
#[derive(derive_more::Debug, Clone)]
pub(crate) struct DecodeSettings {
    /// The broadcast to read.
    #[debug(skip)]
    pub consumer: moq_net::broadcast::Consumer,
    /// The backend to open.
    pub decoder: moq_video::decode::Kind,
    /// What the subscription asks for: the latency's minimum and maximum.
    pub delay: Delay,
    /// Whether to record when each frame arrives.
    pub record_arrivals: bool,
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
    /// The player's playout clock.
    ///
    /// It starts over on a new route, and follows the latency and the
    /// catalog's jitter.
    pub clock: super::PlayoutClock,
    /// The thresholds and timers the choice follows.
    pub adaptation: Adaptation,
    /// Whether decoders record when their frames arrive.
    pub record_arrivals: bool,
}

/// The decoder configuration a target was last built under.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    decoder: moq_video::decode::Kind,
    delay: Delay,
    /// The generation of the broadcast consumer, bumped on a new route.
    epoch: u64,
    /// Bumped to rebuild a video that ended.
    restart: u64,
}

/// Runs the selector until the player is dropped.
pub(crate) async fn run(inputs: Inputs) {
    let mut selector = Selector::new(inputs);
    // The first pass decides at once. A catalog that arrived before the player
    // started wakes nothing in `wait`, and without audio or network signals
    // nothing else would.
    loop {
        selector.decide();
        if !selector.wait().await {
            return;
        }
    }
}

/// The selector's inputs, and the state it carries from one pass to the next.
struct Selector {
    broadcast: RemoteBroadcast,
    status: watch::Sender<PlayerStatus>,
    reports: mpsc::Receiver<Report>,
    playing: watch::Receiver<Option<Target>>,
    desired: watch::Sender<Option<Desired>>,
    clock: super::PlayoutClock,
    record_arrivals: bool,
    mode: watch::Receiver<RenditionMode>,
    latency: watch::Receiver<Delay>,
    decoder: watch::Receiver<moq_video::decode::Kind>,
    catalog: watch::Receiver<Option<Arc<hang::catalog::Catalog>>>,
    epoch: watch::Receiver<crate::remote::Epoch>,
    player: watch::Receiver<PlayerStatus>,
    network: Option<crate::network::NetworkSignals>,
    ticker: tokio::time::Interval,
    bound: Bound,
    backoffs: Backoffs,
    generation: u64,
    last_config: Option<Config>,
    restart: u64,
    /// The target whose track ended with nothing to follow it.
    ended: Option<Target>,
    /// When to ask for `ended` again.
    ///
    /// The catalog or the route changing first makes this moot.
    revive_at: Option<Instant>,
    /// Since when the catalog has had nothing to play while something was asked for.
    vanished_since: Option<Instant>,
    /// The pin error this selector last wrote.
    ///
    /// The selector replaces only its own errors, and leaves a failed
    /// switch's alone.
    last_pin_error: Option<Arc<Error>>,
    /// The last target the supervisor gave up on.
    ///
    /// Choosing the same target again leaves the desired value unchanged, so
    /// this forces a resend.
    failed: Option<Failure>,
}

impl Selector {
    /// Creates a selector that has decided nothing yet.
    fn new(inputs: Inputs) -> Self {
        let Inputs {
            broadcast,
            controls,
            status,
            reports,
            playing,
            desired,
            clock,
            adaptation,
            record_arrivals,
        } = inputs;
        let mut ticker = tokio::time::interval(adaptation.tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            mode: controls.mode.subscribe(),
            latency: controls.latency.subscribe(),
            decoder: controls.decoder.subscribe(),
            catalog: broadcast.catalog(),
            epoch: broadcast.epoch(),
            player: status.subscribe(),
            network: broadcast.network(),
            broadcast,
            status,
            reports,
            playing,
            desired,
            clock,
            record_arrivals,
            ticker,
            bound: Bound::new(adaptation),
            backoffs: Backoffs::default(),
            generation: 0,
            last_config: None,
            restart: 0,
            ended: None,
            revive_at: None,
            vanished_since: None,
            last_pin_error: None,
            failed: None,
        }
    }

    /// Waits for the next input to change, and records what it means.
    ///
    /// Returns `false` once the broadcast closed or the player is gone.
    async fn wait(&mut self) -> bool {
        let auto = matches!(*self.mode.borrow(), RenditionMode::Auto);
        let ticking = (auto && self.network.is_some())
            || self.backoffs.any_excluded(Instant::now().into_std())
            || self.vanished_since.is_some();
        let revive_at = self.revive_at;
        tokio::select! {
            // Returning drops the desired sender. That ends the video task
            // and the player's frames, so a reader in `next()` sees the end.
            () = self.broadcast.closed() => {
                debug!("the broadcast closed");
                return false;
            }
            changed = self.mode.changed() => return changed.is_ok(),
            changed = self.latency.changed() => return changed.is_ok(),
            changed = self.decoder.changed() => return changed.is_ok(),
            changed = self.playing.changed() => {
                if changed.is_err() {
                    return false;
                }
                if self.playing.borrow_and_update().is_some() {
                    self.ended = None;
                    self.revive_at = None;
                }
            }
            changed = self.catalog.changed() => {
                if changed.is_err() {
                    return false;
                }
                // A new catalog means the publisher republished, so a
                // video that ended is rebuilt.
                if self.ended.take().is_some() {
                    self.restart += 1;
                    self.revive_at = None;
                }
            }
            changed = self.epoch.changed() => {
                if changed.is_err() {
                    return false;
                }
                self.clock.restart();
            }
            changed = self.player.changed() => return changed.is_ok(),
            () = async { tokio::time::sleep_until(revive_at.expect("guarded")).await },
                if revive_at.is_some() =>
            {
                self.revive_at = None;
                // Ask again only if nothing changed meanwhile. A new route
                // or catalog has already asked for something.
                let still = self
                    .desired
                    .borrow()
                    .as_ref()
                    .is_some_and(|desired| Some(&desired.target) == self.ended.as_ref());
                if self.ended.take().is_some() && still && self.playing.borrow().is_none() {
                    debug!("asking again for video that ended");
                    self.restart += 1;
                }
            }
            report = self.reports.recv() => match report {
                None => return false,
                Some(Report::Ended(target)) => {
                    debug!(rendition = %target.rendition, "video ended, asking again shortly");
                    self.revive_at = Some(Instant::now() + REVIVE_AFTER);
                    self.ended = Some(target);
                }
                Some(Report::Failed(reported)) => {
                    let rendition = &reported.target.rendition;
                    if reported.exclude && !reported.config_only {
                        self.backoffs.fail(rendition, (Instant::now() + EXCLUSION).into_std());
                        info!(%rendition, after = ?EXCLUSION, "leaving a failing rendition alone");
                    }
                    self.failed = Some(reported);
                }
            },
            _ = self.ticker.tick(), if ticking => {}
        }
        true
    }

    /// Chooses the rendition to play, and tells the supervisor if it changed.
    fn decide(&mut self) {
        let now = Instant::now();
        let std_now = now.into_std();

        let mode = self.mode.borrow().clone();
        let Some(catalog) = self.catalog.borrow_and_update().clone() else {
            return;
        };
        let (route, consumer) = {
            let epoch = self.epoch.borrow_and_update();
            (epoch.generation, epoch.consumer.clone())
        };
        let Some(consumer) = consumer else {
            return;
        };
        let settings = self.settings(&catalog, route, consumer);

        let sample = self.network.as_ref().map(|network| network());
        let on_screen = self.status.borrow().rendition.clone();
        let nothing_playing = on_screen.is_none();
        // The bound weighs its target against what this selector last asked
        // for, on screen or on its way. Weighed against the old rendition still
        // on screen, each pass would restart the hold and undo the switch.
        let current = self
            .desired
            .borrow()
            .as_ref()
            .map(|desired| desired.target.rendition.clone())
            .or_else(|| on_screen.clone());

        let rungs = crate::catalog::rungs(&catalog);
        let chosen = moq_ext_adaptive::choose(
            &mode,
            &rungs,
            &Constraints {
                excluded: self.backoffs.excluded(std_now),
            },
            current.as_deref(),
            on_screen.as_deref(),
            sample.as_ref(),
            &mut self.bound,
            std_now,
        );
        let choice = chosen.rendition;
        let pin_error = chosen.pin.map(|problem| explain_pin(problem, &catalog));
        let pin_error_changed = !same_error(&self.last_pin_error, &pin_error);
        send_if_changed(&self.status, |status| {
            if pin_error_changed {
                status.switch_error = pin_error.clone().map(Into::into);
            }
            match (&mode, &status.video) {
                (RenditionMode::Off, _) => status.video = SlotState::Off,
                (_, SlotState::Off) => status.video = SlotState::Starting,
                // Video came back to a catalog that had none.
                (_, SlotState::Ended) if choice.is_some() => status.video = SlotState::Starting,
                _ => {}
            }
        });
        if pin_error_changed {
            self.last_pin_error = pin_error;
        }
        let step_down =
            moq_ext_adaptive::step_down(&mode, &rungs, choice.as_deref(), on_screen.as_deref());
        let next = choice.and_then(|name| {
            let config = catalog.video.renditions.get(&name)?.clone();
            Some(Desired {
                target: Target::new(name, self.generation),
                settings: settings.clone(),
                config,
                step_down,
            })
        });
        // A catalog that never had video counts too, so an audio-only
        // broadcast ends its video instead of starting forever.
        let vanished = next.is_none()
            && !matches!(mode, RenditionMode::Off)
            && self.status.borrow().video != SlotState::Ended;
        if vanished {
            let since = *self.vanished_since.get_or_insert(now);
            if now.duration_since(since) < VANISH_GRACE {
                return;
            }
            debug!("the catalog has had no video for a while, so the video is over");
            send_if_changed(&self.status, |status| status.video = SlotState::Ended);
        }
        self.vanished_since = None;
        // Ask again for a target given up on once its backoff is over. A
        // configuration that failed beside a working one waits until nothing
        // plays.
        let retry = match (&self.failed, &next) {
            (Some(failed), Some(next)) => {
                failed.target == next.target
                    && !self.backoffs.is_excluded(&failed.target.rendition, std_now)
                    && (!failed.config_only || nothing_playing)
            }
            _ => false,
        };
        let failed = &mut self.failed;
        self.desired.send_if_modified(|desired| {
            let changed = desired.as_ref().map(|wanted| &wanted.target) != next.as_ref().map(|next| &next.target);
            if changed || retry {
                trace!(desired = ?next.as_ref().map(|next| &next.target), retry, "desired rendition");
                *desired = next;
                *failed = None;
            }
            changed || retry
        });
    }

    /// Returns the decoder settings for this pass, and follows the latency and the configuration.
    ///
    /// Sets the playout delay from the latency and the ladder's jitter. A new
    /// decoder configuration starts a new target generation, and gives every
    /// rendition that failed under the old one another try.
    fn settings(
        &mut self,
        catalog: &hang::catalog::Catalog,
        route: u64,
        consumer: moq_net::broadcast::Consumer,
    ) -> DecodeSettings {
        let latency = *self.latency.borrow_and_update();
        // The ladder's largest jitter, so a switch never moves the schedule.
        let jitter = catalog
            .video
            .renditions
            .values()
            .filter_map(|config| config.jitter)
            .max();
        let playout = super::playout_delay(&latency, jitter);
        if playout != self.clock.delay() {
            debug!(?playout, ?jitter, "playout delay changed");
            self.clock.set_delay(playout);
        }
        let config = Config {
            decoder: self.decoder.borrow().clone(),
            delay: latency,
            epoch: route,
            restart: self.restart,
        };
        if self.last_config.as_ref() != Some(&config) {
            self.generation += 1;
            debug!(
                generation = self.generation,
                ?config,
                "decoder configuration changed"
            );
            self.last_config = Some(config);
            // A rendition that failed under the old configuration gets a try
            // under the new one.
            self.backoffs.clear();
        }
        DecodeSettings {
            consumer,
            decoder: self.decoder.borrow().clone(),
            delay: latency,
            record_arrivals: self.record_arrivals,
        }
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

/// Returns the error that explains a pin that could not be honoured.
fn explain_pin(problem: PinProblem, catalog: &hang::catalog::Catalog) -> Arc<Error> {
    Arc::new(match problem {
        PinProblem::Excluded(name) => Error::decoder_msg(format!(
            "the decoder for {name} failed, so another rendition plays until it is retried"
        )),
        PinProblem::Unknown(name) => crate::video_rendition(catalog, &name)
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
        controls: Arc<Controls>,
        clock: super::super::PlayoutClock,
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
                )
                .expect("the pattern thread starts"),
                crate::test_util::single(None),
            )
            .expect("a valid encoding");
        let (reports_tx, reports) = mpsc::channel(8);
        // Nothing is ever on screen, which a revival requires.
        let (playing_tx, playing) = watch::channel(None);
        let (desired, desired_rx) = watch::channel(None);
        let controls = Arc::new(Controls {
            mode: watch::Sender::new(RenditionMode::Auto),
            latency: watch::Sender::new(super::super::DEFAULT_LATENCY),
            decoder: watch::Sender::new(moq_video::decode::Kind::Software),
            volume: watch::Sender::new(1.0),
        });
        let clock = super::super::PlayoutClock::new(Duration::from_millis(100));
        let inputs = Inputs {
            broadcast: RemoteBroadcast::from_consumer(broadcast.consume()),
            controls: controls.clone(),
            status: watch::Sender::default(),
            reports,
            playing,
            desired,
            clock: clock.clone(),
            adaptation: Adaptation::default(),
            record_arrivals: false,
        };
        let task = n0_future::task::AbortOnDropHandle::new(n0_future::task::spawn(run(inputs)));
        Running {
            _broadcast: broadcast,
            reports: reports_tx,
            desired: desired_rx,
            controls,
            clock,
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

    /// A new latency asks for the same rendition under a new configuration with the new delay.
    ///
    /// The new configuration is what makes the supervisor build a decoder
    /// behind the picture, and the playout clock starts over at the new delay.
    #[tokio::test]
    async fn a_new_latency_rebuilds_the_target_and_moves_the_clock() {
        let running = selector();
        let mut desired = running.desired.clone();
        let first = next_target(&mut desired, None).await;
        assert_eq!(
            desired.borrow().as_ref().expect("a target").settings.delay,
            super::super::DEFAULT_LATENCY
        );

        let latency = Delay {
            min: Some(Duration::from_millis(300)),
            max: Some(Duration::from_millis(400)),
        };
        running.controls.latency.send_replace(latency);
        let rebuilt = next_target(&mut desired, Some(&first)).await;
        assert_eq!(rebuilt.rendition, first.rendition);
        assert!(rebuilt.config > first.config, "{rebuilt:?} after {first:?}");
        assert_eq!(
            desired.borrow().as_ref().expect("a target").settings.delay,
            latency
        );
        assert_eq!(running.clock.delay(), Duration::from_millis(300));
    }
}
