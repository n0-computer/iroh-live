//! A subscription's link, in the media crate's terms.

use std::sync::Mutex;

use iroh_live_node::{LinkId, Subscription};
use moq_ext_player::{NetworkSample, RemoteBroadcast};

/// Reads the media of a subscription.
///
/// The broadcast follows the subscription's path through the route table it
/// was resolved in, so a change of route does not end it, and its players
/// adapt to the link that serves it. A subscription on one session
/// re-resolves through that session only. The catalog arrives later: watch
/// [`RemoteBroadcast::catalog`] for it.
#[must_use]
pub fn remote_broadcast(subscription: &Subscription) -> RemoteBroadcast {
    let remote = RemoteBroadcast::from_resolved(
        subscription.origin(),
        subscription.path().clone(),
        subscription.broadcast(),
    );
    remote.set_network(signals(subscription.clone()));
    remote
}

/// Returns the network signals of the link serving `subscription`.
///
/// Each sample reads whichever link serves the subscription at that moment.
/// The path generation also counts changes of the serving link, so
/// adaptation never compares one link's history with another's.
pub(crate) fn signals(
    subscription: Subscription,
) -> impl Fn() -> NetworkSample + Send + Sync + 'static {
    // The serving link and path generation at the last sample, and how often
    // that pair changed.
    let last = Mutex::new((None::<(LinkId, u64)>, 0));
    move || {
        let link = subscription.serving_link();
        let mut last = last.lock().expect("poisoned");
        let path = link
            .as_ref()
            .map(|link| (link.id, link.sample.network.path_generation));
        if last.0 != path {
            *last = (path, last.1 + 1);
        }
        let mut sample = link.map(|link| link.sample.network).unwrap_or_default();
        sample.path_generation = last.1;
        sample
    }
}
