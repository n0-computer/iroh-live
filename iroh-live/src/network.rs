//! A subscription's link, in the media crate's terms.
//!
//! The transport keeps one connection monitor per session and reports it as a
//! [`LinkSample`]. The media crate reads network conditions as
//! [`NetworkSignals`] for its rendition choice and draws [`NetStats`] in a user
//! interface. This module turns the one into the other two, following whichever
//! session serves a [`Subscription`] as its route changes.

use std::time::{Duration, Instant};

use iroh_live_media::{net::NetworkSignals, stats::NetStats};
use iroh_moq::{LinkSample, Subscription};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug_span};

/// How often a subscription's link is read.
const INTERVAL: Duration = Duration::from_millis(200);

/// Returns the network signals of the link serving `subscription`.
///
/// For the media crate's rendition adaptation.
///
/// Follows the serving session as the route changes. The feed runs until
/// `shutdown` is cancelled (pass the broadcast's shutdown token), the path has
/// no route left, or every receiver is dropped.
pub fn signals(
    subscription: &Subscription,
    shutdown: CancellationToken,
) -> watch::Receiver<NetworkSignals> {
    let (tx, rx) = watch::channel(NetworkSignals::default());
    let subscription = subscription.clone();
    let span = debug_span!("network_signals", path = %subscription.path());
    tokio::spawn(
        async move {
            let mut interval = tokio::time::interval(INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let closed = subscription.closed();
            tokio::pin!(closed);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = shutdown.cancelled() => break,
                    _ = &mut closed => break,
                    _ = tx.closed() => break,
                }
                let Some(session) = subscription.session() else {
                    continue;
                };
                tx.send_replace(to_signals(&session.link()));
            }
        }
        .instrument(span),
    );
    rx
}

/// Records the link serving `subscription` into `net`, for a user interface.
///
/// Until `shutdown` is cancelled or the path has no route left.
pub(crate) fn record_stats(
    subscription: &Subscription,
    net: NetStats,
    shutdown: CancellationToken,
) {
    let subscription = subscription.clone();
    let span = debug_span!("network_stats", path = %subscription.path());
    tokio::spawn(
        async move {
            let mut interval = tokio::time::interval(INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let closed = subscription.closed();
            tokio::pin!(closed);
            let mut previous: Option<(Instant, u64, u64, u64)> = None;
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = shutdown.cancelled() => break,
                    _ = &mut closed => break,
                }
                let Some(session) = subscription.session() else {
                    net.path_type.set("relay link");
                    continue;
                };
                let link = session.link();
                net.peer.set(session.remote_id().fmt_short().to_string());
                net.rtt_ms.record(link.rtt.as_secs_f64() * 1000.0);
                net.path_type
                    .set(if link.relayed { "relayed" } else { "direct" });
                if let Some(addr) = &link.remote_addr {
                    net.path_addr.set(addr.clone());
                }
                net.paths_active.record(link.paths as f64);
                net.loss_pct.record(link.loss_rate * 100.0);
                let now = Instant::now();
                // Throughput from the counters, restarted when the path changes
                // so two paths' counters are never differenced.
                if let Some((at, generation, received, sent)) = previous
                    && generation == link.path_generation
                {
                    let seconds = now.duration_since(at).as_secs_f64();
                    if seconds > 0.0 {
                        let mbps = |bytes: u64| bytes as f64 * 8.0 / seconds / 1_000_000.0;
                        net.bw_down_mbps
                            .record(mbps(link.bytes_received.saturating_sub(received)));
                        net.bw_up_mbps
                            .record(mbps(link.bytes_sent.saturating_sub(sent)));
                    }
                }
                previous = Some((
                    now,
                    link.path_generation,
                    link.bytes_received,
                    link.bytes_sent,
                ));
            }
        }
        .instrument(span),
    );
}

/// Converts a link sample into the signals the media crate adapts on.
pub fn to_signals(link: &LinkSample) -> NetworkSignals {
    NetworkSignals {
        rtt: link.rtt,
        rtt_samples: link.rtt_samples,
        min_rtt: link.min_rtt,
        loss_rate: link.loss_rate,
        goodput_bps: link.goodput_bps,
        delivery_bps: link.delivery_bps,
        congestion_events: link.congestion_events,
    }
}
