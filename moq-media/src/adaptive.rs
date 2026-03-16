//! Adaptive rendition switching for video tracks.
//!
//! [`AdaptiveVideoTrack`] wraps a [`RemoteBroadcast`] and automatically
//! switches between video renditions based on [`NetworkSignals`]. It
//! implements [`VideoSource`] so it can be used anywhere a capture source
//! is expected.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use anyhow::Result;
use hang::catalog::VideoConfig;
use n0_watcher::{Watchable, Watcher};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::{
    format::{DecodeConfig, VideoFrame},
    net::NetworkSignals,
    subscribe::{RemoteBroadcast, VideoTrack},
};
use rusty_codecs::{
    format::{PixelFormat, VideoFormat},
    traits::VideoSource,
};

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
pub(crate) struct RankedRendition {
    /// Catalog key (track name).
    pub name: String,
    /// Total pixel count (`coded_width * coded_height`).
    pub pixels: u64,
    /// Advertised bitrate in bits per second.
    pub bitrate_bps: u64,
}

/// Ranks video renditions by pixel count descending (highest quality first).
pub(crate) fn rank_renditions(renditions: &BTreeMap<String, VideoConfig>) -> Vec<RankedRendition> {
    let mut ranked: Vec<_> = renditions
        .iter()
        .map(|(name, config)| RankedRendition {
            name: name.clone(),
            pixels: config.coded_width.unwrap_or(0) as u64
                * config.coded_height.unwrap_or(0) as u64,
            bitrate_bps: config.bitrate.unwrap_or(0),
        })
        .collect();
    ranked.sort_by(|a, b| b.pixels.cmp(&a.pixels));
    ranked
}

// ── Selection logic ─────────────────────────────────────────────────────

/// Decision produced by the adaptation algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
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
pub(crate) struct AdaptationTimers {
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
}

/// Evaluates network signals and decides whether to switch renditions.
///
/// `current_idx` is the index into `ranked` for the currently active rendition.
/// Returns a [`Decision`].
pub(crate) fn evaluate(
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
pub(crate) fn should_abort_probe(
    signals: &NetworkSignals,
    congestion_baseline: u64,
    config: &AdaptiveConfig,
) -> bool {
    signals.loss_rate >= config.loss_probe_abort || signals.congestion_events > congestion_baseline
}

// ── AdaptiveVideoTrack ──────────────────────────────────────────────────

/// Video track that automatically switches renditions based on network
/// conditions.
///
/// Wraps a [`RemoteBroadcast`] and manages rendition switching in a
/// background task. Implements [`VideoSource`] for use in encoding
/// pipelines (transcode relay) and also provides the familiar
/// [`current_frame`](Self::current_frame) / [`next_frame`](Self::next_frame)
/// API.
#[derive(derive_more::Debug)]
pub struct AdaptiveVideoTrack {
    #[debug(skip)]
    current: VideoTrack,
    #[debug(skip)]
    swap_rx: mpsc::Receiver<VideoTrack>,
    selected_rendition: Watchable<String>,
    viewport: Watchable<(u32, u32)>,
    mode_tx: watch::Sender<RenditionMode>,
    _task: n0_future::task::AbortOnDropHandle<()>,
}

impl AdaptiveVideoTrack {
    /// Creates a new adaptive video track.
    ///
    /// Starts on the best available rendition and spawns a background task
    /// that monitors `signals` and switches renditions as needed.
    #[cfg(any_video_codec)]
    pub fn new(
        broadcast: RemoteBroadcast,
        signals: watch::Receiver<NetworkSignals>,
        config: AdaptiveConfig,
        decode_config: DecodeConfig,
    ) -> Result<Self> {
        use crate::codec::DynamicVideoDecoder;

        let catalog = broadcast.catalog();
        let ranked = rank_renditions(&catalog.video.renditions);
        anyhow::ensure!(!ranked.is_empty(), "no video renditions in catalog");

        // Start on the highest quality rendition.
        let initial_name = &ranked[0].name;
        let initial_track =
            broadcast.video_rendition::<DynamicVideoDecoder>(&decode_config, initial_name)?;

        let selected_rendition = Watchable::new(initial_name.clone());
        let viewport = Watchable::new((0u32, 0u32));
        let (mode_tx, mode_rx) = watch::channel(RenditionMode::Auto);
        let (swap_tx, swap_rx) = mpsc::channel::<VideoTrack>(2);

        let task = tokio::spawn(adaptation_task(
            broadcast,
            signals,
            config,
            decode_config,
            ranked,
            0, // start at highest
            selected_rendition.clone(),
            mode_rx,
            swap_tx,
        ));

        Ok(Self {
            current: initial_track,
            swap_rx,
            selected_rendition,
            viewport,
            mode_tx,
            _task: n0_future::task::AbortOnDropHandle::new(task),
        })
    }

    /// Checks for a rendition swap from the adaptation task.
    fn check_swap(&mut self) {
        while let Ok(new_track) = self.swap_rx.try_recv() {
            self.current = new_track;
        }
    }

    /// Returns the most recent decoded frame, draining any older buffered frames.
    pub fn current_frame(&mut self) -> Option<VideoFrame> {
        self.check_swap();
        self.current.current_frame()
    }

    /// Returns the next decoded frame, waiting if none is buffered.
    pub async fn next_frame(&mut self) -> Option<VideoFrame> {
        self.check_swap();
        if let Some(frame) = self.current.current_frame() {
            return Some(frame);
        }
        // Race: wait for a frame from current track OR a swap notification.
        loop {
            tokio::select! {
                frame = self.current.next_frame() => return frame,
                new_track = self.swap_rx.recv() => {
                    match new_track {
                        Some(track) => {
                            self.current = track;
                            // Try to get a frame from the new track immediately.
                            if let Some(frame) = self.current.current_frame() {
                                return Some(frame);
                            }
                            // Otherwise continue the loop waiting on the new track.
                        }
                        None => return None, // adaptation task exited
                    }
                }
            }
        }
    }

    /// Updates the viewport dimensions for resolution-aware scaling.
    pub fn set_viewport(&self, w: u32, h: u32) {
        self.viewport.set((w, h)).ok();
        self.current.set_viewport(w, h);
    }

    /// Returns the name of the currently active rendition.
    pub fn selected_rendition(&self) -> String {
        self.selected_rendition.get()
    }

    /// Returns a watcher for rendition changes.
    pub fn rendition_watcher(&self) -> n0_watcher::Direct<String> {
        self.selected_rendition.watch()
    }

    /// Sets the rendition mode (Auto or Fixed).
    pub fn set_mode(&self, mode: RenditionMode) {
        self.mode_tx.send(mode).ok();
    }
}

impl VideoSource for AdaptiveVideoTrack {
    fn name(&self) -> &str {
        "adaptive"
    }

    fn format(&self) -> VideoFormat {
        VideoFormat {
            pixel_format: PixelFormat::Rgba,
            dimensions: [0, 0], // dynamic, varies per frame
        }
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        Ok(self.current_frame())
    }

    fn start(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        Ok(())
    }
}

// ── Adaptation task ─────────────────────────────────────────────────────

#[cfg(any_video_codec)]
#[allow(
    clippy::too_many_arguments,
    reason = "private task function, grouping args would add complexity"
)]
async fn adaptation_task(
    broadcast: RemoteBroadcast,
    signals: watch::Receiver<NetworkSignals>,
    config: AdaptiveConfig,
    decode_config: DecodeConfig,
    mut ranked: Vec<RankedRendition>,
    mut current_idx: usize,
    selected_rendition: Watchable<String>,
    mut mode_rx: watch::Receiver<RenditionMode>,
    swap_tx: mpsc::Sender<VideoTrack>,
) {
    let mut timers = AdaptationTimers::default();
    let mut interval = tokio::time::interval(config.check_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut catalog_watcher = broadcast.catalog_watcher();
    let mut probe: Option<(VideoTrack, Instant, u64)> = None; // (track, started, congestion_baseline)

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = mode_rx.changed() => {}
        }

        // Refresh rendition ranking on catalog change.
        if catalog_watcher.update() {
            let catalog = broadcast.catalog();
            let new_ranked = rank_renditions(&catalog.video.renditions);
            if !new_ranked.is_empty() {
                // Try to keep current rendition by name.
                let current_name = &ranked[current_idx].name;
                current_idx = new_ranked
                    .iter()
                    .position(|r| r.name == *current_name)
                    .unwrap_or(0);
                ranked = new_ranked;
            }
        }

        let mode = mode_rx.borrow().clone();

        // Handle Fixed mode: switch to pinned rendition if needed.
        if let RenditionMode::Fixed(ref name) = mode {
            if ranked[current_idx].name != *name
                && let Some(idx) = ranked.iter().position(|r| r.name == *name)
            {
                match switch_rendition(&broadcast, &decode_config, &ranked[idx].name) {
                    Ok(new_track) => {
                        current_idx = idx;
                        selected_rendition.set(ranked[idx].name.clone()).ok();
                        info!(rendition = %ranked[idx].name, "fixed mode: switched rendition");
                        if swap_tx.send(new_track).await.is_err() {
                            return;
                        }
                    }
                    Err(err) => warn!("failed to switch to fixed rendition: {err:#}"),
                }
            }
            // Drop any active probe.
            probe = None;
            continue;
        }

        // Auto mode: read signals and evaluate.
        let sigs = *signals.borrow();
        let now = Instant::now();

        // Check active probe.
        if let Some((probe_track, started, baseline)) = probe.take() {
            if should_abort_probe(&sigs, baseline, &config) {
                info!(
                    loss = sigs.loss_rate,
                    congestion = sigs.congestion_events,
                    "probe aborted: congestion detected"
                );
                drop(probe_track);
                timers.last_probe = Some(now);
                continue;
            }
            if now.duration_since(started) >= config.probe_duration {
                // Probe succeeded — commit.
                let probe_idx = current_idx.saturating_sub(1);
                current_idx = probe_idx;
                selected_rendition.set(ranked[probe_idx].name.clone()).ok();
                info!(rendition = %ranked[probe_idx].name, "probe succeeded: upgraded");
                timers.last_probe = Some(now);
                if swap_tx.send(probe_track).await.is_err() {
                    return;
                }
                continue;
            }
            // Probe still running.
            probe = Some((probe_track, started, baseline));
            continue;
        }

        let decision = evaluate(current_idx, &ranked, &sigs, &mut timers, &config, now);

        match decision {
            Decision::Hold => {}
            Decision::Downgrade(idx) => {
                let target_idx = idx.min(ranked.len() - 1);
                match switch_rendition(&broadcast, &decode_config, &ranked[target_idx].name) {
                    Ok(new_track) => {
                        current_idx = target_idx;
                        selected_rendition.set(ranked[target_idx].name.clone()).ok();
                        info!(
                            rendition = %ranked[target_idx].name,
                            loss = sigs.loss_rate,
                            bw = sigs.available_bps,
                            "downgraded rendition"
                        );
                        if swap_tx.send(new_track).await.is_err() {
                            return;
                        }
                    }
                    Err(err) => warn!("failed to switch rendition: {err:#}"),
                }
            }
            Decision::Emergency => {
                let target_idx = ranked.len() - 1;
                match switch_rendition(&broadcast, &decode_config, &ranked[target_idx].name) {
                    Ok(new_track) => {
                        current_idx = target_idx;
                        selected_rendition.set(ranked[target_idx].name.clone()).ok();
                        info!(
                            rendition = %ranked[target_idx].name,
                            loss = sigs.loss_rate,
                            bw = sigs.available_bps,
                            "downgraded rendition"
                        );
                        if swap_tx.send(new_track).await.is_err() {
                            return;
                        }
                    }
                    Err(err) => warn!("failed to switch rendition: {err:#}"),
                }
            }
            Decision::StartProbe(probe_idx) => {
                debug!(
                    rendition = %ranked[probe_idx].name,
                    bw = sigs.available_bps,
                    "starting upgrade probe"
                );
                match switch_rendition(&broadcast, &decode_config, &ranked[probe_idx].name) {
                    Ok(probe_track) => {
                        let baseline = sigs.congestion_events;
                        timers.probe_congestion_baseline = Some(baseline);
                        probe = Some((probe_track, now, baseline));
                    }
                    Err(err) => {
                        warn!("failed to start probe: {err:#}");
                        timers.last_probe = Some(now);
                    }
                }
            }
        }
    }
}

#[cfg(any_video_codec)]
fn switch_rendition(
    broadcast: &RemoteBroadcast,
    decode_config: &DecodeConfig,
    rendition_name: &str,
) -> Result<VideoTrack> {
    use crate::codec::DynamicVideoDecoder;
    broadcast
        .video_rendition::<DynamicVideoDecoder>(decode_config, rendition_name)
        .map_err(|e| anyhow::anyhow!("{e:#}"))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hang::catalog::{H264, VideoCodec};

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
            },
            RankedRendition {
                name: "video-720p".into(),
                pixels: 1280 * 720,
                bitrate_bps: 2_000_000,
            },
            RankedRendition {
                name: "video-360p".into(),
                pixels: 640 * 360,
                bitrate_bps: 500_000,
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
}
