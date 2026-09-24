//! The push end of a source, and the demand a source reports back to it.

use std::sync::{Arc, Mutex};

use n0_watcher::Watchable;
use tokio_util::sync::CancellationToken;

use crate::{audio, error::Closed, frames::FrameSlot, video};

/// Whether anything currently wants a source's frames.
///
/// Counted rather than flagged: every encoder that runs holds a
/// [`DemandGuard`], and the source is wanted while any guard lives.
#[derive(Debug, Clone, Default)]
pub(crate) struct Demand {
    count: Arc<Mutex<usize>>,
    wanted: Watchable<bool>,
}

impl Demand {
    /// Registers one consumer that wants frames, for as long as the guard lives.
    pub(crate) fn acquire(&self) -> DemandGuard {
        let mut count = self.count.lock().expect("poisoned");
        *count += 1;
        self.wanted.set(true).ok();
        DemandGuard {
            demand: self.clone(),
        }
    }

    /// Returns a watcher over whether anything wants frames.
    pub(crate) fn watch(&self) -> n0_watcher::Direct<bool> {
        self.wanted.watch()
    }
}

/// One consumer's claim on a source's frames. Dropping it withdraws the claim.
#[derive(Debug)]
pub(crate) struct DemandGuard {
    demand: Demand,
}

impl Drop for DemandGuard {
    fn drop(&mut self) {
        let mut count = self.demand.count.lock().expect("poisoned");
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.demand.wanted.set(false).ok();
        }
    }
}

/// Where pushed frames go: a latest-wins slot for video, a bounded fan-out for
/// PCM.
pub(crate) trait Sink<T>: Send + Sync + 'static {
    /// Hands one frame on. Never blocks.
    fn deliver(&self, frame: T);
}

impl Sink<video::Frame> for FrameSlot {
    fn deliver(&self, frame: video::Frame) {
        self.send(Arc::new(frame));
    }
}

/// The fan-out a PCM source writes into: every attached broadcast reads it
/// through a receiver of its own.
pub(crate) type PcmFanout = tokio::sync::broadcast::Sender<audio::Frame>;

impl Sink<audio::Frame> for PcmFanout {
    fn deliver(&self, frame: audio::Frame) {
        // An error only means no broadcast is attached right now, and a
        // source that nobody reads drops what it produces.
        let _ = self.send(frame);
    }
}

/// The push end of a source.
///
/// Returned by [`VideoSource::push`](crate::VideoSource::push),
/// [`AudioSource::push`](crate::AudioSource::push), and handed to the closure
/// [`VideoSource::spawn`](crate::VideoSource::spawn) runs. Never blocks: a
/// video frame replaces a pending one, and PCM beyond the buffer drops its
/// oldest samples, which the broadcast reading it counts in its stats.
///
/// The source ends when every sender is dropped. Cheap to clone.
#[derive(derive_more::Debug)]
pub struct FrameSender<T> {
    #[debug(skip)]
    sink: Arc<dyn Sink<T>>,
    /// Cancelled once the source is gone, so nothing reads what is pushed.
    closed: CancellationToken,
    demand: Demand,
}

impl<T> Clone for FrameSender<T> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            closed: self.closed.clone(),
            demand: self.demand.clone(),
        }
    }
}

impl<T: 'static> FrameSender<T> {
    pub(crate) fn new(sink: Arc<dyn Sink<T>>, closed: CancellationToken, demand: Demand) -> Self {
        Self {
            sink,
            closed,
            demand,
        }
    }

    /// Pushes one frame.
    ///
    /// # Errors
    ///
    /// Returns [`Closed`] once every handle to the source has been dropped, so
    /// a producing loop knows to stop.
    pub fn push(&self, frame: T) -> Result<(), Closed> {
        if self.closed.is_cancelled() {
            return Err(Closed);
        }
        self.sink.deliver(frame);
        Ok(())
    }

    /// Returns whether any broadcast currently wants frames.
    ///
    /// True while at least one rendition of a broadcast this source feeds is
    /// encoding, which is to say while somebody watches. A camera the
    /// application drives itself can idle while this is false.
    pub fn demand(&self) -> n0_watcher::Direct<bool> {
        self.demand.watch()
    }

    /// Reports whether the source is gone.
    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// Waits until the source is gone. Cancellation safe.
    pub async fn closed(&self) {
        self.closed.cancelled().await;
    }
}

#[cfg(test)]
mod tests {
    use n0_watcher::Watcher as _;

    use super::*;

    #[test]
    fn demand_follows_the_guards() {
        let demand = Demand::default();
        let mut watch = demand.watch();
        assert!(!watch.get());
        let first = demand.acquire();
        let second = demand.acquire();
        assert!(watch.get());
        drop(first);
        assert!(watch.get(), "one consumer still wants frames");
        drop(second);
        assert!(!watch.get());
    }

    #[test]
    fn a_sender_to_a_gone_source_is_closed() {
        let slot = FrameSlot::new();
        let closed = CancellationToken::new();
        let sender: FrameSender<video::Frame> =
            FrameSender::new(Arc::new(slot), closed.clone(), Demand::default());
        assert!(!sender.is_closed());
        closed.cancel();
        let size = video::Size::new(2, 2);
        let surface = video::Surface::rgba(&[0; 16], size).expect("2x2");
        let frame = video::Frame::new(surface, moq_net::Timestamp::from_micros(0).expect("0"));
        assert_eq!(sender.push(frame), Err(Closed));
    }
}
