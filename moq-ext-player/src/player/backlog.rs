//! Every presented picture with its timing, queued for a reader that drains them at its own pace.

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use crate::stats::FrameTiming;

/// The pictures one [`VideoBacklog`] holds until its next take, oldest first.
struct Ring {
    pictures: VecDeque<(Arc<moq_video::Frame>, FrameTiming)>,
    capacity: usize,
}

impl Ring {
    /// Keeps `picture`, dropping the oldest once full.
    fn push(&mut self, picture: (Arc<moq_video::Frame>, FrameTiming)) {
        if self.pictures.len() == self.capacity {
            self.pictures.pop_front();
        }
        self.pictures.push_back(picture);
    }
}

/// The live [`VideoBacklog`] handles of one player, which the video task writes as it presents.
#[derive(Clone, Default)]
pub(crate) struct VideoBacklogs(Arc<Mutex<Vec<Weak<Mutex<Ring>>>>>);

impl fmt::Debug for VideoBacklogs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let handles = self.0.lock().expect("poisoned").len();
        f.debug_struct("VideoBacklogs")
            .field("handles", &handles)
            .finish()
    }
}

impl VideoBacklogs {
    /// Returns a new handle that keeps up to `capacity` pictures, at least one.
    pub(crate) fn open(&self, capacity: usize) -> VideoBacklog {
        let capacity = capacity.max(1);
        let ring = Arc::new(Mutex::new(Ring {
            pictures: VecDeque::new(),
            capacity,
        }));
        let mut backlogs = self.0.lock().expect("poisoned");
        // Without presented pictures `record` never runs, so the dropped
        // handles are forgotten here as well.
        backlogs.retain(|ring| ring.strong_count() > 0);
        backlogs.push(Arc::downgrade(&ring));
        VideoBacklog { ring }
    }

    /// Hands a presented picture to every live handle, and forgets the dropped ones.
    pub(crate) fn record(&self, frame: &Arc<moq_video::Frame>, timing: FrameTiming) {
        let mut backlogs = self.0.lock().expect("poisoned");
        if backlogs.is_empty() {
            return;
        }
        backlogs.retain(|ring| {
            let Some(ring) = ring.upgrade() else {
                return false;
            };
            ring.lock().expect("poisoned").push((frame.clone(), timing));
            true
        });
    }
}

/// A queue of the pictures a [`Player`](crate::Player) presents, with their timing, until they are taken.
///
/// Returned by [`Player::video_backlog`](crate::Player::video_backlog). For a
/// consumer that runs on its own tick over the recent pictures, such as an
/// analysis of each frame since its last tick. Unlike
/// [`VideoFrames`](crate::VideoFrames), it does not coalesce: each presented
/// picture is queued until [`take`](Self::take) hands it out, however late
/// that is. It holds at most its capacity, and drops the oldest beyond it.
///
/// [`take`](Self::take) drains the queue: the backlog does not keep a picture
/// it handed out, so a consumer that looks at overlapping windows keeps its
/// own copy of the pictures it still needs.
///
/// It records from its creation on. Each handle keeps its own pictures, and
/// dropping it stops the recording. A held picture keeps its decoded buffer
/// alive, which may belong to a hardware decoder's pool of a fixed size, so a
/// backlog that is kept but no longer taken can stall or slow down hardware
/// decoding. Drop the handle when you stop taking.
///
/// The backlog does not tell when playback ends: after the player stops or
/// the broadcast ends, [`take`](Self::take) returns an empty list, as it does
/// when the video stalls. Watch [`Player::status`](crate::Player::status) for
/// the end.
pub struct VideoBacklog {
    ring: Arc<Mutex<Ring>>,
}

impl fmt::Debug for VideoBacklog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ring = self.ring.lock().expect("poisoned");
        f.debug_struct("VideoBacklog")
            .field("held", &ring.pictures.len())
            .field("capacity", &ring.capacity)
            .finish()
    }
}

impl VideoBacklog {
    /// Takes every picture presented since the last take, oldest first, with its timing.
    ///
    /// The pictures come in the order the player presented them, each once,
    /// and the backlog is empty afterwards. Returns an empty list if nothing
    /// was presented since the last take.
    #[must_use]
    pub fn take(&self) -> Vec<(Arc<moq_video::Frame>, FrameTiming)> {
        // Moves the queue out, so the copy into the list happens outside the
        // lock the video task needs to present.
        let pictures = std::mem::take(&mut self.ring.lock().expect("poisoned").pictures);
        pictures.into()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::stats::MediaKind;

    /// Returns a picture stamped `ms`, with its timing.
    fn picture(ms: u64) -> (Arc<moq_video::Frame>, FrameTiming) {
        let timestamp = moq_net::Timestamp::from_micros(ms * 1000).expect("a valid timestamp");
        let i420 =
            moq_video::I420::new(moq_video::Size::new(2, 2), vec![0; 6]).expect("a 2x2 I420");
        let frame = Arc::new(moq_video::Frame::new(
            moq_video::Surface::I420(i420),
            timestamp,
        ));
        let now = Instant::now();
        let timing = FrameTiming {
            kind: MediaKind::Video,
            pts: Duration::from_millis(ms),
            arrived: None,
            decoded: now,
            presented: now,
        };
        (frame, timing)
    }

    /// Returns the timestamps of `pictures` in ms.
    fn stamps(pictures: &[(Arc<moq_video::Frame>, FrameTiming)]) -> Vec<u128> {
        pictures
            .iter()
            .map(|(_, timing)| timing.pts.as_millis())
            .collect()
    }

    fn record(backlogs: &VideoBacklogs, ms: u64) {
        let (frame, timing) = picture(ms);
        backlogs.record(&frame, timing);
    }

    #[test]
    fn every_picture_between_two_takes_comes_out_once_in_order() {
        let backlogs = VideoBacklogs::default();
        let backlog = backlogs.open(8);
        assert!(backlog.take().is_empty(), "nothing presented yet");
        for ms in [0, 33, 66] {
            record(&backlogs, ms);
        }
        assert_eq!(stamps(&backlog.take()), [0, 33, 66]);
        assert!(backlog.take().is_empty(), "nothing new");
        record(&backlogs, 100);
        assert_eq!(stamps(&backlog.take()), [100]);
    }

    #[test]
    fn a_full_backlog_drops_the_oldest() {
        let backlogs = VideoBacklogs::default();
        let backlog = backlogs.open(2);
        for ms in [0, 33, 66] {
            record(&backlogs, ms);
        }
        assert_eq!(stamps(&backlog.take()), [33, 66]);
        let one = backlogs.open(0);
        record(&backlogs, 100);
        record(&backlogs, 133);
        assert_eq!(stamps(&one.take()), [133], "a capacity of 0 keeps one");
    }

    #[test]
    fn each_handle_keeps_its_own_pictures_from_its_creation_on() {
        let backlogs = VideoBacklogs::default();
        let first = backlogs.open(8);
        record(&backlogs, 0);
        let second = backlogs.open(8);
        record(&backlogs, 33);
        assert_eq!(stamps(&first.take()), [0, 33]);
        assert_eq!(stamps(&second.take()), [33]);
        drop(first);
        record(&backlogs, 66);
        assert_eq!(backlogs.0.lock().expect("poisoned").len(), 1);
        assert_eq!(stamps(&second.take()), [66]);
    }

    #[test]
    fn dropped_handles_are_forgotten_without_presented_pictures() {
        let backlogs = VideoBacklogs::default();
        let kept = backlogs.open(8);
        for _ in 0..10 {
            drop(backlogs.open(8));
        }
        // The kept handle and the last one dropped, which the next open forgets.
        assert_eq!(backlogs.0.lock().expect("poisoned").len(), 2);
        drop(kept);
    }
}
