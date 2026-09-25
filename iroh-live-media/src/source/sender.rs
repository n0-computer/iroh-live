//! The push end of a source, and the demand a source reports back to it.

use std::sync::{Arc, Mutex};

use n0_watcher::Watchable;
use tokio_util::sync::CancellationToken;

use crate::{audio, error::Closed, frames::FrameSlot, video};

/// Whether anything currently wants a source's frames.
///
/// Every running encoder holds a [`DemandGuard`]. The source is wanted while
/// any guard lives.
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

/// One consumer's claim on a source's frames, withdrawn on drop.
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

/// Where pushed frames go.
///
/// Video goes to a latest-wins slot, PCM to a bounded fan-out.
pub(crate) trait Sink<T>: Send + Sync + 'static {
    /// Hands one frame on without blocking.
    fn deliver(&self, frame: T);
}

impl Sink<video::Frame> for FrameSlot {
    fn deliver(&self, frame: video::Frame) {
        self.send(Arc::new(frame));
    }
}

/// The fan-out a PCM source writes into.
///
/// Every attached broadcast reads it through its own receiver.
pub(crate) type PcmFanout = tokio::sync::broadcast::Sender<audio::Frame>;

/// A [`PcmFanout`] that does not keep the source alive.
pub(crate) type WeakPcmFanout = tokio::sync::broadcast::WeakSender<audio::Frame>;

impl Sink<audio::Frame> for PcmFanout {
    fn deliver(&self, frame: audio::Frame) {
        // An error only means no broadcast is attached, and the frame is
        // dropped.
        let _ = self.send(frame);
    }
}

/// The push end of a source.
///
/// [`VideoSource::push`](crate::VideoSource::push) and
/// [`AudioSource::push`](crate::AudioSource::push) return one, and
/// [`VideoSource::spawn`](crate::VideoSource::spawn) hands one to its closure.
/// Pushing never blocks. A video frame replaces a pending one. PCM beyond the
/// buffer drops the oldest frames, and the broadcast reading it counts them in
/// its stats.
///
/// The source ends when every sender is dropped. Cheap to clone.
#[derive(derive_more::Debug)]
pub struct FrameSender<T> {
    #[debug(skip)]
    sink: Arc<dyn Sink<T>>,
    /// Cancelled once the source is gone.
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
    /// Returns [`Closed`] once every handle to the source has been dropped.
    /// A producing loop should stop then.
    pub fn push(&self, frame: T) -> Result<(), Closed> {
        if self.closed.is_cancelled() {
            return Err(Closed);
        }
        self.sink.deliver(frame);
        Ok(())
    }

    /// Returns a watcher over whether any broadcast wants frames.
    ///
    /// It is true while a broadcast this source feeds encodes it, which for
    /// video means while somebody watches. A camera the application drives
    /// itself can idle while it is false.
    pub fn demand(&self) -> n0_watcher::Direct<bool> {
        self.demand.watch()
    }

    /// Reports whether the source is gone.
    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// Waits until the source is gone.
    ///
    /// Cancellation safe.
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
