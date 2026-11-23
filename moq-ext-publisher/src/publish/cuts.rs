//! Which frames to open a new group on, for subscribers that join.
//!
//! A track that sets `track::Info::refresh` lets a subscriber that joins over
//! the refresh ratio ask for a new group, which `Demand::poll_refresh` reports
//! until the next group lands.

/// One encoder's view of the keyframe requests: decides which frames to cut, coalescing.
///
/// A request stays up until a group lands, which takes as long as the frames
/// still in the encoder. So after a cut, or while a fresh encoder's opening
/// keyframe is on its way, every request waits for that keyframe instead of
/// cutting again. There is no spacing between cuts: the refresh ratio already
/// limits joins by the bytes a group holds.
#[derive(Debug)]
pub(super) struct Cuts {
    /// Whether a keyframe is on its way that answers every request so far.
    keyframe_coming: bool,
}

impl Cuts {
    /// Returns the state of a fresh encoder, which opens with a keyframe on every backend.
    pub(super) fn opening() -> Self {
        Self {
            keyframe_coming: true,
        }
    }

    /// Returns whether the next frame should be cut, given whether a subscriber asks for a group.
    pub(super) fn due(&mut self, asked: bool) -> bool {
        let due = asked && !self.keyframe_coming;
        self.keyframe_coming |= due;
        due
    }

    /// Records that a keyframe was published, which answers every request so far.
    pub(super) fn served(&mut self) {
        self.keyframe_coming = false;
    }
}

/// Returns whether a subscriber of the track behind `demand` asks for a new group now.
pub(super) fn asked(demand: &moq_net::track::Demand) -> bool {
    matches!(
        demand.poll_refresh(&kio::Waiter::noop()),
        std::task::Poll::Ready(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opening_keyframe_serves_earlier_asks() {
        let mut cuts = Cuts::opening();
        assert!(!cuts.due(true));
        assert!(
            !cuts.due(true),
            "the opening keyframe is still in the encoder"
        );
        cuts.served();
        assert!(cuts.due(true), "a join after the opening keyframe");
    }

    /// A join cuts once, and the frames still in the encoder wait for that keyframe.
    #[test]
    fn a_request_cuts_once() {
        let mut cuts = Cuts::opening();
        cuts.served();
        assert!(!cuts.due(false));
        assert!(cuts.due(true), "a join");
        assert!(!cuts.due(true), "the keyframe is still in the encoder");
        cuts.served();
        assert!(!cuts.due(false), "the group landed");
        assert!(cuts.due(true), "the next join");
    }

    /// A track asks for its first group, and a live join after a delta asks
    /// again until the next group lands.
    #[tokio::test]
    async fn asked_follows_the_tracks_requests() {
        use std::time::Duration;

        use moq_net::{
            Timestamp,
            track::{Info, Range, Refresh, Start, Subscription},
        };

        // A track without a timescale keys its groups by unscaled timestamps.
        let at = Timestamp::unscaled;
        let broadcast = moq_net::broadcast::Info::new().produce();
        let info = Info::default().with_refresh(Refresh::default().with_ratio(0));
        let track = broadcast
            .create_track("video", Some(info))
            .expect("a new track");
        let demand = track.demand();
        let _first = track.subscribe(None);
        assert!(asked(&demand), "a track with no group asks for its first");

        let mut group = track.append_group(at(0)).expect("a group");
        group.write_frame(at(0), &b"key"[..]).expect("a frame");
        assert!(!asked(&demand), "the first group answered it");
        group.write_frame(at(0), &b"delta"[..]).expect("a frame");
        assert!(!asked(&demand), "a delta asks nothing");

        let live = Subscription::default().with_ranges([Range::from(Start::Live(Duration::ZERO))]);
        let _late = track.subscribe(live);
        assert!(asked(&demand), "a live join after a delta asks for a group");
        group.finish().expect("finished");
        assert!(asked(&demand), "the request holds until a group lands");

        let mut next = track.append_group(at(1)).expect("a group");
        next.write_frame(at(1), &b"key"[..]).expect("a frame");
        assert!(!asked(&demand), "the next group answered it");
    }

    /// A scheduled keyframe that answered a request leaves nothing to cut for it.
    #[test]
    fn a_served_request_does_not_cut() {
        let mut cuts = Cuts::opening();
        cuts.served();
        assert!(cuts.due(true));
        cuts.served();
        assert!(!cuts.due(false), "the scheduled keyframe opened the group");
    }
}
