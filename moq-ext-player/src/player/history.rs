//! Every presented picture with its timing, kept for a reader that takes them at its own pace.

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use crate::stats::FrameTiming;

/// The pictures one [`VideoHistory`] holds until its next take, oldest first.
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

/// The live [`VideoHistory`] handles of one player, which the video task writes as it presents.
#[derive(Clone, Default)]
pub(crate) struct VideoHistories(Arc<Mutex<Vec<Weak<Mutex<Ring>>>>>);

impl fmt::Debug for VideoHistories {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let handles = self.0.lock().expect("poisoned").len();
        f.debug_struct("VideoHistories")
            .field("handles", &handles)
            .finish()
    }
}

impl VideoHistories {
    /// Returns a new handle that keeps up to `capacity` pictures, at least one.
    pub(crate) fn open(&self, capacity: usize) -> VideoHistory {
        let capacity = capacity.max(1);
        let ring = Arc::new(Mutex::new(Ring {
            pictures: VecDeque::new(),
            capacity,
        }));
        self.0.lock().expect("poisoned").push(Arc::downgrade(&ring));
        VideoHistory { ring }
    }

    /// Hands a presented picture to every live handle, and forgets the dropped ones.
    pub(crate) fn record(&self, frame: &Arc<moq_video::Frame>, timing: FrameTiming) {
        let mut histories = self.0.lock().expect("poisoned");
        if histories.is_empty() {
            return;
        }
        histories.retain(|ring| {
            let Some(ring) = ring.upgrade() else {
                return false;
            };
            ring.lock().expect("poisoned").push((frame.clone(), timing));
            true
        });
    }
}

/// Every picture a [`Player`](crate::Player) presents, with its timing, until it is taken.
///
/// Returned by [`Player::video_history`](crate::Player::video_history). For a
/// consumer that runs on its own tick over the recent pictures, such as an
/// analysis over the last 100 ms. Unlike
/// [`VideoFrames`](crate::VideoFrames), it does not coalesce: each presented
/// picture is kept until [`take`](Self::take) hands it out, however late
/// that is. It holds at most its capacity, and drops the oldest beyond it.
///
/// It records from its creation on. Each handle keeps its own pictures, and
/// dropping it stops the recording.
pub struct VideoHistory {
    ring: Arc<Mutex<Ring>>,
}

impl fmt::Debug for VideoHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ring = self.ring.lock().expect("poisoned");
        f.debug_struct("VideoHistory")
            .field("held", &ring.pictures.len())
            .field("capacity", &ring.capacity)
            .finish()
    }
}

impl VideoHistory {
    /// Takes every picture presented since the last take, oldest first, with its timing.
    ///
    /// The pictures come in the order the player presented them, each once.
    /// Returns an empty list if nothing was presented since the last take.
    #[must_use]
    pub fn take(&self) -> Vec<(Arc<moq_video::Frame>, FrameTiming)> {
        self.ring
            .lock()
            .expect("poisoned")
            .pictures
            .drain(..)
            .collect()
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

    fn record(histories: &VideoHistories, ms: u64) {
        let (frame, timing) = picture(ms);
        histories.record(&frame, timing);
    }

    #[test]
    fn every_picture_between_two_takes_comes_out_once_in_order() {
        let histories = VideoHistories::default();
        let history = histories.open(8);
        assert!(history.take().is_empty(), "nothing presented yet");
        for ms in [0, 33, 66] {
            record(&histories, ms);
        }
        assert_eq!(stamps(&history.take()), [0, 33, 66]);
        assert!(history.take().is_empty(), "nothing new");
        record(&histories, 100);
        assert_eq!(stamps(&history.take()), [100]);
    }

    #[test]
    fn a_full_history_drops_the_oldest() {
        let histories = VideoHistories::default();
        let history = histories.open(2);
        for ms in [0, 33, 66] {
            record(&histories, ms);
        }
        assert_eq!(stamps(&history.take()), [33, 66]);
        let one = histories.open(0);
        record(&histories, 100);
        record(&histories, 133);
        assert_eq!(stamps(&one.take()), [133], "a capacity of 0 keeps one");
    }

    #[test]
    fn each_handle_keeps_its_own_pictures_from_its_creation_on() {
        let histories = VideoHistories::default();
        let first = histories.open(8);
        record(&histories, 0);
        let second = histories.open(8);
        record(&histories, 33);
        assert_eq!(stamps(&first.take()), [0, 33]);
        assert_eq!(stamps(&second.take()), [33]);
        drop(first);
        record(&histories, 66);
        assert_eq!(histories.0.lock().expect("poisoned").len(), 1);
        assert_eq!(stamps(&second.take()), [66]);
    }
}
