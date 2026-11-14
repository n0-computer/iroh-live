//! A subscription's link, in the media crate's terms.

use std::sync::Mutex;

use iroh_live_node::{LinkId, Subscription};
use moq_ext_player::{NetworkSample, RemoteBroadcast};

/// Reads a [`Subscription`] as media.
///
/// # Examples
///
/// ```no_run
/// use iroh_live::{
///     BroadcastTicket, Node,
///     media::{PlayerConfig, SubscriptionExt},
/// };
///
/// # async fn run(node: Node, ticket: BroadcastTicket) -> Result<(), Box<dyn std::error::Error>> {
/// let subscription = node.subscribe_ticket(&ticket).await?;
/// let player = subscription.remote_broadcast().play(PlayerConfig::default())?;
/// # drop(player);
/// # Ok(())
/// # }
/// ```
pub trait SubscriptionExt {
    /// Returns the media of this subscription.
    ///
    /// The broadcast follows the subscription's path through the route table
    /// it was resolved in, so a change of route does not end it, and its
    /// players adapt to the link that serves it. A subscription on one
    /// session re-resolves through that session only. The catalog arrives
    /// later: watch [`RemoteBroadcast::catalog`] for it.
    #[must_use]
    fn remote_broadcast(&self) -> RemoteBroadcast;
}

impl SubscriptionExt for Subscription {
    fn remote_broadcast(&self) -> RemoteBroadcast {
        let remote =
            RemoteBroadcast::from_resolved(self.origin(), self.path().clone(), self.broadcast());
        remote.set_network(signals(self.clone()));
        remote
    }
}

/// Returns the network signals of the link serving `subscription`.
///
/// Each sample reads whichever link serves the subscription at that moment.
/// The path generation also counts changes of the serving link, so
/// adaptation never compares one link's history with another's.
fn signals(subscription: Subscription) -> impl Fn() -> NetworkSample + Send + Sync + 'static {
    // The serving link and path generation at the last sample, and how often
    // that pair changed.
    let last = Mutex::new((None::<(LinkId, u64)>, 0));
    move || {
        let link = subscription.serving_link();
        let mut last = last.lock().expect("poisoned");
        let serving = link
            .as_ref()
            .map(|link| (link.id, link.sample.path_generation));
        if last.0 != serving {
            *last = (serving, last.1 + 1);
        }
        let mut sample = link.map(|link| link.sample).unwrap_or_default();
        sample.path_generation = last.1;
        sample
    }
}
