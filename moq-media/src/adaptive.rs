//! Adaptive rendition switching for video tracks.
//!
//! The selection algorithm ([`evaluate`]), the ranking ([`rank_renditions`])
//! and the upgrade bet ([`Probe`]) are used by
//! [`VideoTrack::enable_adaptation`](crate::subscribe::VideoTrack::enable_adaptation)
//! to decide when to switch renditions based on [`NetworkSignals`].
//!
//! Almost nothing QUIC reports describes the direction a subscriber cares about.
//! Loss, congestion events and the congestion window all cover what this
//! endpoint sends, and a subscriber sends little but acknowledgements. The two
//! figures that reach across are the bytes that arrived and the round trip, and
//! the rules here are built out of those two wherever a decision has to hold on
//! a link that is only saturated one way.
//!
//! Both of those are read against a baseline rather than in absolute terms, so
//! both describe change rather than level: what this rendition was delivering a
//! moment ago, and what the round trip was with nothing queued in front of it.
//! A subscriber that joins a link already at its limit measures that limit as
//! the normal state and holds where it started, which is as far as either
//! figure goes. Tightening the limit from there is visible, and so is lifting
//! it, which is what [`Decision::StartProbe`] is for.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use hang::catalog::VideoConfig;

use crate::net::NetworkSignals;

// --- Configuration ---------------------------------------------------

/// Thresholds and timers for the adaptation algorithm.
#[derive(Debug, Clone)]
pub struct AdaptiveConfig {
    /// Sustained good conditions required before starting an upgrade probe.
    pub upgrade_hold: Duration,
    /// Sustained bad conditions before downgrading.
    pub downgrade_hold: Duration,
    /// How long a probe runs before committing or aborting.
    pub probe_duration: Duration,
    /// Cooldown after a failed probe before retrying.
    pub probe_cooldown: Duration,
    /// Cooldown after any downgrade before upgrade probes are allowed.
    pub post_downgrade_cooldown: Duration,
    /// Loss rate above which downgrade is triggered (sustained).
    pub loss_downgrade: f64,
    /// Loss rate above which emergency drop to lowest occurs (immediate).
    pub loss_emergency: f64,
    /// Loss rate below which conditions are considered good.
    pub loss_good: f64,
    /// Loss rate above which an active probe is aborted.
    pub loss_probe_abort: f64,
    /// Fraction of what the current rendition was delivering over a healthy
    /// path that must still be arriving for the link to count as keeping up
    /// (e.g. 0.75 = three quarters).
    ///
    /// Measured against the rendition's own recent history rather than against
    /// the bitrate it declares, because a declared bitrate is a ceiling handed
    /// to the encoder and openh264 was measured spending about 40% of it. A
    /// ratio against the ceiling is therefore below any threshold worth setting
    /// on a link with nothing wrong with it.
    pub goodput_downgrade_ratio: f64,
    /// How long the healthy-goodput reference takes to halve towards a lower
    /// rate while the path stays healthy.
    ///
    /// The reference is a peak, so without decay a scene that quietened down
    /// for good would leave it stranded at what the busy scene cost and every
    /// later reading short of it.
    pub goodput_baseline_halflife: Duration,
    /// Distinct round trip readings that must show a queue before the queueing
    /// branch of the downgrade rule fires.
    ///
    /// Elapsed time is no evidence for a signal that is not measured again while
    /// it elapses. [`NetworkSignals::rtt`] is handed out unchanged until QUIC
    /// takes another sample, so a single spurious reading satisfies any
    /// wall-clock hold by itself. Counting readings instead is what
    /// [`AdaptiveConfig::downgrade_hold`] cannot do here.
    pub queueing_samples: u32,
    /// How long a probe's rendition must have been playing before its goodput
    /// is judged.
    ///
    /// The handover briefly carries the incumbent and the replacement at once
    /// and then neither, so a goodput window spanning it measures the switch
    /// rather than the link. Long enough for the window to have refilled from
    /// the new rendition alone.
    pub probe_settle: Duration,
    /// Multiple of the path's minimum round trip above which the path counts as
    /// queueing (e.g. 2.0 = twice the idle round trip).
    pub rtt_queueing_ratio: f64,
    /// Round trip in excess of the path's minimum below which the path is never
    /// called queueing, whatever [`AdaptiveConfig::rtt_queueing_ratio`] says.
    ///
    /// A link with a sub-millisecond idle round trip doubles it on the ordinary
    /// business of carrying a video frame, so the ratio on its own would call
    /// every healthy local path congested.
    pub rtt_queueing_floor: Duration,
    /// How often the adaptation task checks signals.
    pub check_interval: Duration,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            upgrade_hold: Duration::from_secs(4),
            downgrade_hold: Duration::from_millis(500),
            probe_duration: Duration::from_secs(3),
            probe_cooldown: Duration::from_secs(8),
            post_downgrade_cooldown: Duration::from_secs(4),
            loss_downgrade: 0.10,
            loss_emergency: 0.20,
            loss_good: 0.02,
            loss_probe_abort: 0.05,
            goodput_downgrade_ratio: 0.75,
            goodput_baseline_halflife: Duration::from_secs(30),
            queueing_samples: 2,
            probe_settle: Duration::from_millis(1500),
            rtt_queueing_ratio: 2.0,
            rtt_queueing_floor: Duration::from_millis(25),
            check_interval: Duration::from_millis(200),
        }
    }
}

// --- Rendition ranking -----------------------------------------------

/// Rendition ranked by quality. Index 0 = highest quality.
#[derive(Debug, Clone)]
pub struct RankedRendition {
    /// Catalog key (track name).
    pub name: String,
    /// Total pixel count (`coded_width * coded_height`).
    pub pixels: u64,
    /// Advertised bitrate in bits per second.
    pub bitrate_bps: u64,
    /// Coded width from the catalog, or 0 if it declared none.
    pub width: u32,
    /// Coded height from the catalog, or 0 if it declared none.
    pub height: u32,
}

/// Ranks video renditions by pixel count descending (highest quality first).
pub fn rank_renditions(renditions: &BTreeMap<String, VideoConfig>) -> Vec<RankedRendition> {
    let mut ranked: Vec<_> = renditions
        .iter()
        .map(|(name, config)| {
            let w = config.coded_width.unwrap_or(0);
            let h = config.coded_height.unwrap_or(0);
            RankedRendition {
                name: name.clone(),
                pixels: w as u64 * h as u64,
                bitrate_bps: config.bitrate.unwrap_or(0),
                width: w,
                height: h,
            }
        })
        .collect();
    ranked.sort_by_key(|b| std::cmp::Reverse(b.pixels));
    ranked
}

// --- Selection logic -------------------------------------------------

/// Decision produced by the adaptation algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Stay on the current rendition.
    Hold,
    /// Switch to a lower rendition at the given index.
    Downgrade(usize),
    /// Emergency drop to the lowest rendition.
    Emergency,
    /// Start a probe for the rendition at the given index.
    StartProbe(usize),
}

/// Mutable state tracked across evaluation ticks.
#[derive(Debug, Default)]
pub struct AdaptationTimers {
    /// When bad conditions were first detected, against
    /// [`AdaptiveConfig::downgrade_hold`].
    pub bad_since: Option<Instant>,
    /// When good conditions were first detected, against
    /// [`AdaptiveConfig::upgrade_hold`].
    pub good_since: Option<Instant>,
    /// When the last downgrade occurred.
    pub last_downgrade: Option<Instant>,
    /// When the last probe attempt, successful or not, occurred.
    pub last_probe: Option<Instant>,
    /// What the rendition now playing delivers when nothing is in its way.
    healthy: Option<HealthyGoodput>,
    /// Round trip readings showing a queue since [`AdaptationTimers::bad_since`],
    /// counted so that one reading held across many ticks cannot pass for
    /// several.
    queueing_seen: u32,
    /// The [`NetworkSignals::rtt_samples`] value the last counted reading came
    /// from.
    counted_rtt_samples: u64,
}

/// What a rendition delivers over a path with nothing in its way.
///
/// The reference the downgrade rule measures a shortfall against, because the
/// catalog cannot supply one. A declared bitrate is a ceiling handed to the
/// encoder, and openh264 was measured spending some 40% of it, so a rendition
/// arriving exactly as intended still reads as a link at two fifths of its rung.
/// What a subscriber can establish for itself is what this rung was delivering a
/// moment ago, which is a figure in the same units as the one it is compared to.
///
/// A peak rather than a mean, because the comparison is against a rendition
/// arriving in full and content that got easier is not that.
#[derive(Debug)]
struct HealthyGoodput {
    /// The rendition it was measured on. A different rung delivers a different
    /// rate, so the reference does not survive a switch.
    rendition: String,
    /// The peak, decayed since it was last raised.
    bps: f64,
    /// When `bps` last moved, so the decay runs against elapsed time rather than
    /// against a count of ticks.
    at: Instant,
}

impl AdaptationTimers {
    /// Returns what the rendition now playing delivers over a healthy path, or
    /// `None` if it has not been measured since the last switch.
    pub fn healthy_goodput(&self) -> Option<u64> {
        self.healthy.as_ref().map(|healthy| healthy.bps as u64)
    }

    /// Folds `bps` into the reference for `rendition`, decaying the peak first.
    ///
    /// Called only on a tick where the path is healthy: a reading taken through
    /// a queue would teach the rule that the capped rate is the normal one, and
    /// leave nothing to react to.
    fn record_healthy_goodput(
        &mut self,
        rendition: &str,
        bps: u64,
        halflife: Duration,
        now: Instant,
    ) {
        match &mut self.healthy {
            Some(healthy) if healthy.rendition == rendition => {
                let halved = now.duration_since(healthy.at).as_secs_f64()
                    / halflife.as_secs_f64().max(f64::MIN_POSITIVE);
                healthy.bps = (healthy.bps * 0.5f64.powf(halved)).max(bps as f64);
                healthy.at = now;
            }
            _ => {
                self.healthy = Some(HealthyGoodput {
                    rendition: rendition.to_string(),
                    bps: bps as f64,
                    at: now,
                });
            }
        }
    }

    /// Drops the reference if it was measured on a rung other than `rendition`.
    fn forget_other_rendition(&mut self, rendition: &str) {
        if self
            .healthy
            .as_ref()
            .is_some_and(|healthy| healthy.rendition != rendition)
        {
            self.healthy = None;
        }
    }
}

/// Evaluates network signals and decides whether to switch renditions.
///
/// `current_idx` is the index into `ranked` of the rendition now playing.
/// `timers` carries the state the rules need across ticks and is updated in
/// place, so the same one has to be handed back on every call.
///
/// # Panics
///
/// Panics if `ranked` is empty, or if `current_idx` is out of range for it.
/// Callers rank the current catalog and locate the playing rendition in it on
/// every tick, which is what keeps the two in step.
pub fn evaluate(
    current_idx: usize,
    ranked: &[RankedRendition],
    signals: &NetworkSignals,
    timers: &mut AdaptationTimers,
    config: &AdaptiveConfig,
    now: Instant,
) -> Decision {
    let current = &ranked[current_idx];
    let is_lowest = current_idx == ranked.len() - 1;
    let is_highest = current_idx == 0;

    // --- Emergency: immediate drop to lowest -------------------------
    if signals.loss_rate >= config.loss_emergency && !is_lowest {
        timers.bad_since = None;
        timers.good_since = None;
        timers.last_downgrade = Some(now);
        return Decision::Emergency;
    }

    let queueing = queueing(signals, config);
    let loss_high = signals.loss_rate >= config.loss_downgrade;

    // What this rung delivers when there is nothing in its way, kept up to date
    // only while there is nothing in its way. A reading taken through a queue
    // would teach the rule that the capped rate is the normal one, leaving
    // nothing to react to.
    timers.forget_other_rendition(&current.name);
    if !queueing
        && !loss_high
        && let Some(bps) = signals.goodput_bps
    {
        timers.record_healthy_goodput(&current.name, bps, config.goodput_baseline_halflife, now);
    }

    // --- Downgrade check ---------------------------------------------
    // Two things have to hold at once, and neither is worth much alone.
    //
    // The rendition has stopped arriving at the rate it was arriving at a moment
    // ago, which says the rung is no longer being delivered in full. On its own
    // that is as easily a scene that got easier to encode.
    //
    // And the path is queueing, which is what a bottleneck does to it and what
    // easy content does not. On its own that is as easily a path that simply got
    // longer, or someone else's traffic in front of a link with room to spare.
    //
    // High loss stands alone, because it is recounted from fresh packet totals
    // on every tick and describes trouble a downgrade actually helps with.
    let starved = match (signals.goodput_bps, timers.healthy_goodput()) {
        (Some(bps), Some(healthy)) => {
            (bps as f64) < healthy as f64 * config.goodput_downgrade_ratio
        }
        // Nothing arriving, or nothing to compare it against. Absence of
        // evidence: a publisher that went quiet is not a link that failed, and a
        // rung that has never been seen arriving cleanly has no rate to fall
        // short of.
        _ => false,
    };

    if ((starved && queueing) || loss_high) && !is_lowest {
        let bad_since = match timers.bad_since {
            Some(bad_since) => {
                if queueing && signals.rtt_samples != timers.counted_rtt_samples {
                    timers.queueing_seen += 1;
                    timers.counted_rtt_samples = signals.rtt_samples;
                }
                bad_since
            }
            None => {
                timers.bad_since = Some(now);
                timers.queueing_seen = u32::from(queueing);
                timers.counted_rtt_samples = signals.rtt_samples;
                now
            }
        };
        // Elapsed time is evidence for loss, which is measured again on every
        // tick. It is no evidence at all for a queueing round trip, which is
        // handed out unchanged until QUIC takes another sample: a hold shorter
        // than the gap between samples is satisfied by one reading, so a single
        // scheduler hiccup on a path with a one-millisecond baseline is a
        // downgrade. Distinct readings are the debounce there.
        let corroborated = loss_high || timers.queueing_seen >= config.queueing_samples;
        if now.duration_since(bad_since) >= config.downgrade_hold && corroborated {
            timers.bad_since = None;
            timers.good_since = None;
            timers.last_downgrade = Some(now);
            return Decision::Downgrade(current_idx + 1);
        }
    } else {
        timers.bad_since = None;
    }

    // --- Upgrade check (probe gating) --------------------------------
    if is_highest {
        timers.good_since = None;
        return Decision::Hold;
    }

    // Cooldown after downgrade.
    if let Some(last_dg) = timers.last_downgrade
        && now.duration_since(last_dg) < config.post_downgrade_cooldown
    {
        return Decision::Hold;
    }
    // Cooldown after probe.
    if let Some(last_pr) = timers.last_probe
        && now.duration_since(last_pr) < config.probe_cooldown
    {
        return Decision::Hold;
    }

    // No bandwidth precondition, on purpose. A receiver sees only what was sent,
    // so goodput at the rung being played says the link carries at least that
    // much and nothing at all about what it would carry if asked for more.
    // Discovering headroom is what [`Decision::StartProbe`] is for, and the gate
    // is the absence of trouble rather than proof of room. What keeps that from
    // being a one-way ratchet is [`Probe::abort`], which judges the step up on
    // signals a receiver can actually move.
    //
    // Not gated on `queueing` either: the round trip is sampled so rarely on a
    // subscriber that a reading taken while the link was still bad outlives the
    // impairment by tens of seconds, and would hold the ladder down long after
    // there was anything holding it down.
    let loss_good = signals.loss_rate <= config.loss_good;

    if loss_good {
        let good_since = *timers.good_since.get_or_insert(now);
        if now.duration_since(good_since) >= config.upgrade_hold {
            timers.good_since = None;
            return Decision::StartProbe(current_idx - 1);
        }
    } else {
        timers.good_since = None;
    }

    Decision::Hold
}

/// Checks whether the round trip has grown enough over the path's minimum to
/// say a queue has built up in front of the bottleneck.
///
/// This is the one thing a subscriber sees about a downlink that has run out of
/// room. Loss and congestion events both describe the direction it sends in,
/// where it sends almost nothing and so runs out of nothing, while the queue
/// holding up the media holds up the acknowledgements behind it too.
fn queueing(signals: &NetworkSignals, config: &AdaptiveConfig) -> bool {
    // A zero minimum is an unmeasured one rather than an instant path: no real
    // link reports one, and taking it at face value collapses the ratio test
    // into "the round trip is above the floor", which condemns every path with
    // an ordinary intercontinental hop.
    if signals.min_rtt.is_zero() {
        return false;
    }
    let Some(excess) = signals.rtt.checked_sub(signals.min_rtt) else {
        return false;
    };
    excess >= config.rtt_queueing_floor
        && signals.rtt.as_secs_f64() >= signals.min_rtt.as_secs_f64() * config.rtt_queueing_ratio
}

/// An upgrade in progress: a step up the ladder that has not proved itself yet.
///
/// A probe is a bet that the link will carry more than it has so far been asked
/// for, which is the only way a receiver finds headroom: goodput says what
/// arrived and never what would have. The bet is watched and taken back on the
/// first sign it was wrong rather than left for the next downgrade timer.
#[derive(Debug)]
pub struct Probe {
    /// The rendition stepped up to.
    rendition: String,
    /// What the rung below was delivering over a healthy path, or `None` if it
    /// was never measured there.
    below_bps: Option<u64>,
    /// What the link looked like when `rendition` started playing, or `None`
    /// while the step up has not landed and so cannot be judged.
    landing: Option<Landing>,
}

/// What the link looked like when a probe's rendition started playing.
///
/// Anchored at the landing rather than at the request, so trouble on the rung
/// the probe had not left yet is not charged to it.
#[derive(Debug, Clone, Copy)]
struct Landing {
    at: Instant,
    congestion_events: u64,
    rtt_samples: u64,
}

/// Why an upgrade probe was taken back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeAbort {
    /// Loss reached [`AdaptiveConfig::loss_probe_abort`].
    Loss,
    /// This endpoint's own congestion controller backed off after the step up.
    Congestion,
    /// A round trip measured after the step up showed a queue.
    Queueing,
    /// The higher rung delivered less than the rung below it had.
    Goodput,
}

impl Probe {
    /// Starts a probe of `rendition`, stepping up from a rung that was
    /// delivering `below_bps` over a healthy path.
    pub fn new(rendition: String, below_bps: Option<u64>) -> Self {
        Self {
            rendition,
            below_bps,
            landing: None,
        }
    }

    /// Returns the rendition being probed.
    pub fn rendition(&self) -> &str {
        &self.rendition
    }

    /// Records that the probe's rendition has started playing, if it had not
    /// already.
    pub fn land(&mut self, signals: &NetworkSignals, now: Instant) {
        self.landing.get_or_insert(Landing {
            at: now,
            congestion_events: signals.congestion_events,
            rtt_samples: signals.rtt_samples,
        });
    }

    /// Returns why the probe should be taken back, or `None` to let it run.
    ///
    /// Loss and congestion are what this endpoint's own sending ran into, so on
    /// a subscriber they fail a probe only when both directions are impaired
    /// alike. The asymmetric case a probe is most likely to meet, a downlink
    /// that runs out of room while the uplink stays clear, moves neither, which
    /// on its own would leave every probe on such a path bound to succeed and
    /// the ladder free to climb whatever the link said. The other two reasons
    /// are the receiver's own: a queue that appeared after the step up, and a
    /// rung that costs more and delivers less than the one below it.
    ///
    /// Returns `None` for a probe that has not landed, which has nothing to be
    /// judged on yet.
    pub fn abort(
        &self,
        signals: &NetworkSignals,
        config: &AdaptiveConfig,
        now: Instant,
    ) -> Option<ProbeAbort> {
        let landing = self.landing?;
        if signals.loss_rate >= config.loss_probe_abort {
            return Some(ProbeAbort::Loss);
        }
        if signals.congestion_events > landing.congestion_events {
            return Some(ProbeAbort::Congestion);
        }
        // Only against a round trip measured after the step up. The reading from
        // before it describes the rung the probe stepped off, and judging a
        // probe on that would fail it for the conditions that let it start.
        if signals.rtt_samples > landing.rtt_samples && queueing(signals, config) {
            return Some(ProbeAbort::Queueing);
        }
        // A rung that costs more must not deliver less. If it does, something
        // between the publisher and here is refusing the difference, which is
        // the answer the probe went looking for.
        if now.duration_since(landing.at) >= config.probe_settle
            && let (Some(arriving), Some(below)) = (signals.goodput_bps, self.below_bps)
            && arriving < below
        {
            return Some(ProbeAbort::Goodput);
        }
        None
    }

    /// Returns whether the probe has played for
    /// [`AdaptiveConfig::probe_duration`] without being taken back.
    pub fn held(&self, config: &AdaptiveConfig, now: Instant) -> bool {
        self.landing
            .is_some_and(|landing| now.duration_since(landing.at) >= config.probe_duration)
    }
}

// --- Tests -----------------------------------------------------------

#[cfg(test)]
mod tests {
    use hang::catalog::{H264, VideoCodec};

    use super::*;

    fn test_config(w: u32, h: u32, bitrate: u64) -> VideoConfig {
        let mut config = VideoConfig::new(VideoCodec::H264(H264 {
            inline: true,
            profile: 0x64,
            constraints: 0,
            level: 0x1f,
        }));
        config.coded_width = Some(w);
        config.coded_height = Some(h);
        config.bitrate = Some(bitrate);
        config
    }

    fn test_ranked() -> Vec<RankedRendition> {
        vec![
            RankedRendition {
                name: "video-1080p".into(),
                pixels: 1920 * 1080,
                bitrate_bps: 4_000_000,
                width: 1920,
                height: 1080,
            },
            RankedRendition {
                name: "video-720p".into(),
                pixels: 1280 * 720,
                bitrate_bps: 2_000_000,
                width: 1280,
                height: 720,
            },
            RankedRendition {
                name: "video-360p".into(),
                pixels: 640 * 360,
                bitrate_bps: 500_000,
                width: 640,
                height: 360,
            },
        ]
    }

    fn good_signals() -> NetworkSignals {
        NetworkSignals {
            // At the path minimum, so nothing is queued.
            rtt: Duration::from_millis(20),
            rtt_samples: 1,
            min_rtt: Duration::from_millis(20),
            loss_rate: 0.0,
            // Three quarters of the top rung's advertised bitrate, which is what
            // an encoder handed a ceiling it does not need actually sends. Only
            // the cases that say otherwise carry a shortfall.
            goodput_bps: Some(3_000_000),
            congestion_events: 0,
        }
    }

    /// Signals for a path with a queue built up in front of the bottleneck.
    fn queued_signals() -> NetworkSignals {
        NetworkSignals {
            rtt: Duration::from_millis(300),
            ..good_signals()
        }
    }

    /// Runs one evaluation over a clear path, which is what teaches the rule
    /// what the rendition at `idx` delivers when there is nothing in its way.
    ///
    /// Every shortfall below is measured against the reference this leaves
    /// behind, so a test that skips it is testing a rule with nothing to compare
    /// against.
    fn establish(
        idx: usize,
        ranked: &[RankedRendition],
        goodput_bps: u64,
        timers: &mut AdaptationTimers,
        config: &AdaptiveConfig,
        now: Instant,
    ) {
        let signals = NetworkSignals {
            goodput_bps: Some(goodput_bps),
            ..good_signals()
        };
        evaluate(idx, ranked, &signals, timers, config, now);
    }

    /// Returns a probe of the top rung that has already started playing, judged
    /// against `signals` and stepping up from a rung delivering `below_bps`.
    fn landed_probe(signals: &NetworkSignals, below_bps: Option<u64>, now: Instant) -> Probe {
        let mut probe = Probe::new("video-1080p".into(), below_bps);
        probe.land(signals, now);
        probe
    }

    #[test]
    fn hold_when_conditions_good() {
        let ranked = test_ranked();
        let signals = good_signals();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let now = Instant::now();

        let d = evaluate(0, &ranked, &signals, &mut timers, &config, now);
        assert_eq!(
            d,
            Decision::Hold,
            "highest rendition + good signals -> hold"
        );
    }

    #[test]
    fn emergency_on_extreme_loss() {
        let ranked = test_ranked();
        let signals = NetworkSignals {
            loss_rate: 0.25,
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let now = Instant::now();

        let d = evaluate(0, &ranked, &signals, &mut timers, &config, now);
        assert_eq!(d, Decision::Emergency, "25% loss -> emergency");
    }

    #[test]
    fn emergency_does_not_fire_at_lowest() {
        let ranked = test_ranked();
        let signals = NetworkSignals {
            loss_rate: 0.25,
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let now = Instant::now();

        let d = evaluate(2, &ranked, &signals, &mut timers, &config, now);
        assert_eq!(d, Decision::Hold, "already at lowest -> hold");
    }

    #[test]
    fn downgrade_after_sustained_loss() {
        let ranked = test_ranked();
        let signals = NetworkSignals {
            loss_rate: 0.12,
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        let d = evaluate(0, &ranked, &signals, &mut timers, &config, start);
        assert_eq!(d, Decision::Hold, "first tick -> hold (timer just started)");
        assert!(timers.bad_since.is_some());

        // No fresh round trip reading anywhere in this, on purpose: loss is
        // recounted on every tick, so elapsed time really is evidence for it.
        let later = start + config.downgrade_hold;
        let d = evaluate(0, &ranked, &signals, &mut timers, &config, later);
        assert_eq!(d, Decision::Downgrade(1));
    }

    #[test]
    fn downgrade_when_a_queueing_path_starves_the_rendition() {
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        establish(0, &ranked, 3_000_000, &mut timers, &config, start);

        // Half of what the rung was delivering, over a path that has grown a
        // queue, read twice from two different round trip samples.
        let starved = NetworkSignals {
            goodput_bps: Some(1_500_000),
            rtt_samples: 2,
            ..queued_signals()
        };
        evaluate(0, &ranked, &starved, &mut timers, &config, start);
        let again = NetworkSignals {
            rtt_samples: 3,
            ..starved
        };
        let d = evaluate(
            0,
            &ranked,
            &again,
            &mut timers,
            &config,
            start + config.downgrade_hold,
        );
        assert_eq!(d, Decision::Downgrade(1));
    }

    #[test]
    fn an_encoder_under_its_ceiling_sets_the_reference_rather_than_missing_it() {
        // A rung declaring 4 Mbit/s and delivering 3 over a clear path is an
        // encoder spending what the picture needs, which is every encoder. The
        // rule has to read that as the rate this rung arrives at, because
        // reading it as a shortfall against the ceiling would be true forever
        // and would pin the ladder to its bottom rung.
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        let d = evaluate(0, &ranked, &good_signals(), &mut timers, &config, start);
        assert_eq!(d, Decision::Hold);
        assert_eq!(
            timers.healthy_goodput(),
            Some(3_000_000),
            "the arriving rate is the reference, not the advertised one",
        );
        let d = evaluate(
            0,
            &ranked,
            &good_signals(),
            &mut timers,
            &config,
            start + config.downgrade_hold,
        );
        assert_eq!(d, Decision::Hold);
    }

    #[test]
    fn no_downgrade_when_a_queueing_path_still_delivers() {
        // A round trip that has grown without the rendition falling behind is
        // someone else's queue, or a path that simply got longer. The second is
        // what an iroh connection does when it falls back to a relay, and
        // stepping the ladder down for it would step it down for good.
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        establish(0, &ranked, 3_000_000, &mut timers, &config, start);

        for tick in 1..=10 {
            let signals = NetworkSignals {
                rtt_samples: 1 + tick,
                ..queued_signals()
            };
            let now = start + config.check_interval * tick as u32;
            let d = evaluate(0, &ranked, &signals, &mut timers, &config, now);
            assert_eq!(d, Decision::Hold, "tick {tick} on a longer path");
        }
    }

    #[test]
    fn a_latched_queueing_reading_does_not_downgrade() {
        // A subscriber sends few packets that ask to be acknowledged, so QUIC
        // hands it the same round trip over and over: one reading covers a hold
        // of any length by itself. Without a second sample to corroborate it,
        // one scheduler hiccup on a path with a millisecond baseline would be a
        // downgrade.
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        establish(0, &ranked, 3_000_000, &mut timers, &config, start);

        let latched = NetworkSignals {
            goodput_bps: Some(1_000_000),
            rtt_samples: 2,
            ..queued_signals()
        };
        for tick in 1..=10 {
            let now = start + config.check_interval * tick;
            let d = evaluate(0, &ranked, &latched, &mut timers, &config, now);
            assert_eq!(
                d,
                Decision::Hold,
                "tick {tick} still reads the one sample taken at tick 1",
            );
        }
    }

    #[test]
    fn a_second_queueing_sample_downgrades() {
        // The other half of the rule above: the hold is there to be met, and a
        // path that keeps measuring a queue meets it.
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        establish(0, &ranked, 3_000_000, &mut timers, &config, start);

        let mut last = Decision::Hold;
        for tick in 1..=10 {
            let signals = NetworkSignals {
                goodput_bps: Some(1_000_000),
                rtt_samples: 1 + tick as u64,
                ..queued_signals()
            };
            let now = start + config.check_interval * tick;
            last = evaluate(0, &ranked, &signals, &mut timers, &config, now);
            if last != Decision::Hold {
                break;
            }
        }
        assert_eq!(last, Decision::Downgrade(1));
    }

    #[test]
    fn a_short_round_trip_rise_is_not_queueing() {
        // Twice a one-millisecond idle round trip is two milliseconds, which is
        // what sending a frame costs rather than what congestion costs.
        let config = AdaptiveConfig::default();
        let signals = NetworkSignals {
            rtt: Duration::from_millis(2),
            min_rtt: Duration::from_millis(1),
            ..good_signals()
        };
        assert!(!queueing(&signals, &config));
    }

    #[test]
    fn an_unmeasured_baseline_is_not_queueing() {
        // A signals producer that fills in the round trip but not the minimum
        // leaves the ratio test with nothing to compare against, and every
        // reading above the floor would read as a queue.
        let config = AdaptiveConfig::default();
        let signals = NetworkSignals {
            rtt: Duration::from_millis(120),
            min_rtt: Duration::ZERO,
            ..good_signals()
        };
        assert!(!queueing(&signals, &config));
    }

    #[test]
    fn a_long_idle_path_is_not_queueing() {
        // A satellite hop sits at 300ms with nothing wrong with it, so the
        // floor alone would condemn it and the ratio is what saves it.
        let config = AdaptiveConfig::default();
        let signals = NetworkSignals {
            rtt: Duration::from_millis(340),
            min_rtt: Duration::from_millis(300),
            ..good_signals()
        };
        assert!(!queueing(&signals, &config));
    }

    #[test]
    fn no_downgrade_when_loss_clears() {
        let ranked = test_ranked();
        let bad = NetworkSignals {
            loss_rate: 0.12,
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        evaluate(0, &ranked, &bad, &mut timers, &config, start);
        assert!(timers.bad_since.is_some());

        let good = good_signals();
        let d = evaluate(
            0,
            &ranked,
            &good,
            &mut timers,
            &config,
            start + Duration::from_millis(200),
        );
        assert_eq!(d, Decision::Hold);
        assert!(timers.bad_since.is_none(), "bad_since should reset");
    }

    #[test]
    fn a_quieter_scene_lowers_the_reference() {
        // The reference is a peak, so a picture that stopped moving would leave
        // it stranded at what the busy scene cost and every later reading short
        // of it. Decay is what keeps the comparison about the link.
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        establish(0, &ranked, 3_000_000, &mut timers, &config, start);
        let halved = start + config.goodput_baseline_halflife;
        establish(0, &ranked, 1_000_000, &mut timers, &config, halved);
        let reference = timers.healthy_goodput().expect("a reference was recorded");
        assert!(
            (1_400_000..=1_600_000).contains(&reference),
            "one halflife should take 3 Mbit/s to about 1.5, got {reference}",
        );

        let quiet = halved + config.goodput_baseline_halflife;
        establish(0, &ranked, 1_000_000, &mut timers, &config, quiet);
        assert_eq!(
            timers.healthy_goodput(),
            Some(1_000_000),
            "the rate that keeps arriving eventually becomes the reference",
        );
    }

    #[test]
    fn the_reference_does_not_survive_a_switch() {
        // Every rung delivers a different rate, so carrying the old rung's over
        // a downgrade would read the smaller rendition as a starved larger one
        // and walk the ladder to the bottom in one step per tick.
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        establish(0, &ranked, 3_000_000, &mut timers, &config, start);
        establish(1, &ranked, 1_000_000, &mut timers, &config, start);
        assert_eq!(timers.healthy_goodput(), Some(1_000_000));
    }

    #[test]
    fn upgrade_probe_after_sustained_good() {
        let ranked = test_ranked();
        let signals = good_signals();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        let d = evaluate(1, &ranked, &signals, &mut timers, &config, start);
        assert_eq!(d, Decision::Hold, "first tick -> hold");
        assert!(timers.good_since.is_some());

        let d = evaluate(
            1,
            &ranked,
            &signals,
            &mut timers,
            &config,
            start + config.upgrade_hold,
        );
        assert_eq!(d, Decision::StartProbe(0));
    }

    #[test]
    fn no_upgrade_during_downgrade_cooldown() {
        let ranked = test_ranked();
        let signals = good_signals();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let now = Instant::now();

        timers.last_downgrade = Some(now);

        let d = evaluate(1, &ranked, &signals, &mut timers, &config, now);
        assert_eq!(d, Decision::Hold, "within cooldown -> hold");

        let later = now + config.post_downgrade_cooldown + Duration::from_millis(1);
        let d = evaluate(1, &ranked, &signals, &mut timers, &config, later);
        assert_eq!(d, Decision::Hold, "still needs upgrade_hold time");
        assert!(timers.good_since.is_some());
    }

    #[test]
    fn no_upgrade_during_probe_cooldown() {
        let ranked = test_ranked();
        let signals = good_signals();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let now = Instant::now();

        timers.last_probe = Some(now);

        let d = evaluate(1, &ranked, &signals, &mut timers, &config, now);
        assert_eq!(d, Decision::Hold, "within probe cooldown -> hold");
    }

    #[test]
    fn no_upgrade_when_already_highest() {
        let ranked = test_ranked();
        let signals = good_signals();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        let d = evaluate(
            0,
            &ranked,
            &signals,
            &mut timers,
            &config,
            start + config.upgrade_hold,
        );
        assert_eq!(d, Decision::Hold, "already at highest -> no upgrade");
    }

    #[test]
    fn upgrade_probes_below_the_advertised_bitrate() {
        // Half the advertised bitrate arriving is what an efficient encoder on
        // a healthy path looks like, and holding the ladder down for it would
        // strand every publisher that undershoots its own ceiling. Probing is
        // how the room above the current rate gets found.
        let ranked = test_ranked();
        let signals = NetworkSignals {
            goodput_bps: Some(1_000_000),
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        evaluate(1, &ranked, &signals, &mut timers, &config, start);
        let d = evaluate(
            1,
            &ranked,
            &signals,
            &mut timers,
            &config,
            start + config.upgrade_hold,
        );
        assert_eq!(d, Decision::StartProbe(0));
    }

    #[test]
    fn upgrade_probes_without_a_goodput_reading() {
        // Nothing measurable arriving is not evidence of a small link, and a
        // probe is how headroom gets found, so an unmeasured goodput must not
        // stand in the way of one.
        let ranked = test_ranked();
        let signals = NetworkSignals {
            goodput_bps: None,
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        evaluate(1, &ranked, &signals, &mut timers, &config, start);
        let d = evaluate(
            1,
            &ranked,
            &signals,
            &mut timers,
            &config,
            start + config.upgrade_hold,
        );
        assert_eq!(d, Decision::StartProbe(0));
    }

    #[test]
    fn no_downgrade_without_a_goodput_reading() {
        // The publisher going quiet leaves nothing to measure, even with a
        // reference to measure it against. Reading that as a link carrying
        // nothing would step the ladder down over a pause.
        let ranked = test_ranked();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        establish(0, &ranked, 3_000_000, &mut timers, &config, start);

        for tick in 1..=10 {
            let signals = NetworkSignals {
                goodput_bps: None,
                rtt_samples: 1 + tick as u64,
                ..queued_signals()
            };
            let now = start + config.check_interval * tick;
            let d = evaluate(0, &ranked, &signals, &mut timers, &config, now);
            assert_eq!(d, Decision::Hold, "tick {tick} with nothing arriving");
        }
    }

    #[test]
    fn probe_abort_on_loss() {
        let config = AdaptiveConfig::default();
        let now = Instant::now();
        let probe = landed_probe(&good_signals(), Some(1_000_000), now);
        let signals = NetworkSignals {
            loss_rate: 0.06,
            ..good_signals()
        };
        assert_eq!(probe.abort(&signals, &config, now), Some(ProbeAbort::Loss));
    }

    #[test]
    fn probe_abort_on_congestion() {
        let config = AdaptiveConfig::default();
        let now = Instant::now();
        let probe = landed_probe(&good_signals(), Some(1_000_000), now);
        let signals = NetworkSignals {
            congestion_events: 5,
            ..good_signals()
        };
        assert_eq!(
            probe.abort(&signals, &config, now),
            Some(ProbeAbort::Congestion),
        );
    }

    #[test]
    fn probe_aborts_when_the_path_starts_queueing() {
        // The failure a subscriber is most likely to meet and the one loss and
        // congestion both miss: a downlink that runs out of room while the
        // uplink, which is all this endpoint's own counters describe, stays
        // clear. Without this the probe cannot fail on such a path at all, so
        // the ladder climbs whatever the link says.
        let config = AdaptiveConfig::default();
        let now = Instant::now();
        let probe = landed_probe(&good_signals(), Some(1_000_000), now);
        let queued = NetworkSignals {
            rtt_samples: 2,
            ..queued_signals()
        };
        assert_eq!(
            probe.abort(&queued, &config, now),
            Some(ProbeAbort::Queueing),
        );
    }

    #[test]
    fn probe_is_not_judged_on_a_round_trip_from_before_it_started() {
        // The reading that was current when the step up landed describes the
        // rung it stepped off. Failing the probe on it would fail it for the
        // conditions that let it start, and no probe would ever complete.
        let config = AdaptiveConfig::default();
        let now = Instant::now();
        let queued = queued_signals();
        let probe = landed_probe(&queued, Some(1_000_000), now);
        assert_eq!(probe.abort(&queued, &config, now), None);
    }

    #[test]
    fn probe_aborts_when_the_higher_rung_delivers_less() {
        // A rung that costs more and delivers less than the one below it is a
        // link refusing the difference, which is the question the probe asked.
        let config = AdaptiveConfig::default();
        let start = Instant::now();
        let probe = landed_probe(&good_signals(), Some(1_000_000), start);
        let signals = NetworkSignals {
            goodput_bps: Some(900_000),
            ..good_signals()
        };

        assert_eq!(
            probe.abort(&signals, &config, start),
            None,
            "not before the goodput window has refilled from the new rendition",
        );
        assert_eq!(
            probe.abort(&signals, &config, start + config.probe_settle),
            Some(ProbeAbort::Goodput),
        );
    }

    #[test]
    fn probe_continues_when_clean() {
        let config = AdaptiveConfig::default();
        let start = Instant::now();
        let signals = NetworkSignals {
            loss_rate: 0.01,
            congestion_events: 3,
            ..good_signals()
        };
        let probe = landed_probe(&signals, Some(1_000_000), start);

        let after = start + config.probe_settle;
        assert_eq!(probe.abort(&signals, &config, after), None);
        assert!(!probe.held(&config, after), "still inside probe_duration");
        assert!(probe.held(&config, start + config.probe_duration));
    }

    #[test]
    fn an_unlanded_probe_is_not_judged() {
        // A step up that never took effect is still playing the rung below, so
        // aborting it would step down from a rung nothing ever left.
        let config = AdaptiveConfig::default();
        let now = Instant::now();
        let probe = Probe::new("video-1080p".into(), Some(1_000_000));
        let awful = NetworkSignals {
            loss_rate: 0.5,
            congestion_events: 99,
            rtt_samples: 9,
            ..queued_signals()
        };
        assert_eq!(probe.abort(&awful, &config, now), None);
        assert!(!probe.held(&config, now + config.probe_duration));
    }

    #[test]
    fn rank_renditions_sorted() {
        let mut renditions = BTreeMap::new();
        renditions.insert("low".into(), test_config(640, 360, 500_000));
        renditions.insert("high".into(), test_config(1920, 1080, 4_000_000));
        renditions.insert("mid".into(), test_config(1280, 720, 2_000_000));

        let ranked = rank_renditions(&renditions);
        assert_eq!(ranked[0].name, "high");
        assert_eq!(ranked[1].name, "mid");
        assert_eq!(ranked[2].name, "low");
    }
}
