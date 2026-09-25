//! A subscription's link, in the media crate's terms.

use std::sync::Mutex;

use iroh_live_media::{Bitrate, NetworkSample};
use iroh_moq::{LinkId, Subscription};

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
        let link = subscription.link();
        let mut sample = NetworkSample::default();
        let mut last = last.lock().expect("poisoned");
        let path = link
            .as_ref()
            .map(|link| (link.id, link.sample.path_generation));
        if last.0 != path {
            *last = (path, last.1 + 1);
        }
        sample.path_generation = last.1;
        if let Some(link) = link {
            sample.rtt = link.sample.rtt;
            sample.min_rtt = link.sample.min_rtt;
            sample.loss = link.sample.loss_rate;
            sample.delivery = link.sample.delivery_bps.map(Bitrate::from_bps);
        }
        sample
    }
}
