//! When each frame of a video track reached this node, before decoding.
//!
//! `moq_video::decode::Consumer` reads the track and decodes it in one call,
//! so the time a picture spent on the network and the time it spent in the
//! decoder add up in what the reader measures. The arrival is on every
//! `moq_net` frame, but the decoder's container layer drops it.
//!
//! [`watch`] reads the same track beside the decoder, with the same
//! subscription, so moq-net serves both from one upstream request. It decodes
//! only the container to learn each frame's timestamp, and records when the
//! frame landed. The reader then looks up each decoded picture by timestamp.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    task::Poll,
    time::Instant,
};

use moq_mux::container::Container as _;
use moq_net::kio;
use tracing::debug;

/// How many frames' arrivals a track keeps.
///
/// Eight seconds at 30 fps. A picture leaves the decoder a few frames after
/// its data arrived, so this covers any decoder delay with room.
const ARRIVALS_LEN: usize = 256;

/// How many groups [`watch`] reads at once.
///
/// A live track has one open group, and a second while the next one starts.
/// A group still open behind these is late, and its frames are not needed.
const OPEN_GROUPS: usize = 4;

/// The arrival of the last [`ARRIVALS_LEN`] frames of one track, by timestamp.
#[derive(Debug, Clone, Default)]
pub(crate) struct Arrivals(Arc<Mutex<VecDeque<(moq_net::Timestamp, Instant)>>>);

impl Arrivals {
    /// Records that the frame at `timestamp` arrived at `at`, dropping the oldest once full.
    fn record(&self, timestamp: moq_net::Timestamp, at: Instant) {
        let mut arrivals = self.0.lock().expect("poisoned");
        if arrivals.len() >= ARRIVALS_LEN {
            arrivals.pop_front();
        }
        arrivals.push_back((timestamp, at));
    }

    /// Returns when the frame at `timestamp` arrived, if it was seen.
    pub(crate) fn get(&self, timestamp: moq_net::Timestamp) -> Option<Instant> {
        let arrivals = self.0.lock().expect("poisoned");
        // The frame is usually among the newest, since a decoder runs only a
        // few frames behind the network.
        arrivals
            .iter()
            .rev()
            .find(|(seen, _)| *seen == timestamp)
            .map(|(_, at)| *at)
    }
}

/// The next thing that happened on the watched track.
enum Next {
    /// A group started, or the track ended or failed.
    Group(moq_net::Result<Option<moq_net::group::Consumer>>),
    /// The open group at this index read a frame, ended, or failed.
    Frame(usize, moq_net::Result<Option<moq_net::frame::Frame>>),
}

/// Records the arrival of every frame of `track` in `arrivals`, until the track and its groups end.
///
/// `subscription` must be the decoder's, so the two share one upstream
/// request rather than asking the publisher for different delays.
pub(crate) async fn watch(
    track: moq_net::track::Consumer,
    subscription: moq_net::track::Subscription,
    container: moq_mux::catalog::hang::Container,
    arrivals: Arrivals,
) {
    let mut subscriber = match track.subscribe(subscription).await {
        Ok(subscriber) => subscriber,
        Err(err) => {
            debug!(error = %err, "cannot watch the track's arrivals");
            return;
        }
    };
    // Only the live media matters, as for the decoder. The group before the
    // live edge covers a decoder that joins a group early.
    if let Some(live_edge) = subscriber.largest().await {
        subscriber = subscriber.with_range(live_edge.group.saturating_sub(1)..);
    }
    let mut groups: VecDeque<moq_net::group::Consumer> = VecDeque::new();
    // Once the track has ended, its open groups are still read to their end.
    let mut ended = false;
    while !(ended && groups.is_empty()) {
        let next = kio::wait(|waiter| {
            if !ended && let Poll::Ready(group) = subscriber.poll_recv_group(waiter) {
                return Poll::Ready(Next::Group(group));
            }
            for (index, group) in groups.iter_mut().enumerate() {
                if let Poll::Ready(frame) = group.poll_read_frame(waiter) {
                    return Poll::Ready(Next::Frame(index, frame));
                }
            }
            Poll::Pending
        })
        .await;
        match next {
            Next::Group(Ok(Some(group))) => {
                if groups.len() >= OPEN_GROUPS {
                    groups.pop_front();
                }
                groups.push_back(group);
            }
            Next::Group(Ok(None)) => ended = true,
            Next::Group(Err(err)) => {
                debug!(error = %err, "the watched track closed");
                return;
            }
            Next::Frame(_, Ok(Some(frame))) => {
                // The arrival is on moq-net's clock. Its age moves it onto
                // std's, which the rest of the timeline uses.
                let age = frame.arrived().elapsed();
                let Some(at) = Instant::now().checked_sub(age) else {
                    continue;
                };
                // An undecodable frame fails the decoder too, which reports it.
                let Ok(decoded) = container.decode(frame) else {
                    continue;
                };
                for frame in decoded {
                    arrivals.record(frame.timestamp, at);
                }
            }
            // An ended group and a group dropped as too old are both done.
            Next::Frame(index, Ok(None) | Err(_)) => {
                groups.remove(index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn ts(micros: u64) -> moq_net::Timestamp {
        moq_net::Timestamp::from_micros(micros).expect("in range")
    }

    /// A lookup finds the newest record of a timestamp, and the oldest fall out.
    #[test]
    fn arrivals_keep_the_newest_frames() {
        let arrivals = Arrivals::default();
        let start = Instant::now();
        for index in 0..ARRIVALS_LEN as u64 + 10 {
            arrivals.record(ts(index), start + Duration::from_millis(index));
        }
        assert_eq!(arrivals.get(ts(5)), None, "the oldest frames are dropped");
        assert_eq!(
            arrivals.get(ts(20)),
            Some(start + Duration::from_millis(20))
        );
        assert_eq!(arrivals.get(ts(ARRIVALS_LEN as u64 + 100)), None);
    }
}
