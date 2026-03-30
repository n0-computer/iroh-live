//! Adaptive rendition switching for video tracks.
//!
//! The selection algorithm ([`evaluate`]) and ranking ([`rank_renditions`])
//! are used by [`VideoTrack::enable_adaptation`](crate::subscribe::VideoTrack::enable_adaptation)
//! to decide when to switch renditions based on [`NetworkSignals`].

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use hang::catalog::VideoConfig;

use crate::net::NetworkSignals;

// ── Configuration ───────────────────────────────────────────────────────

/// Controls which rendition an adaptive track subscribes to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RenditionMode {
    /// Automatically select based on network signals.
    #[default]
    Auto,
    /// Pin to a specific rendition by catalog key.
    Fixed(String),
}

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
    /// Bandwidth utilization ceiling before downgrade (e.g. 0.85 = 85%).
    pub bw_downgrade_ratio: f64,
    /// Bandwidth headroom factor required before probing (e.g. 1.2 = 20% excess).
    pub bw_probe_headroom: f64,
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
            bw_downgrade_ratio: 0.85,
            bw_probe_headroom: 1.2,
            check_interval: Duration::from_millis(200),
        }
    }
}

// ── Rendition ranking ───────────────────────────────────────────────────

/// Rendition ranked by quality. Index 0 = highest quality.
#[derive(Debug, Clone)]
pub struct RankedRendition {
    /// Catalog key (track name).
    pub name: String,
    /// Total pixel count (`coded_width * coded_height`).
    pub pixels: u64,
    /// Advertised bitrate in bits per second.
    pub bitrate_bps: u64,
    /// Coded dimensions from the catalog.
    pub width: u32,
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
    ranked.sort_by(|a, b| b.pixels.cmp(&a.pixels));
    ranked
}

// ── Selection logic ─────────────────────────────────────────────────────

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
    /// When bad conditions were first detected (for downgrade_hold).
    pub bad_since: Option<Instant>,
    /// When good conditions were first detected (for upgrade_hold).
    pub good_since: Option<Instant>,
    /// When the last downgrade occurred.
    pub last_downgrade: Option<Instant>,
    /// When the last probe attempt (success or failure) occurred.
    pub last_probe: Option<Instant>,
    /// Baseline congestion_events counter at probe start.
    pub probe_congestion_baseline: Option<u64>,
    /// When the last rendition switch attempt failed. Prevents rapid
    /// retry thrashing (decoder allocation churn) when switches fail
    /// persistently. Cleared on successful switch.
    pub last_switch_failure: Option<Instant>,
}

/// Evaluates network signals and decides whether to switch renditions.
///
/// `current_idx` is the index into `ranked` for the currently active rendition.
/// Returns a [`Decision`].
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

    // ── Emergency: immediate drop to lowest ─────────────────────────
    if signals.loss_rate >= config.loss_emergency && !is_lowest {
        timers.bad_since = None;
        timers.good_since = None;
        timers.last_downgrade = Some(now);
        return Decision::Emergency;
    }

    // ── Downgrade check ─────────────────────────────────────────────
    let bandwidth_stressed = current.bitrate_bps > 0
        && signals.available_bps < (current.bitrate_bps as f64 * config.bw_downgrade_ratio) as u64;
    let loss_high = signals.loss_rate >= config.loss_downgrade;

    if (bandwidth_stressed || loss_high) && !is_lowest {
        let bad_since = *timers.bad_since.get_or_insert(now);
        if now.duration_since(bad_since) >= config.downgrade_hold {
            timers.bad_since = None;
            timers.good_since = None;
            timers.last_downgrade = Some(now);
            return Decision::Downgrade(current_idx + 1);
        }
    } else {
        timers.bad_since = None;
    }

    // ── Upgrade check (probe gating) ────────────────────────────────
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

    let next_higher = &ranked[current_idx - 1];
    let has_headroom = next_higher.bitrate_bps == 0
        || signals.available_bps
            >= (next_higher.bitrate_bps as f64 * config.bw_probe_headroom) as u64;
    let loss_good = signals.loss_rate <= config.loss_good;

    if has_headroom && loss_good {
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

/// Checks whether an active probe should be aborted.
pub fn should_abort_probe(
    signals: &NetworkSignals,
    congestion_baseline: u64,
    config: &AdaptiveConfig,
) -> bool {
    signals.loss_rate >= config.loss_probe_abort || signals.congestion_events > congestion_baseline
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use hang::catalog::{H264, VideoCodec};

    use super::*;

    fn test_config(w: u32, h: u32, bitrate: u64) -> VideoConfig {
        VideoConfig {
            codec: VideoCodec::H264(H264 {
                inline: true,
                profile: 0x64,
                constraints: 0,
                level: 0x1f,
            }),
            coded_width: Some(w),
            coded_height: Some(h),
            bitrate: Some(bitrate),
            description: None,
            display_ratio_width: None,
            display_ratio_height: None,
            framerate: None,
            optimize_for_latency: None,
            container: Default::default(),
            jitter: None,
        }
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
            rtt: Duration::from_millis(20),
            loss_rate: 0.0,
            available_bps: 10_000_000, // 10 Mbps
            congestion_events: 0,
        }
    }

    #[test]
    fn hold_when_conditions_good() {
        let ranked = test_ranked();
        let signals = good_signals();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let now = Instant::now();

        let d = evaluate(0, &ranked, &signals, &mut timers, &config, now);
        assert_eq!(d, Decision::Hold, "highest rendition + good signals → hold");
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
        assert_eq!(d, Decision::Emergency, "25% loss → emergency");
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
        assert_eq!(d, Decision::Hold, "already at lowest → hold");
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
        assert_eq!(d, Decision::Hold, "first tick → hold (timer just started)");
        assert!(timers.bad_since.is_some());

        let later = start + config.downgrade_hold;
        let d = evaluate(0, &ranked, &signals, &mut timers, &config, later);
        assert_eq!(d, Decision::Downgrade(1));
    }

    #[test]
    fn downgrade_on_bandwidth_stress() {
        let ranked = test_ranked();
        let signals = NetworkSignals {
            available_bps: 3_000_000,
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        evaluate(0, &ranked, &signals, &mut timers, &config, start);
        let d = evaluate(
            0,
            &ranked,
            &signals,
            &mut timers,
            &config,
            start + config.downgrade_hold,
        );
        assert_eq!(d, Decision::Downgrade(1));
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
    fn upgrade_probe_after_sustained_good() {
        let ranked = test_ranked();
        let signals = good_signals();
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        let d = evaluate(1, &ranked, &signals, &mut timers, &config, start);
        assert_eq!(d, Decision::Hold, "first tick → hold");
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
        assert_eq!(d, Decision::Hold, "within cooldown → hold");

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
        assert_eq!(d, Decision::Hold, "within probe cooldown → hold");
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
        assert_eq!(d, Decision::Hold, "already at highest → no upgrade");
    }

    #[test]
    fn no_upgrade_without_headroom() {
        let ranked = test_ranked();
        let signals = NetworkSignals {
            available_bps: 4_500_000,
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        let d = evaluate(
            1,
            &ranked,
            &signals,
            &mut timers,
            &config,
            start + config.upgrade_hold,
        );
        assert_eq!(d, Decision::Hold, "not enough headroom → no probe");
    }

    #[test]
    fn probe_abort_on_loss() {
        let config = AdaptiveConfig::default();
        let signals = NetworkSignals {
            loss_rate: 0.06,
            congestion_events: 0,
            ..good_signals()
        };
        assert!(should_abort_probe(&signals, 0, &config));
    }

    #[test]
    fn probe_abort_on_congestion() {
        let config = AdaptiveConfig::default();
        let signals = NetworkSignals {
            loss_rate: 0.01,
            congestion_events: 5,
            ..good_signals()
        };
        assert!(should_abort_probe(&signals, 3, &config));
    }

    #[test]
    fn probe_continues_when_clean() {
        let config = AdaptiveConfig::default();
        let signals = NetworkSignals {
            loss_rate: 0.01,
            congestion_events: 3,
            ..good_signals()
        };
        assert!(!should_abort_probe(&signals, 3, &config));
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

    #[test]
    fn emergency_fires_despite_recent_failure() {
        // The failure cooldown should NOT block emergency downgrades.
        // Emergency exists for catastrophic conditions where immediate
        // reaction is critical.
        let ranked = test_ranked();
        let signals = NetworkSignals {
            loss_rate: 0.25, // catastrophic
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let now = Instant::now();

        // Simulate a recent switch failure.
        timers.last_switch_failure = Some(now);

        let d = evaluate(0, &ranked, &signals, &mut timers, &config, now);
        assert_eq!(
            d,
            Decision::Emergency,
            "emergency should fire even with recent failure"
        );
    }

    #[test]
    fn downgrade_blocked_by_failure_cooldown() {
        let ranked = test_ranked();
        let signals = NetworkSignals {
            loss_rate: 0.12, // above downgrade threshold
            ..good_signals()
        };
        let config = AdaptiveConfig::default();
        let mut timers = AdaptationTimers::default();
        let start = Instant::now();

        // Prime the downgrade hold timer.
        evaluate(0, &ranked, &signals, &mut timers, &config, start);
        let later = start + config.downgrade_hold;
        let d = evaluate(0, &ranked, &signals, &mut timers, &config, later);
        assert_eq!(d, Decision::Downgrade(1), "should want to downgrade");

        // Now the adaptation_task would attempt the switch and fail.
        // The failure cooldown is checked in the task, not in evaluate().
        // evaluate() will still return Downgrade, but the task skips it.
        // This test verifies evaluate() behavior is unchanged.
    }
}
