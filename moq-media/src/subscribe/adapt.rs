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
    adaptive::{AdaptationTimers, AdaptiveConfig, Decision, Probe, evaluate, rank_renditions},
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

        if let Some(active_probe) = &mut probe {
            if active_probe.rendition() == active {
                active_probe.land(&signals, now);
            } else if requested.borrow().is_none() {
                // The step up never landed: the replacement failed to open, or
                // the supervisor withdrew it. Forget it rather than judging it,
                // or the abort path steps down from a rung we never left.
                debug!(rendition = %active_probe.rendition(), "probe never took effect");
                probe = None;
            }
        }

        if let Some(active_probe) = probe.as_ref() {
            if let Some(reason) = active_probe.abort(&signals, &config, now) {
                // The step up did not hold. Go back down and start the probe
                // cooldown, so the next attempt waits rather than oscillating.
                let back = (index + 1).min(ranked.len() - 1);
                info!(
                    from = %active,
                    to = %ranked[back].name,
                    ?reason,
                    "probe aborted, stepping back down",
                );
                // Clear both holds along with the probe. The good run that
                // let this probe start has just been disproved, and the bad
                // run that the abort observed belongs to the rung being left,
                // so carrying either into the rung below would let the very
                // next tick step down again.
                timers.last_probe = Some(now);
                timers.good_since = None;
                timers.bad_since = None;
                probe = None;
                requested.send_replace(Some(ranked[back].name.clone()));
                continue;
            }
            if active_probe.held(&config, now) {
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
                // The rung being left is what the step up has to beat: a higher
                // rendition delivering less than the one below it did is a link
                // refusing the difference.
                probe = Some(Probe::new(
                    ranked[next].name.clone(),
                    timers.healthy_goodput(),
                ));
                next
            }
            Decision::Emergency => {
                probe = None;
                ranked.len() - 1
            }
        };

        let goodput_kbps = signals.goodput_bps.map(|bps| bps / 1000);
        info!(
            from = %active,
            to = %ranked[target].name,
            rtt_ms = signals.rtt.as_millis() as u64,
            loss = signals.loss_rate,
            ?goodput_kbps,
            ?decision,
            "adapting rendition",
        );
        requested.send_replace(Some(ranked[target].name.clone()));
    }
}
