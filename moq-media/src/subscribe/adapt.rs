//! The adaptation task: follow the downlink by switching renditions.
//!
//! `moq_mux::select` fixes the rendition at construction and
//! `moq_video::encode::rate` backs off the *sender's* bitrate, so neither
//! covers a subscriber choosing for itself. This loop reads transport signals,
//! asks [`crate::adaptive`] what to do, and requests the switch. The actual
//! decoder swap is the video supervisor's job.

use std::time::Instant;

use n0_future::task::{AbortOnDropHandle, spawn};
use n0_watcher::Watchable;
use tokio::sync::watch;
use tracing::{Instrument, debug, error_span, info};

use super::RemoteBroadcast;
use crate::{
    adaptive::{
        AdaptationTimers, AdaptiveConfig, Decision, evaluate, rank_renditions, should_abort_probe,
    },
    net::NetworkSignals,
};

/// Starts following `signals`. Dropping the handle stops adapting and holds
/// whatever rendition is playing.
pub(super) fn spawn_adaptation(
    broadcast: RemoteBroadcast,
    current: Watchable<String>,
    requested: watch::Sender<Option<String>>,
    signals: watch::Receiver<NetworkSignals>,
    config: AdaptiveConfig,
) -> AbortOnDropHandle<()> {
    let name = broadcast.name().to_string();
    let task = spawn(
        run(broadcast, current, requested, signals, config)
            .instrument(error_span!("adapt", broadcast = %name)),
    );
    AbortOnDropHandle::new(task)
}

async fn run(
    broadcast: RemoteBroadcast,
    current: Watchable<String>,
    requested: watch::Sender<Option<String>>,
    signals: watch::Receiver<NetworkSignals>,
    config: AdaptiveConfig,
) {
    let mut timers = AdaptationTimers::default();
    let mut ticker = tokio::time::interval(config.check_interval);
    let shutdown = broadcast.shutdown_token();
    // The rendition a probe stepped up to, and when. An upgrade is a bet that
    // the link can carry more, so it is watched and taken back if the link says
    // otherwise rather than left to the next downgrade timer.
    let mut probe: Option<Probe> = None;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {}
        }

        let catalog = broadcast.catalog();
        let ranked = rank_renditions(catalog.video());
        if ranked.len() < 2 {
            // Nothing to adapt between. Keep ticking rather than returning:
            // a publisher can add renditions to its catalog mid-broadcast.
            continue;
        }

        // A switch already asked for but not yet applied: the replacement
        // decoder is opening and waiting for its first frame. Deciding again
        // against the old rendition would re-request the same switch every tick
        // until it lands, so hold until it does.
        if let Some(target) = requested.borrow().clone()
            && target != current.get()
        {
            continue;
        }

        let active = current.get();
        let Some(index) = ranked.iter().position(|r| r.name == active) else {
            debug!(rendition = %active, "active rendition left the catalog");
            continue;
        };

        let signals = *signals.borrow();
        let now = Instant::now();

        if let Some(active_probe) = &probe {
            if should_abort_probe(&signals, active_probe.congestion_baseline, &config) {
                // The step up did not hold. Go back down and start the probe
                // cooldown, so the next attempt waits rather than oscillating.
                let back = (index + 1).min(ranked.len() - 1);
                info!(
                    from = %active,
                    to = %ranked[back].name,
                    loss = signals.loss_rate,
                    "probe aborted, stepping back down",
                );
                timers.last_probe = Some(now);
                probe = None;
                requested.send_replace(Some(ranked[back].name.clone()));
                continue;
            }
            if now.duration_since(active_probe.started) >= config.probe_duration {
                debug!(rendition = %active, "probe held");
                probe = None;
            }
        }
        let decision = evaluate(index, &ranked, &signals, &mut timers, &config, now);
        let target = match decision {
            Decision::Hold => continue,
            Decision::Downgrade(next) => {
                probe = None;
                next
            }
            Decision::StartProbe(next) => {
                timers.last_probe = Some(now);
                probe = Some(Probe {
                    started: now,
                    congestion_baseline: signals.congestion_events,
                });
                next
            }
            Decision::Emergency => {
                probe = None;
                ranked.len() - 1
            }
        };

        info!(
            from = %active,
            to = %ranked[target].name,
            rtt_ms = signals.rtt.as_millis() as u64,
            loss = signals.loss_rate,
            available_kbps = signals.available_bps / 1000,
            ?decision,
            "adapting rendition",
        );
        requested.send_replace(Some(ranked[target].name.clone()));
    }
}

/// An upgrade in progress: a step up the ladder that has not yet proved itself.
struct Probe {
    /// When the step up was requested, against `AdaptiveConfig::probe_duration`.
    started: Instant,
    /// The congestion counter at the moment of the step, so any new congestion
    /// event during the probe is attributable to it.
    congestion_baseline: u64,
}
