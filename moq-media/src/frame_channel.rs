//! A single-slot channel holding the latest decoded frame.
//!
//! The producer overwrites the slot on every send, dropping whatever was
//! there. The consumer takes the value out and owns it, without cloning.
//! [`FrameReceiver::take`] returns `None` when nothing has arrived since the
//! last one.
//!
//! A queue would be wrong for this. A renderer that falls behind wants the
//! newest picture, not the oldest, and a backlog of frames is a backlog of GPU
//! surfaces held out of the decoder's pool. One slot bounds that at a single
//! frame, and drops the ones nobody will draw at the producer rather than
//! carrying them to a consumer that will discard them.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use tokio::sync::Notify;

struct SlotInner<T> {
    value: Mutex<Option<T>>,
    /// Monotonic count of values sent. Lets consumers check production
    /// count without needing to observe every value.
    produced: AtomicU64,
    /// Number of live senders. When this drops to zero, the channel is
    /// considered closed.
    sender_count: AtomicU64,
    /// Wakes [`FrameReceiver::recv`] waiters on send or close.
    notify: Notify,
}

/// Sender half of a single-slot frame channel.
///
/// Each [`send`](Self::send) replaces the current value, dropping the
/// previous one. Never blocks. When dropped, signals the receiver
/// that no more values will arrive.
pub struct FrameSender<T> {
    inner: Arc<SlotInner<T>>,
}

/// Receiver half of a single-slot frame channel.
///
/// [`take`](Self::take) returns the latest value if one has arrived
/// since the last take. [`recv`](Self::recv) waits asynchronously
/// for the next value: primarily useful in tests.
pub struct FrameReceiver<T> {
    inner: Arc<SlotInner<T>>,
}

/// Creates a single-slot frame channel.
///
/// The sender overwrites the current value on each send. The receiver
/// takes the latest value out. At most one value is buffered.
pub fn frame_channel<T>() -> (FrameSender<T>, FrameReceiver<T>) {
    let inner = Arc::new(SlotInner {
        value: Mutex::new(None),
        produced: AtomicU64::new(0),
        sender_count: AtomicU64::new(1),
        notify: Notify::new(),
    });
    (
        FrameSender {
            inner: inner.clone(),
        },
        FrameReceiver { inner },
    )
}

impl<T> FrameSender<T> {
    /// Replaces the current value, dropping the old one.
    ///
    /// Never blocks. Wakes any [`FrameReceiver::recv`] waiter.
    pub fn send(&self, value: T) {
        *self.inner.value.lock().expect("poisoned") = Some(value);
        // Released so a `FrameWatcher` that reads this counter sees the value
        // that was put in the slot before it, rather than pairing with nothing.
        self.inner.produced.fetch_add(1, Ordering::Release);
        self.inner.notify.notify_waiters();
    }
}

impl<T> Clone for FrameSender<T> {
    fn clone(&self) -> Self {
        self.inner.sender_count.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for FrameSender<T> {
    fn drop(&mut self) {
        if self.inner.sender_count.fetch_sub(1, Ordering::Release) == 1 {
            // Last sender dropped: wake waiters so they see closure.
            self.inner.notify.notify_waiters();
        }
    }
}

impl<T> FrameReceiver<T> {
    /// Takes the latest value if one has arrived since the last take.
    ///
    /// Returns `None` if nothing new. Non-blocking.
    pub fn take(&self) -> Option<T> {
        self.inner.value.lock().expect("poisoned").take()
    }

    /// Returns `true` if a value is available without consuming it.
    pub fn has_value(&self) -> bool {
        self.inner.value.lock().expect("poisoned").is_some()
    }

    /// Returns `true` if all senders have been dropped.
    pub fn is_closed(&self) -> bool {
        self.inner.sender_count.load(Ordering::Acquire) == 0
    }

    /// Total number of values sent, including ones overwritten before
    /// the consumer could take them.
    pub fn produced(&self) -> u64 {
        self.inner.produced.load(Ordering::Relaxed)
    }

    /// Waits until every sender has been dropped.
    ///
    /// For a reader that has to notice the source ending while it is waiting on
    /// something else, rather than while it is waiting for a frame.
    pub async fn closed(&self) {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_closed() {
                return;
            }
            notified.await;
        }
    }

    /// Returns a watcher that reports arrivals without consuming them.
    ///
    /// That is what a drawing loop wants: something has to be woken when a
    /// picture arrives, and it is not the thing that takes it. Without it a
    /// renderer has to poll, and polling a slot fed by a playout clock adds its
    /// own interval to every frame's latency and quantises the spacing between
    /// them. A 30fps stream sampled every 16ms is presented on the sampler's
    /// grid rather than the stream's, which looks like judder because it is.
    ///
    /// The watcher owns a handle of its own, so it outlives a borrow of the
    /// receiver and can be moved into the task that does the waking.
    pub fn watch(&self) -> FrameWatcher<T> {
        FrameWatcher {
            inner: Arc::clone(&self.inner),
            seen: self.inner.produced.load(Ordering::Acquire),
        }
    }

    /// Waits for the next value. Returns `None` when the sender is
    /// dropped and no value remains.
    ///
    /// If multiple values arrive between calls, intermediate ones are
    /// lost: only the latest is returned.
    pub async fn recv(&self) -> Option<T> {
        loop {
            // Register for the wakeup before checking, not after. `notified()`
            // only covers notifications from the moment it is enabled, so a
            // sender dropping between a check and the registration would be
            // missed and this would park with nothing left to wake it.
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if let Some(v) = self.take() {
                return Some(v);
            }
            if self.is_closed() {
                return None;
            }
            notified.await;
        }
    }
}

/// Watches a slot for arrivals, without taking anything out of it.
///
/// From [`FrameReceiver::watch`]. Counts sends rather than looking at the slot,
/// which is what keeps [`changed`](Self::changed) from returning over and over
/// while a value nobody has taken yet sits there: a waker that did that would
/// spin, and between a frame landing and the drawing pass taking it there is no
/// point at which it would yield.
pub struct FrameWatcher<T> {
    inner: Arc<SlotInner<T>>,
    /// How many sends this watcher has already reported.
    seen: u64,
}

impl<T> std::fmt::Debug for FrameWatcher<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameWatcher")
            .field("seen", &self.seen)
            .field("produced", &self.inner.produced.load(Ordering::Relaxed))
            .finish()
    }
}

impl<T> FrameWatcher<T> {
    /// Waits until something has been sent that this watcher has not reported,
    /// and returns `false` once every sender is gone.
    ///
    /// Sends that arrive faster than the watcher reports them coalesce into
    /// one, which is right for a renderer: only the newest value is still in
    /// the slot.
    pub async fn changed(&mut self) -> bool {
        loop {
            // Registered before the check, for the reason [`FrameReceiver::recv`]
            // gives.
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let produced = self.inner.produced.load(Ordering::Acquire);
            if produced != self.seen {
                self.seen = produced;
                return true;
            }
            // Checked after the count, so a value sent just before the last
            // sender dropped is reported before the closure is.
            if self.inner.sender_count.load(Ordering::Acquire) == 0 {
                return false;
            }
            notified.await;
        }
    }
}

// Convenience Debug impls: don't expose the value.
impl<T> std::fmt::Debug for FrameSender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameSender")
            .field("produced", &self.inner.produced.load(Ordering::Relaxed))
            .finish()
    }
}

impl<T> std::fmt::Debug for FrameReceiver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameReceiver")
            .field("produced", &self.inner.produced.load(Ordering::Relaxed))
            .field("closed", &self.is_closed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_and_take() {
        let (tx, rx) = frame_channel::<u32>();
        assert!(rx.take().is_none());

        tx.send(1);
        tx.send(2);
        tx.send(3);
        // Only the latest value is available.
        assert_eq!(rx.take(), Some(3));
        assert!(rx.take().is_none());
        assert_eq!(rx.produced(), 3);
    }

    #[test]
    fn close_signal() {
        let (tx, rx) = frame_channel::<u32>();
        assert!(!rx.is_closed());
        drop(tx);
        assert!(rx.is_closed());
    }

    #[tokio::test]
    async fn recv_returns_none_on_close() {
        let (tx, rx) = frame_channel::<u32>();
        tx.send(42);
        drop(tx);
        assert_eq!(rx.recv().await, Some(42));
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn recv_wakes_on_send() {
        let (tx, rx) = frame_channel::<u32>();
        let handle = tokio::spawn(async move { rx.recv().await });
        // Small delay to ensure recv is waiting.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        tx.send(7);
        assert_eq!(handle.await.unwrap(), Some(7));
    }

    #[test]
    fn overwrite_drops_old_value() {
        use std::sync::{Arc, atomic::AtomicUsize};

        #[derive(Clone)]
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let (tx, _rx) = frame_channel();
        tx.send(Counted(drops.clone()));
        tx.send(Counted(drops.clone())); // first value dropped
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn clone_sender_keeps_channel_open() {
        let (tx, rx) = frame_channel::<u32>();
        let second = tx.clone();
        drop(tx);
        assert!(!rx.is_closed(), "one sender is still alive");
        second.send(99);
        assert_eq!(rx.take(), Some(99));
        drop(second);
        assert!(rx.is_closed());
    }

    #[tokio::test]
    async fn recv_sees_a_close_that_races_the_check() {
        // `recv` has to register for its wakeup before checking, or a sender
        // dropping between the check and the registration leaves it parked with
        // nothing left to wake it.
        for _ in 0..64 {
            let (tx, rx) = frame_channel::<u32>();
            let closer = std::thread::spawn(move || drop(tx));
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                    .await
                    .expect("recv should observe the close"),
                None,
            );
            closer.join().unwrap();
        }
    }

    /// A watcher leaves the value where it is: the drawing pass is what takes
    /// it, and a waker that consumed the picture would draw nothing.
    #[tokio::test]
    async fn a_watcher_reports_without_taking() {
        let (tx, rx) = frame_channel::<u32>();
        let mut watcher = rx.watch();
        tx.send(7);
        assert!(watcher.changed().await);
        assert!(rx.has_value(), "the watcher took the value");
        assert_eq!(rx.take(), Some(7));
    }

    /// Regression: the old `arrived` returned while a value sat in the slot, so
    /// a waker that does not consume had no pending point between a frame
    /// landing and the drawing pass taking it. It pinned a runtime worker for
    /// that whole window, every frame.
    #[tokio::test]
    async fn a_reported_value_nobody_took_does_not_report_again() {
        let (tx, rx) = frame_channel::<u32>();
        let mut watcher = rx.watch();
        tx.send(7);
        assert!(watcher.changed().await);
        // Deliberately not taken, which is the case that used to spin.
        assert!(rx.has_value());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), watcher.changed())
                .await
                .is_err(),
            "an unconsumed value must not be reported a second time",
        );
    }

    /// Sends that outrun the watcher coalesce, because only the newest value is
    /// still in the slot for it to point at.
    #[tokio::test]
    async fn sends_that_outrun_the_watcher_coalesce() {
        let (tx, rx) = frame_channel::<u32>();
        let mut watcher = rx.watch();
        tx.send(1);
        tx.send(2);
        tx.send(3);
        assert!(watcher.changed().await);
        assert_eq!(rx.take(), Some(3));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), watcher.changed())
                .await
                .is_err(),
            "three sends before one report are one report",
        );
    }

    /// It returns when the last sender goes, so a waker parked on a track that
    /// ended is not parked forever.
    #[tokio::test]
    async fn a_watcher_stops_when_the_sender_goes() {
        let (tx, rx) = frame_channel::<u32>();
        let mut watcher = rx.watch();
        drop(tx);
        assert!(!watcher.changed().await);
    }

    /// A value sent just before the last sender dropped is still reported, so
    /// the final picture of a track reaches the screen.
    #[tokio::test]
    async fn a_final_value_is_reported_before_the_close() {
        let (tx, rx) = frame_channel::<u32>();
        let mut watcher = rx.watch();
        tx.send(9);
        drop(tx);
        assert!(watcher.changed().await, "the last frame was skipped");
        assert!(!watcher.changed().await);
    }
}
