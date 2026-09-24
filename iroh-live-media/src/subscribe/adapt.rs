//! The adaptation task: follow the downlink by switching renditions.
//!
//! `moq_mux::select` fixes the rendition at construction and
//! `moq_video::encode::rate` backs off the *sender's* bitrate, so neither
//! covers a subscriber choosing for itself. This loop reads transport signals,
//! asks the bound in [`crate::adaptive`] which rendition fits, and requests the
//! switch. The actual decoder swap is the video supervisor's job.

use n0_future::task::{AbortOnDropHandle, spawn};
use n0_watcher::Watchable;
use tokio::{sync::watch, time::Instant};
use tracing::{Instrument, error_span, info, trace};

use super::{CatalogSnapshot, RemoteBroadcast};
use crate::{
    adaptive::{
        AdaptiveConfig,
        bound::{Bound, Constraints, Reading, Rung, Tuning},
        rank_renditions,
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
    let mut bound = Bound::new(tuning(&config));
    let mut ticker = tokio::time::interval(config.check_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let shutdown = broadcast.shutdown_token();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {}
        }

        let catalog = broadcast.catalog();
        let ranked = rungs(&catalog);
        if ranked.len() < 2 {
            // Nothing to adapt between. Keep ticking rather than returning:
            // a publisher can add renditions to its catalog mid-broadcast.
            continue;
        }

        // A switch already asked for but not yet applied: the replacement
        // decoder is opening and catching up. Deciding again against the old
        // rendition would re-request the same switch every tick until it
        // lands, so hold until it does.
        let active = current.get();
        if let Some(target) = requested.borrow().clone()
            && target != active
        {
            continue;
        }

        let signals = *signals.borrow();
        let reading = Reading {
            loss: Some(signals.loss_rate),
            delivery: signals.delivery_bps,
            path_generation: signals.path_generation,
        };
        let Some(target) = bound.decide(
            &ranked,
            Some(&active),
            &Constraints::default(),
            &reading,
            Instant::now(),
        ) else {
            continue;
        };
        // Every tick, because the interesting case is the one where nothing
        // happens: these are the inputs that tell a loop holding for a reason
        // from one holding for none.
        trace!(
            rendition = %active,
            loss = signals.loss_rate,
            delivery_bps = ?signals.delivery_bps,
            path_generation = signals.path_generation,
            %target,
            "adaptation tick",
        );
        if target == active {
            continue;
        }
        info!(
            from = %active,
            to = %target,
            loss = signals.loss_rate,
            delivery_kbps = ?signals.delivery_bps.map(|bps| bps / 1000),
            "adapting rendition",
        );
        requested.send_replace(Some(target));
    }
}

/// The bound's tuning, from the thresholds and timers a caller configured.
fn tuning(config: &AdaptiveConfig) -> Tuning {
    Tuning {
        fit_ratio: config.delivery_downgrade_ratio,
        loss_step_down: config.loss_downgrade,
        loss_emergency: config.loss_emergency,
        downgrade_hold: config.downgrade_hold,
        upgrade_hold: config.upgrade_hold,
        post_downgrade_cooldown: config.post_downgrade_cooldown,
        ..Tuning::default()
    }
}

/// The catalog's video renditions, best first, as the bound reads them.
fn rungs(catalog: &CatalogSnapshot) -> Vec<Rung> {
    rank_renditions(catalog.video())
        .into_iter()
        .map(|ranked| {
            let stalled = catalog
                .video()
                .get(&ranked.name)
                .and_then(|config| config.stalled)
                .unwrap_or(false);
            Rung {
                height: (ranked.height > 0).then_some(ranked.height),
                bitrate: (ranked.bitrate_bps > 0).then_some(ranked.bitrate_bps),
                name: ranked.name,
                stalled,
            }
        })
        .collect()
}
