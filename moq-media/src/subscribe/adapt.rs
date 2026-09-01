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
    adaptive::{AdaptationTimers, AdaptiveConfig, Decision, evaluate, rank_renditions},
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
        let decision = evaluate(
            index,
            &ranked,
            &signals,
            &mut timers,
            &config,
            Instant::now(),
        );
        let target = match decision {
            Decision::Hold => continue,
            Decision::Downgrade(next) | Decision::StartProbe(next) => next,
            Decision::Emergency => ranked.len() - 1,
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
