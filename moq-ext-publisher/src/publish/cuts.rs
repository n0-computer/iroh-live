//! Which frames to open a new group on, for subscribers that join.
//!
//! A copy of moq-video's private `encode::cuts`, which its capture loop uses
//! to answer a joiner's request for a fresh group. A track that sets
//! `track::Info::refresh` lets a subscriber that joins over the refresh ratio
//! ask for a group past the largest written, through `Demand::wanted`.

/// One encoder's view of the keyframe requests: decides which frames to cut, coalescing.
///
/// Built fresh for every encoder a rendition opens, since a fresh encoder
/// opens with a keyframe that serves every earlier request. Joins while the
/// keyframe is still in the encoder ask for the same group and coalesce into
/// it. There is no spacing between cuts: the refresh ratio already limits
/// joins by the bytes a group holds.
#[derive(Debug, Default)]
pub(super) struct Cuts {
    /// The largest wanted group already cut for or served, or `None` before any.
    wanted: Option<u64>,
    /// Whether this encoder produced its first frame.
    opened: bool,
}

impl Cuts {
    /// Returns whether the next frame should be cut, given the group `wanted` asks for.
    pub(super) fn due(&mut self, wanted: Option<u64>) -> bool {
        let asked = wanted > self.wanted;
        self.wanted = self.wanted.max(wanted);
        // A fresh encoder opens with a keyframe on every backend, which serves
        // anything asked for before it.
        if !self.opened {
            self.opened = true;
            return false;
        }
        asked
    }

    /// Records that a keyframe the encoder made on its own served the group `wanted` asks for.
    ///
    /// moq-video compares the request with the largest group written, which
    /// the publisher cannot read here. A scheduled keyframe that lands while
    /// a request waits answers it, and this keeps it from causing a second
    /// cut.
    pub(super) fn served(&mut self, wanted: Option<u64>) {
        self.wanted = self.wanted.max(wanted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opening_keyframe_serves_earlier_asks() {
        let mut cuts = Cuts::default();
        assert!(!cuts.due(Some(4)));
        assert!(!cuts.due(Some(4)), "the ask was already served");
    }

    /// A join cuts once per wanted group.
    ///
    /// The frames still in the encoder ask for the same group, and a group
    /// that lands drops the ask until another join raises it.
    #[test]
    fn a_wanted_group_cuts_once() {
        let mut cuts = Cuts::default();
        assert!(!cuts.due(None));
        assert!(cuts.due(Some(1)), "a join past the largest group");
        assert!(!cuts.due(Some(1)), "the keyframe is still in the encoder");
        assert!(!cuts.due(None), "group 1 landed");
        assert!(cuts.due(Some(2)), "the next join");
    }

    /// Back-to-back joins each cut: the refresh ratio decides how often they arrive.
    #[test]
    fn joins_are_not_spaced() {
        let mut cuts = Cuts::default();
        assert!(!cuts.due(None));
        assert!(cuts.due(Some(1)));
        assert!(cuts.due(Some(2)), "a join one frame after a cut still cuts");
    }

    /// A scheduled keyframe that answered a request leaves nothing to cut for it.
    #[test]
    fn a_served_request_does_not_cut() {
        let mut cuts = Cuts::default();
        assert!(!cuts.due(None));
        cuts.served(Some(3));
        assert!(!cuts.due(Some(3)), "the scheduled keyframe opened group 3");
        assert!(cuts.due(Some(4)), "a later join still cuts");
    }
}
