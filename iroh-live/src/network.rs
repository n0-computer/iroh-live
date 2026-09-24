//! A subscription's link, in the media crate's terms.
//!
//! The transport keeps one connection monitor per session and reports it as a
//! [`LinkSample`]. A player adapts on [`NetworkSignals`], which it samples on its
//! own schedule. [`signals`] turns the one into the other, following whichever
//! session serves a [`Subscription`] as its route changes.

use std::{fmt, sync::Mutex};

use iroh_live_media::{Bitrate, NetworkSample, NetworkSignals};
use iroh_moq::{LinkId, LinkSample, Subscription};
use tracing::debug;

/// Returns the network signals of the link serving `subscription`.
///
/// Each sample reads the session serving the subscription at that moment, so
/// the signals follow the route as it changes. A change of serving session
/// counts as a new path: the sample's path generation moves past every value it
/// took before, so adaptation never compares one session's history with
/// another's. While a relay serves the path, or nothing does yet, the sample
/// carries no measurements.
pub(crate) fn signals(subscription: Subscription) -> impl NetworkSignals {
    let serving = Mutex::new(Serving::<LinkId>::default());
    move || {
        let session = subscription.session();
        let link = session.as_ref().map(|session| session.link());
        let mut serving = serving.lock().expect("poisoned");
        let generation = serving.generation(
            session.as_ref().map(|session| session.link_id()),
            link.as_ref().map_or(0, |link| link.path_generation),
        );
        match link {
            Some(link) => to_sample(&link, generation),
            None => NetworkSample::default().with_path_generation(generation),
        }
    }
}

/// Which session served the subscription at the last sample, and the path
/// generation numbering across sessions.
///
/// Generic over the link's key only so a test can name links.
#[derive(Debug)]
struct Serving<K> {
    /// The serving session's link, `None` for a relay or no route.
    link: Option<K>,
    /// What the serving session's own path generations are offset by.
    base: u64,
    /// The serving session's own path generation at the last sample.
    last: u64,
}

impl<K> Default for Serving<K> {
    fn default() -> Self {
        Self {
            link: None,
            base: 0,
            last: 0,
        }
    }
}

impl<K: PartialEq + fmt::Debug> Serving<K> {
    /// Returns the path generation for a sample of `link` at its own
    /// `generation`, moving past every earlier value when the link changed.
    fn generation(&mut self, link: Option<K>, generation: u64) -> u64 {
        if link != self.link {
            debug!(from = ?self.link, to = ?link, "the serving session changed");
            self.base += self.last + 1;
            self.link = link;
        }
        self.last = generation;
        self.base + generation
    }
}

/// Converts a link sample into what a player adapts on.
///
/// A round trip of zero is one not measured yet, so it is left out rather than
/// read as an instant path.
fn to_sample(link: &LinkSample, path_generation: u64) -> NetworkSample {
    let mut sample = NetworkSample::default()
        .with_loss(link.loss_rate as f32)
        .with_path_generation(path_generation);
    if !link.rtt.is_zero() {
        sample = sample.with_rtt(link.rtt);
    }
    if !link.min_rtt.is_zero() {
        sample = sample.with_min_rtt(link.min_rtt);
    }
    if let Some(delivery) = link.delivery_bps {
        sample = sample.with_delivery(Bitrate::from_bps(delivery));
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_session_moves_past_every_earlier_generation() {
        let mut serving = Serving::<u64>::default();
        let first = serving.generation(Some(1), 0);
        let moved = serving.generation(Some(1), 3);
        assert_eq!(
            moved,
            first + 3,
            "one session's own path changes carry over"
        );

        // The next session starts its own count at zero, which must not read
        // as a path the player has seen.
        let next = serving.generation(Some(2), 0);
        assert!(next > moved, "{next} does not move past {moved}");
        let relay = serving.generation(None, 0);
        assert!(relay > next);
        let back = serving.generation(Some(1), 3);
        assert!(back > relay, "returning to a session is a new path too");
    }

    #[test]
    fn an_unmeasured_round_trip_is_left_out() {
        let sample = to_sample(&LinkSample::default(), 4);
        assert_eq!(sample.rtt, None);
        assert_eq!(sample.min_rtt, None);
        assert_eq!(sample.delivery, None);
        assert_eq!(sample.path_generation, 4);
    }
}
