//! Single-slot "latest value" channel for decoded video frames.
//!
//! The producer overwrites the current value on each send, dropping
//! whatever was there. The consumer takes the value out, getting
//! ownership without cloning. If no new value has arrived since the
//! last take, [`FrameReceiver::take`] returns `None`.
//!
//! This replaces a bounded `mpsc::channel(32)` that was drained to
//! the latest frame on every consume — wasting decode effort and
//! holding up to 32 GPU surfaces in flight. With a single slot, at
//! most one frame is buffered, and overwritten frames are dropped
//! immediately at the producer (before the consumer ever sees them).

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
/// for the next value — primarily useful in tests.
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
        self.inner.produced.fetch_add(1, Ordering::Relaxed);
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
            // Last sender dropped — wake waiters so they see closure.
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

    /// Creates a new [`FrameSender`] that writes to the same slot.
    ///
    /// Used by the adaptation layer to redirect a new decoder pipeline's
    /// output to the same receiver the consumer already holds. The
    /// previous sender (from the old pipeline) can be dropped without
    /// closing the channel as long as this new sender is alive.
    pub fn new_sender(&self) -> FrameSender<T> {
        self.inner.sender_count.fetch_add(1, Ordering::Relaxed);
        FrameSender {
            inner: self.inner.clone(),
        }
    }

    /// Waits for the next value. Returns `None` when the sender is
    /// dropped and no value remains.
    ///
    /// If multiple values arrive between calls, intermediate ones are
    /// lost — only the latest is returned.
    pub async fn recv(&self) -> Option<T> {
        loop {
            if let Some(v) = self.take() {
                return Some(v);
            }
            if self.is_closed() {
                return None;
            }
            self.inner.notify.notified().await;
        }
    }
}

// Convenience Debug impls — don't expose the value.
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
        let tx2 = tx.clone();
        drop(tx);
        // Channel stays open because tx2 is still alive.
        assert!(!rx.is_closed());
        tx2.send(99);
        assert_eq!(rx.take(), Some(99));
        drop(tx2);
        assert!(rx.is_closed());
    }

    #[test]
    fn new_sender_writes_to_same_slot() {
        let (tx, rx) = frame_channel::<u32>();
        tx.send(1);
        assert_eq!(rx.take(), Some(1));

        // Create a new sender from the receiver (simulating a pipeline swap).
        let tx2 = rx.new_sender();
        drop(tx); // old sender gone
        assert!(!rx.is_closed(), "new_sender keeps channel open");

        tx2.send(42);
        assert_eq!(rx.take(), Some(42));
        assert_eq!(rx.produced(), 2);
    }

    #[tokio::test]
    async fn new_sender_wakes_recv() {
        let (tx, rx) = frame_channel::<u32>();
        let tx2 = rx.new_sender();
        drop(tx);

        let handle = tokio::spawn(async move { rx.recv().await });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        tx2.send(7);
        assert_eq!(handle.await.unwrap(), Some(7));
    }

    #[test]
    fn sender_swap_no_frame_loss() {
        // Simulates what adaptation does: old sender produces frames,
        // then a new sender takes over via new_sender(). The consumer
        // should see frames from both without gaps.
        let (tx_old, rx) = frame_channel::<u32>();
        tx_old.send(1);
        assert_eq!(rx.take(), Some(1));

        let tx_new = rx.new_sender();
        // Both senders alive briefly (overlap during switch).
        tx_new.send(2);
        drop(tx_old);
        assert_eq!(rx.take(), Some(2));
        assert!(!rx.is_closed());

        tx_new.send(3);
        assert_eq!(rx.take(), Some(3));
        drop(tx_new);
        assert!(rx.is_closed());
        assert_eq!(rx.produced(), 3);
    }
}
