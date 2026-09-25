//! The selector: which rendition should play, under which decoder settings.
//!
//! Turns the rendition mode, the catalog, the network and the decoders'
//! failures into one [`Desired`] value for the video supervisor. It never
//! opens a decoder.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use n0_watcher::Watcher as _;
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace};

use super::{
    Controls, Latency, RenditionMode, StatusCell,
    bound::{Adaptation, Bound, Constraints, Rung},
    switch::Target,
};
use crate::{Catalog, RemoteBroadcast, SlotState, error::Error, video};

/// How long a rendition whose decoder failed is left alone.
const EXCLUSION: Duration = Duration::from_secs(10);

/// How long after a track ended with no successor the target is asked for again.
const REVIVE_AFTER: Duration = Duration::from_secs(2);

/// How long a catalog may lack video before the video counts as over.
///
/// A publisher replacing its video removes the old renditions before it adds
/// the new ones. A closing publisher empties its catalog before it ends, just
/// before the next route in a route table serves the path. Both gaps are brief.
const VANISH_GRACE: Duration = Duration::from_secs(3);

/// What the decoder of a target is built with.
#[derive(derive_more::Debug, Clone)]
pub(crate) struct DecodeSettings {
    /// The broadcast to read.
    #[debug(skip)]
    pub consumer: moq_net::broadcast::Consumer,
    /// The backend to open.
    pub decoder: video::decode::Kind,
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
    pub status: StatusCell,
    /// What happened to the decoders, reported by the supervisor.
    pub reports: mpsc::Receiver<Report>,
    /// The target on screen, as the supervisor reports it.
    pub playing: watch::Receiver<Option<Target>>,
    pub desired: watch::Sender<Option<Desired>>,
    /// The player's playout clock, started over on a new route.
    pub clock: super::PlayoutClock,
    /// The thresholds and timers the choice follows.
    pub adaptation: Adaptation,
    pub shutdown: CancellationToken,
}

/// The renditions whose decoders failed, and until when each is left alone.
#[derive(Debug, Default)]
struct Backoffs(BTreeMap<String, Instant>);

impl Backoffs {
    /// Leaves `rendition` alone for [`EXCLUSION`] from `now`.
    fn fail(&mut self, rendition: &str, now: Instant) {
        self.0.insert(rendition.to_string(), now + EXCLUSION);
    }

    /// Returns the renditions left alone at `now`.
    fn excluded(&self, now: Instant) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Reports whether `rendition` is left alone at `now`.
    fn is_excluded(&self, rendition: &str, now: Instant) -> bool {
        self.0.get(rendition).is_some_and(|until| *until > now)
    }

    /// Reports whether any rendition is left alone at `now`.
    fn any_excluded(&self, now: Instant) -> bool {
        self.0.values().any(|until| *until > now)
    }

    /// Forgets every failure, for a new decoder configuration.
    fn clear(&mut self) {
        self.0.clear();
    }
}

/// The decoder configuration a target was last built under.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    decoder: video::decode::Kind,
    max_age: Duration,
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
        shutdown,
    } = inputs;
    let mut mode = controls.mode.subscribe();
    let mut latency = controls.latency.subscribe();
    let mut decoder = controls.decoder.subscribe();
    let mut catalog = broadcast.catalog();
    let mut epoch = broadcast.epoch();
    let mut player = status.watch.watch();
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
        let auto = matches!(*mode.borrow(), RenditionMode::Auto { .. });
        let ticking = (auto && network.is_some())
            || backoffs.any_excluded(Instant::now())
            || vanished_since.is_some();
        if !std::mem::take(&mut first) {
            tokio::select! {
                () = shutdown.cancelled() => return,
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
                updated = catalog.updated() => {
                    if updated.is_err() {
                        return;
                    }
                    // A new catalog means the publisher republished, so a
                    // video that ended is rebuilt.
                    if ended.take().is_some() {
                        restart += 1;
                        revive_at = None;
                    }
                }
                updated = epoch.updated() => {
                    if updated.is_err() {
                        return;
                    }
                    clock.restart();
                }
                updated = player.updated() => if updated.is_err() { return },
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
                            backoffs.fail(rendition, Instant::now());
                            info!(%rendition, after = ?EXCLUSION, "leaving a failing rendition alone");
                        }
                        failed = Some(reported);
                    }
                },
                _ = ticker.tick(), if ticking => {}
            }
        }

        let now = Instant::now();

        let mode = mode.borrow().clone();
        let Some(catalog) = catalog.peek().clone() else {
            continue;
        };
        let Some(consumer) = epoch.peek().consumer.clone() else {
            continue;
        };

        let latency: Latency = *latency.borrow();
        let config = Config {
            decoder: decoder.borrow().clone(),
            max_age: latency.max,
            epoch: epoch.peek().generation,
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

        let sample = network.as_ref().map(|network| network.0.sample());
        let on_screen = status.get().rendition;
        let nothing_playing = on_screen.is_none();
        // The bound weighs its target against what this selector last asked
        // for, on screen or on its way. Weighed against the old rendition still
        // on screen, each pass would restart the hold and undo the switch.
        let current = desired
            .borrow()
            .as_ref()
            .map(|desired| desired.target.rendition.clone())
            .or_else(|| on_screen.clone());

        let (choice, why) = choose(
            &mode,
            &catalog,
            &backoffs.excluded(now),
            current.as_deref(),
            on_screen.as_deref(),
            sample.as_ref(),
            &mut bound,
            now,
        );
        let why_changed = !same_error(&last_why, &why);
        status.update(|status| {
            if why_changed {
                status.switch_error = why.clone();
            }
            match (&mode, &status.video) {
                (RenditionMode::Off, _) => status.video = SlotState::Off,
                (_, SlotState::Off) => status.video = SlotState::Starting,
                // Video came back to a catalog that had none.
                (_, SlotState::Ended) if choice.is_some() => status.video = SlotState::Starting,
                _ => {}
            }
        });
        if why_changed {
            last_why = why;
        }
        let ranked = catalog.ranked_video();
        let rank = |name: &str| ranked.iter().position(|(known, _)| *known == name);
        let step_down = matches!(mode, RenditionMode::Auto { .. })
            && match (choice.as_deref(), on_screen.as_deref()) {
                (Some(choice), Some(playing)) => rank(choice) > rank(playing),
                _ => false,
            };
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
            && !matches!(mode, RenditionMode::Off)
            && status.get().video != SlotState::Ended;
        if vanished {
            let since = *vanished_since.get_or_insert(now);
            if now.duration_since(since) < VANISH_GRACE {
                continue;
            }
            debug!("the catalog has had no video for a while; the video is over");
            status.update(|status| status.video = SlotState::Ended);
        }
        vanished_since = None;
        // Ask again for a target given up on once its backoff is over. A
        // configuration that failed beside a working one waits until nothing
        // plays.
        let retry = match (&failed, &next) {
            (Some(failed), Some(next)) => {
                failed.target == next.target
                    && !backoffs.is_excluded(&failed.target.rendition, now)
                    && (!failed.config_only || nothing_playing)
            }
            _ => false,
        };
        desired.send_if_modified(|desired| {
            let changed = desired.as_ref().map(|d| &d.target) != next.as_ref().map(|d| &d.target);
            if changed || retry {
                trace!(target = ?next.as_ref().map(|d| &d.target), retry, "desired rendition");
                *desired = next;
                failed = None;
            }
            changed || retry
        });
    }
}

/// Returns whether two optional errors say the same thing.
///
/// Compares messages, because the selector builds a fresh error on every pass.
fn same_error(left: &Option<Arc<Error>>, right: &Option<Arc<Error>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.to_string() == right.to_string(),
        (None, None) => true,
        _ => false,
    }
}

/// Picks the rendition to play, and why a pin could not be honoured.
#[allow(
    clippy::too_many_arguments,
    reason = "the rendition asked for and the one on screen are both weighed"
)]
fn choose(
    mode: &RenditionMode,
    catalog: &Catalog,
    excluded: &BTreeSet<String>,
    current: Option<&str>,
    on_screen: Option<&str>,
    sample: Option<&crate::NetworkSample>,
    bound: &mut Bound,
    now: Instant,
) -> (Option<String>, Option<Arc<Error>>) {
    let rungs: Vec<Rung> = catalog
        .ranked_video()
        .into_iter()
        .map(|(name, config)| Rung {
            name: name.to_string(),
            bitrate: config.bitrate,
            height: config.coded_height,
            stalled: config.stalled.unwrap_or(false),
        })
        .collect();
    let (max_height, why) = match mode {
        RenditionMode::Off => return (None, None),
        RenditionMode::Pinned(name) => {
            if excluded.contains(name) {
                (
                    None,
                    Some(Arc::new(Error::decoder(std::io::Error::other(format!(
                        "the decoder for {name} failed; playing another rendition until it \
                         is retried"
                    ))))),
                )
            } else if let Err(err) = catalog.video_rendition(name) {
                (None, Some(Arc::new(err)))
            } else {
                return (Some(name.clone()), None);
            }
        }
        RenditionMode::Auto { max_height } => (*max_height, None),
    };
    let constraints = Constraints {
        max_height,
        excluded: excluded.clone(),
    };
    let choice = match sample {
        Some(sample) => bound.decide(&rungs, current, on_screen, &constraints, sample, now),
        // Nothing to adapt to: the best rendition allowed plays.
        None => best(&rungs, &constraints),
    };
    (choice, why)
}

/// Returns the best rendition the constraints allow, or the smallest if none is.
fn best(rungs: &[Rung], constraints: &Constraints) -> Option<String> {
    rungs
        .iter()
        .find(|rung| constraints.allows(rung))
        .or_else(|| rungs.last())
        .map(|rung| rung.name.clone())
}

#[cfg(test)]
mod tests {
    use hang::catalog::{H264, VideoCodec, VideoConfig};

    use super::*;

    fn catalog() -> Catalog {
        let mut hang = hang::catalog::Catalog::default();
        for (name, height, bitrate) in [("1080p", 1080, 4_000_000), ("360p", 360, 500_000)] {
            let mut config = VideoConfig::new(VideoCodec::H264(H264 {
                inline: true,
                profile: 0x64,
                constraints: 0,
                level: 0x1f,
            }));
            config.coded_width = Some(height * 16 / 9);
            config.coded_height = Some(height);
            config.bitrate = Some(bitrate);
            hang.video.renditions.insert(name.to_string(), config);
        }
        Catalog::from(hang)
    }

    fn pick(mode: &RenditionMode, excluded: &[&str]) -> (Option<String>, Option<Arc<Error>>) {
        let excluded = excluded.iter().map(|name| name.to_string()).collect();
        let mut bound = Bound::new(Adaptation::default());
        choose(
            mode,
            &catalog(),
            &excluded,
            None,
            None,
            None,
            &mut bound,
            Instant::now(),
        )
    }

    #[test]
    fn auto_without_signals_plays_the_best() {
        assert_eq!(
            pick(&RenditionMode::auto(), &[]).0.as_deref(),
            Some("1080p")
        );
    }

    #[test]
    fn auto_respects_the_height_limit() {
        assert_eq!(
            pick(&RenditionMode::auto_below(720), &[]).0.as_deref(),
            Some("360p")
        );
    }

    #[test]
    fn a_pin_plays_its_rendition() {
        let (choice, why) = pick(&RenditionMode::pinned("360p"), &[]);
        assert_eq!(choice.as_deref(), Some("360p"));
        assert!(why.is_none());
    }

    /// A pin to a missing rendition falls back to automatic and says why.
    #[test]
    fn a_pin_to_a_missing_rendition_falls_back_and_says_why() {
        let (choice, why) = pick(&RenditionMode::pinned("4k"), &[]);
        assert_eq!(choice.as_deref(), Some("1080p"));
        assert!(matches!(
            why.as_deref(),
            Some(Error::UnknownRendition { .. })
        ));
    }

    #[test]
    fn a_pin_to_a_failing_rendition_falls_back_until_it_is_retried() {
        let (choice, why) = pick(&RenditionMode::pinned("1080p"), &["1080p"]);
        assert_eq!(choice.as_deref(), Some("360p"));
        assert!(why.is_some());
    }

    #[test]
    fn off_plays_nothing() {
        assert_eq!(pick(&RenditionMode::Off, &[]).0, None);
    }

    /// A running selector and what drives it.
    struct Running {
        _broadcast: crate::LocalBroadcast,
        reports: mpsc::Sender<Report>,
        desired: watch::Receiver<Option<Desired>>,
        _playing: watch::Sender<Option<Target>>,
        _task: n0_future::task::AbortOnDropHandle<()>,
    }

    /// Starts a selector over a one-rendition broadcast.
    fn selector() -> Running {
        let broadcast = crate::LocalBroadcast::new();
        broadcast
            .set_video(
                crate::VideoSource::test_pattern(
                    video::Size::new(320, 180),
                    video::Rate::new(30, 1).expect("a valid rate"),
                ),
                crate::VideoEncoding {
                    prefer_hardware: false,
                    ..crate::VideoEncoding::single(crate::VideoRendition::new("video"))
                },
            )
            .expect("a valid encoding");
        let (reports_tx, reports) = mpsc::channel(8);
        // Nothing is ever on screen, which a revival requires.
        let (playing_tx, playing) = watch::channel(None);
        let (desired, desired_rx) = watch::channel(None);
        let inputs = Inputs {
            broadcast: RemoteBroadcast::local(&broadcast),
            controls: Arc::new(Controls {
                mode: watch::Sender::new(RenditionMode::auto()),
                latency: watch::Sender::new(Latency::default()),
                decoder: watch::Sender::new(video::decode::Kind::Software),
                volume: watch::Sender::new(1.0),
            }),
            status: StatusCell::new(super::super::PlayerStatus::default()),
            reports,
            playing,
            desired,
            clock: super::super::PlayoutClock::new(Duration::from_millis(100)),
            adaptation: Adaptation::default(),
            shutdown: CancellationToken::new(),
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
                    .map(|d| d.target.clone())
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

    #[test]
    fn a_failing_rendition_is_skipped_by_auto() {
        assert_eq!(
            pick(&RenditionMode::auto(), &["1080p"]).0.as_deref(),
            Some("360p")
        );
    }
}
