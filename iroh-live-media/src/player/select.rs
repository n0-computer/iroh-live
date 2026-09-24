//! The selector: which rendition should play, under which decoder settings.
//!
//! Turns the rendition mode, the catalog, the network and the decoders'
//! failures into one [`Desired`] value the video supervisor follows. Nothing
//! here opens a decoder; a changed desire is all it produces.

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
    Controls, Latency, PlaybackRecorder, RenditionMode, StatusCell,
    bound::{Bound, Constraints, Reading, Rung, Tuning},
    switch::Target,
};
use crate::{Catalog, RemoteBroadcast, SlotState, error::Error, video};

/// How often the network is read while it can change the choice.
const TICK: Duration = Duration::from_millis(200);

/// How long a rendition whose decoder failed is left alone, the first time.
const BACKOFF_FIRST: Duration = Duration::from_secs(5);

/// The longest a failing rendition is left alone.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long after a track ended with nothing to follow it the target is asked
/// for again, doubling up to [`REVIVE_MAX`].
const REVIVE_FIRST: Duration = Duration::from_secs(1);

/// The longest wait before a target whose track ended is asked for again.
const REVIVE_MAX: Duration = Duration::from_secs(30);

/// How long a catalog that lost its video is given before the video counts as
/// over.
///
/// A publisher replacing its video removes the old renditions before it adds
/// the new ones, and one that closes empties its catalog before it ends, which
/// a player following a route table sees just before the next route serves the
/// path. Either is a moment, not the end of the video.
const VANISH_GRACE: Duration = Duration::from_secs(3);

/// What the decoder of a target is built with.
#[derive(Clone)]
pub(crate) struct DecodeSettings {
    /// The broadcast to read.
    pub consumer: moq_net::broadcast::Consumer,
    /// The backend to open.
    pub decoder: video::decode::Kind,
    /// The oldest media still worth decoding.
    pub max_age: Duration,
}

impl std::fmt::Debug for DecodeSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodeSettings")
            .field("decoder", &self.decoder)
            .field("max_age", &self.max_age)
            .finish_non_exhaustive()
    }
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
}

/// A decoder that failed, as the supervisor reports it.
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    /// The rendition and configuration it was for.
    pub target: Target,
    /// Whether it only changed the decoder configuration of the rendition
    /// already playing.
    ///
    /// Such a failure says the configuration is broken, not the rendition:
    /// the incumbent keeps playing under the configuration that works, and the
    /// rendition is not excluded, which would only walk the ladder down under
    /// the same broken configuration.
    pub config_only: bool,
    /// Whether the rendition is left alone for a backoff before it is tried
    /// again.
    ///
    /// Not for a first decoder that timed out: with nothing on screen there is
    /// nothing better to play meanwhile, and a slow link is not a broken
    /// rendition, so it is asked for again at once.
    pub exclude: bool,
}

/// What the supervisor tells the selector about the decoders it ran.
#[derive(Debug, Clone)]
pub(crate) enum Report {
    /// A decoder failed: it did not open, did not produce a picture in time,
    /// or gave up on its track.
    Failed(Failure),
    /// A track ended cleanly with nothing to take over from it.
    ///
    /// The publisher replaced or withdrew its video, or the route to it
    /// changed. The catalog or the route usually says what comes next, but
    /// not always in an order that shows it: a replacement's catalog can land
    /// before the old track's end. So the selector asks for the target again
    /// after a backoff as well.
    Ended(Target),
}

/// The selector's inputs.
pub(crate) struct Inputs {
    pub broadcast: RemoteBroadcast,
    pub controls: Arc<Controls>,
    pub status: StatusCell,
    pub stats: PlaybackRecorder,
    /// What happened to the decoders, reported by the supervisor.
    pub reports: mpsc::Receiver<Report>,
    /// The target on screen, as the supervisor reports it.
    pub playing: watch::Receiver<Option<Target>>,
    pub desired: watch::Sender<Option<Desired>>,
    /// The player's playout clock, started over on a new route.
    pub clock: super::PlayoutClock,
    pub shutdown: CancellationToken,
}

/// One failing rendition's backoff.
#[derive(Debug)]
struct Excluded {
    until: Instant,
    backoff: Duration,
}

/// The renditions whose decoders failed, and how long each is left alone.
///
/// An entry outlives its exclusion: the rendition can only be tried again once
/// its exclusion is over, so an entry dropped at that point would make every
/// retry a first failure, and a rendition that never decodes would be retried
/// at the first backoff forever. Entries go only when the rendition lands, or
/// when the decoder configuration changes and every rendition deserves a fresh
/// try.
#[derive(Debug, Default)]
struct Backoffs(BTreeMap<String, Excluded>);

impl Backoffs {
    /// Records a failure of `rendition` at `now`, doubling its backoff, and
    /// returns the backoff.
    fn fail(&mut self, rendition: &str, now: Instant) -> Duration {
        let entry = self.0.entry(rendition.to_string()).or_insert(Excluded {
            until: now,
            backoff: BACKOFF_FIRST / 2,
        });
        entry.backoff = (entry.backoff * 2).min(BACKOFF_MAX);
        entry.until = now + entry.backoff;
        entry.backoff
    }

    /// Returns the renditions left alone at `now`.
    fn excluded(&self, now: Instant) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|(_, entry)| entry.until > now)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Reports whether `rendition` is left alone at `now`.
    fn is_excluded(&self, rendition: &str, now: Instant) -> bool {
        self.0.get(rendition).is_some_and(|entry| entry.until > now)
    }

    /// Reports whether any rendition is left alone at `now`.
    fn any_excluded(&self, now: Instant) -> bool {
        self.0.values().any(|entry| entry.until > now)
    }

    /// Forgets `rendition`'s failures, as it plays.
    fn landed(&mut self, rendition: &str) {
        self.0.remove(rendition);
    }

    /// Forgets every failure, as the decoder configuration changed.
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
    /// Bumped to rebuild a video that ended, when the catalog moves.
    restart: u64,
}

/// Runs the selector until the player is dropped.
pub(crate) async fn run(inputs: Inputs) {
    let Inputs {
        broadcast,
        controls,
        status,
        stats,
        mut reports,
        mut playing,
        desired,
        clock,
        shutdown,
    } = inputs;
    let mut mode = controls.mode.subscribe();
    let mut latency = controls.latency.subscribe();
    let mut decoder = controls.decoder.subscribe();
    let mut catalog = broadcast.catalog();
    let mut epoch = broadcast.epoch();
    let mut player = status.watch.watch();
    let network = broadcast.network();

    let mut bound = Bound::new(Tuning::default());
    let mut backoffs = Backoffs::default();
    let mut generation = 0u64;
    let mut generations = 0u64;
    let mut last_config: Option<Config> = None;
    // Every configuration since the one on screen, by generation, so a failed
    // change of decoder can go back to the configuration that works.
    let mut configs: BTreeMap<u64, Config> = BTreeMap::new();
    // The generation of the target on screen.
    let mut working: Option<u64> = None;
    // The decoder played instead of the one asked for, after the one asked for
    // failed beside a working one; cleared when another is asked for.
    let mut fallback: Option<video::decode::Kind> = None;
    let mut restart = 0u64;
    // The target whose track ended with nothing to follow it, and when it is
    // asked for again if neither the catalog nor the route moves first.
    let mut ended: Option<Target> = None;
    let mut revive_at: Option<Instant> = None;
    let mut revive_backoff = REVIVE_FIRST;
    // Since when the target on screen has played, so a track that plays for
    // a while earns a quick revival and one that ends at once backs off.
    let mut playing_since: Option<Instant> = None;
    // Since when the catalog has had nothing to play while something was
    // asked for.
    let mut vanished_since: Option<Instant> = None;
    // The reason for a pin this selector could not honour, as last written, so
    // it replaces only its own reports and leaves a failed switch's alone.
    let mut last_why: Option<Arc<Error>> = None;
    // The last target the supervisor gave up on, and whether it only changed
    // the configuration of the rendition playing. The desired value does not
    // change when the same target is chosen again after its backoff, so this
    // is what asks the supervisor for it once more: without it, a first decoder
    // that failed to open was never tried again.
    let mut failed: Option<Failure> = None;
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // The first pass decides at once: a broadcast whose catalog arrived before
    // the player started changes nothing the loop below waits on, so waiting
    // first would leave such a player without video until something else
    // moved, which with no audio and no network signals is never.
    let mut first = true;
    loop {
        let auto = matches!(*mode.borrow(), RenditionMode::Auto { .. });
        let ticking = (auto && network.is_some())
            || backoffs.any_excluded(Instant::now())
            || vanished_since.is_some();
        if !std::mem::take(&mut first) {
            tokio::select! {
                () = shutdown.cancelled() => return,
                // Returning drops the desired rendition's sender, which ends the
                // video task and with it the player's frames: a reader waiting on
                // `next()` sees the end rather than waiting forever.
                () = broadcast.closed() => {
                    debug!("the broadcast closed");
                    return;
                }
                changed = mode.changed() => if changed.is_err() { return },
                changed = latency.changed() => if changed.is_err() { return },
                changed = decoder.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    fallback = None;
                }
                changed = playing.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if let Some(target) = playing.borrow_and_update().clone() {
                        // A rendition that plays has recovered: its next
                        // failure starts again from the first backoff.
                        backoffs.landed(&target.rendition);
                        working = Some(target.config);
                        configs.retain(|&known, _| known >= target.config);
                        playing_since = Some(Instant::now());
                        ended = None;
                        revive_at = None;
                    }
                }
                updated = catalog.updated() => {
                    if updated.is_err() {
                        return;
                    }
                    // A video that ended gets another go when the publisher
                    // republishes, which is what a new catalog says.
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
                    // Asked for again only if nothing moved meanwhile: a new
                    // route or catalog has asked for something already.
                    let still = desired
                        .borrow()
                        .as_ref()
                        .is_some_and(|desired| Some(&desired.target) == ended.as_ref());
                    if ended.take().is_some() && still && playing.borrow().is_none() {
                        debug!(backoff = ?revive_backoff, "asking again for video that ended");
                        restart += 1;
                        revive_backoff = (revive_backoff * 2).min(REVIVE_MAX);
                    }
                }
                report = reports.recv() => match report {
                    None => return,
                    Some(Report::Ended(target)) => {
                        if playing_since.is_some_and(|since| since.elapsed() >= REVIVE_MAX) {
                            revive_backoff = REVIVE_FIRST;
                        }
                        playing_since = None;
                        debug!(rendition = %target.rendition, backoff = ?revive_backoff, "video ended, asking again after a backoff");
                        revive_at = Some(Instant::now() + revive_backoff);
                        ended = Some(target);
                    }
                    Some(Report::Failed(reported)) => {
                    let rendition = &reported.target.rendition;
                    if reported.config_only {
                        // Go back to the decoder that works, so the next switch
                        // opens under it rather than under the one that failed,
                        // which would walk the ladder down one failure at a time.
                        let restored = working
                            .and_then(|working| configs.get(&working))
                            .map(|config| config.decoder.clone());
                        info!(
                            %rendition,
                            ?restored,
                            "the new decoder failed; going back to the one that works"
                        );
                        if restored.is_some() {
                            fallback = restored;
                        }
                    } else if reported.exclude {
                        let backoff = backoffs.fail(rendition, Instant::now());
                        info!(%rendition, ?backoff, "leaving a failing rendition alone");
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
        let effective = fallback.clone().unwrap_or_else(|| decoder.borrow().clone());
        let config = Config {
            decoder: effective.clone(),
            max_age: latency.max,
            epoch: epoch.peek().generation,
            restart,
        };
        if last_config.as_ref() != Some(&config) {
            // Back to the configuration on screen keeps its generation, so the
            // supervisor sees the target playing and opens nothing.
            generation = match working.filter(|working| configs.get(working) == Some(&config)) {
                Some(working) => working,
                None => {
                    generations += 1;
                    configs.insert(generations, config.clone());
                    // Bounded while nothing lands: the one on screen and the
                    // last few are all a fallback can want.
                    configs.retain(|&known, _| Some(known) == working || known + 16 > generations);
                    generations
                }
            };
            debug!(generation, ?config, "decoder configuration changed");
            last_config = Some(config);
            // A rendition that failed under the old configuration deserves a
            // try under the new one.
            backoffs.clear();
        }
        let settings = DecodeSettings {
            consumer,
            decoder: effective,
            max_age: latency.max,
        };

        let sample = network.as_ref().map(|network| network.0.sample());
        stats.network.update(|last| *last = sample);
        let on_screen = status.get().rendition;
        let nothing_playing = on_screen.is_none();
        // The bound weighs its target against what this selector last asked
        // for, which is on screen or on its way. While a replacement warms up,
        // the rendition on screen is still the old one: weighed against that,
        // the next pass restarted the downgrade hold and took the decision
        // straight back, and on a real link a switch never landed.
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
        let next = choice.and_then(|name| {
            let config = catalog.hang_video(&name)?.clone();
            Some(Desired {
                target: Target::new(name, generation),
                settings: settings.clone(),
                config,
            })
        });
        let vanished =
            next.is_none() && !matches!(mode, RenditionMode::Off) && desired.borrow().is_some();
        if vanished {
            let since = *vanished_since.get_or_insert(now);
            if now.duration_since(since) < VANISH_GRACE {
                continue;
            }
            debug!("the catalog has had no video for a while; the video is over");
        }
        vanished_since = None;
        // The target given up on is asked for again once it is chosen with its
        // backoff over, or, for a configuration that failed beside a working
        // one, once nothing plays any more.
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

/// Whether two optional errors say the same thing.
///
/// Compared by message rather than identity, because the selector builds a
/// fresh error on every pass for a reason that has not changed.
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
        .video()
        .iter()
        .map(|info| Rung {
            name: info.name.clone(),
            bitrate: info.bitrate.map(crate::Bitrate::as_bps),
            height: info.height(),
            stalled: info.stalled,
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
            } else if catalog.video_rendition(name).is_none() {
                (
                    None,
                    Some(Arc::new(n0_error::e!(Error::UnknownRendition {
                        name: name.clone()
                    }))),
                )
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
        Some(sample) => bound.decide(
            &rungs,
            current,
            on_screen,
            &constraints,
            &Reading {
                loss: sample.loss.map(f64::from),
                delivery: sample.delivery.map(crate::Bitrate::as_bps),
                path_generation: sample.path_generation,
            },
            now,
        ),
        // Nothing to adapt to: the best rendition allowed plays.
        None => best(&rungs, &constraints),
    };
    (choice, why)
}

/// The best rendition the constraints allow, or the smallest when none is.
fn best(rungs: &[Rung], constraints: &Constraints) -> Option<String> {
    rungs
        .iter()
        .find(|rung| {
            !rung.stalled
                && !constraints.excluded.contains(&rung.name)
                && match (constraints.max_height, rung.height) {
                    (Some(max), Some(height)) => height <= max,
                    _ => true,
                }
        })
        .or_else(|| rungs.last())
        .map(|rung| rung.name.clone())
}

#[cfg(test)]
mod tests {
    use hang::catalog::{H264, VideoCodec, VideoConfig};

    use super::*;

    fn catalog() -> Catalog {
        let mut hang = crate::catalog::HangCatalog::default();
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
        Catalog::new(hang)
    }

    fn pick(mode: &RenditionMode, excluded: &[&str]) -> (Option<String>, Option<Arc<Error>>) {
        let excluded = excluded.iter().map(|name| name.to_string()).collect();
        let mut bound = Bound::new(Tuning::default());
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

    /// A pin that cannot be honoured falls back to automatic selection and
    /// says why, rather than holding a rendition that never plays.
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
                crate::VideoEncoding::single(crate::VideoRendition::new("video"))
                    .with_prefer_hardware(false),
            )
            .expect("a valid encoding");
        let (reports_tx, reports) = mpsc::channel(8);
        // Nothing is ever on screen, which is the state the revival acts in.
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
            stats: PlaybackRecorder::default(),
            reports,
            playing,
            desired,
            clock: super::super::PlayoutClock::new(),
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

    /// N2: a track that ended with nothing to follow it was asked for again
    /// only when a catalog update came after the end. The replacement's
    /// catalog can land first, and then nothing more ever comes: the video
    /// stayed over for good. The target is asked for again after a backoff,
    /// with the catalog unchanged.
    #[tokio::test]
    async fn a_track_that_ended_is_asked_for_again() {
        // Bound whole: a field left out of a pattern would drop at once, and
        // with it the channel that keeps the selector running.
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

    /// N1: an entry used to be dropped    /// N1: an entry used to be dropped the moment its exclusion ran out, and
    /// a rendition can only be retried after that, so every retry that failed
    /// was a first failure and the backoff never grew past its first step.
    #[test]
    fn the_backoff_grows_across_retries() {
        let mut backoffs = Backoffs::default();
        let mut now = Instant::now();
        let mut seen = Vec::new();
        for _ in 0..5 {
            let backoff = backoffs.fail("high", now);
            seen.push(backoff);
            assert!(backoffs.is_excluded("high", now));
            // The retry comes once the exclusion is over, and fails again.
            now += backoff;
            assert!(
                !backoffs.is_excluded("high", now),
                "still excluded at retry"
            );
            assert!(backoffs.excluded(now).is_empty());
        }
        assert_eq!(
            seen,
            [5, 10, 20, 40, 60].map(Duration::from_secs),
            "the backoff did not grow"
        );
        // Playing resets it.
        backoffs.landed("high");
        assert_eq!(backoffs.fail("high", now), BACKOFF_FIRST);
    }

    #[test]
    fn a_failing_rendition_is_skipped_by_auto() {
        assert_eq!(
            pick(&RenditionMode::auto(), &["1080p"]).0.as_deref(),
            Some("360p")
        );
    }
}
