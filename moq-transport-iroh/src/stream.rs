//! The streams of a [`Transport`](crate::Transport), as moq-net's poll traits.

use std::{
    fmt,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    task::Poll,
};

use bytes::Bytes;
use iroh::endpoint::{self, StoppedError, VarInt};
use kio::Waiter;
use moq_net::transport::poll::{self, SendOptions};

use crate::{
    SendGroup, TransportError,
    group::{self, Membership, SharedStream},
    transport::{OpSlot, poll_op},
};

/// The sending half of a stream.
pub struct SendStream {
    /// Shared with the stream's send group, which sets its urgency.
    stream: SharedStream,
    /// The priority moq-net gave the stream: its order inside its group, or
    /// its rank among the ungrouped streams.
    priority: i64,
    /// The stream's send group, or `None` while it is ungrouped.
    membership: Option<Membership>,
    /// Whether the stream has ended.
    ///
    /// Set by [`finish`](poll::SendStream::finish) and
    /// [`reset`](poll::SendStream::reset).
    ended: bool,
    /// The peer's acknowledgement of the FIN, or its STOP_SENDING.
    ///
    /// iroh's `stopped` future owns what it watches, so it is stored here and
    /// polled through a shared borrow while writes go on.
    closed: OpSlot<Result<Option<VarInt>, StoppedError>>,
}

impl SendStream {
    /// Wraps `stream`, scheduled by `options`.
    pub(crate) fn new(stream: endpoint::SendStream, options: SendOptions<'_, SendGroup>) -> Self {
        let stream = Arc::new(Mutex::new(stream));
        let membership = options
            .group
            .map(|group| group.join(stream.clone(), options.priority));
        let stream = Self {
            stream,
            priority: options.priority,
            membership,
            ended: false,
            closed: OpSlot::default(),
        };
        if stream.membership.is_none() {
            stream.apply_ungrouped();
        }
        stream
    }

    /// Sets the urgency of a stream outside every group.
    fn apply_ungrouped(&self) {
        group::lock(&self.stream)
            .set_priority(group::ungrouped(self.priority))
            .ok();
    }
}

impl fmt::Debug for SendStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendStream")
            .field("id", &group::lock(&self.stream).id())
            .field("priority", &self.priority)
            .field("grouped", &self.membership.is_some())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl poll::SendStream for SendStream {
    type Error = TransportError;
    type Group = SendGroup;

    fn poll_write(&mut self, waiter: &Waiter, buf: &[u8]) -> Poll<Result<usize, TransportError>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // iroh's write takes nothing from `buf` until it returns ready, so a
        // pending write leaves the stream as it was.
        let mut stream = group::lock(&self.stream);
        Pin::new(&mut *stream)
            .poll_write(&mut waiter.context(), buf)
            .map_err(Into::into)
    }

    fn set_priority(&mut self, priority: i64) {
        self.priority = priority;
        match &self.membership {
            Some(membership) => membership.set_order(priority),
            None => self.apply_ungrouped(),
        }
    }

    fn set_group(&mut self, group: &SendGroup) {
        if self
            .membership
            .as_ref()
            .is_some_and(|membership| membership.group().same(group))
        {
            return;
        }
        // Leaves the old group first, which reranks the streams it leaves.
        self.membership = None;
        self.membership = Some(group.join(self.stream.clone(), self.priority));
    }

    fn finish(&mut self) -> Result<(), TransportError> {
        self.ended = true;
        group::lock(&self.stream).finish().map_err(Into::into)
    }

    fn reset(&mut self, code: u32) {
        self.ended = true;
        group::lock(&self.stream).reset(code.into()).ok();
    }

    fn poll_closed(&self, waiter: &Waiter) -> Poll<Result<(), TransportError>> {
        poll_op(&self.closed, waiter, || {
            Box::pin(group::lock(&self.stream).stopped())
        })
        .map(|stopped| match stopped {
            // Acknowledged after a finish, or freed after a reset.
            Ok(None) => Ok(()),
            Ok(Some(code)) => Err(TransportError::Stopped(code)),
            Err(err) => Err(err.into()),
        })
    }
}

impl Drop for SendStream {
    /// Leaves the send group, then resets a stream that was neither finished
    /// nor reset.
    ///
    /// iroh finishes a dropped stream, so a writer that went away would read
    /// as a complete stream to the peer. moq-net asks for a reset instead.
    fn drop(&mut self) {
        self.membership = None;
        if !self.ended {
            group::lock(&self.stream).reset(VarInt::from_u32(0)).ok();
        }
    }
}

/// The receiving half of a stream.
///
/// moq-net reads it through [`poll::Buffered`], since iroh returns flow
/// control credit as it reads. Dropping it stops the stream with code 0, as
/// iroh does.
///
/// It keeps the default [`poll_reset`](poll::Chunks::poll_reset), which never
/// completes. iroh's `received_reset` leaves its waker registered with the
/// stream, which a later read to the end then trips over in iroh's drop check.
pub struct RecvStream {
    stream: endpoint::RecvStream,
}

impl RecvStream {
    /// Wraps `stream` for moq-net.
    pub(crate) fn buffered(stream: endpoint::RecvStream) -> poll::Buffered<Self> {
        poll::Buffered::new(Self { stream })
    }
}

impl fmt::Debug for RecvStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvStream")
            .field("id", &self.stream.id())
            .finish_non_exhaustive()
    }
}

impl poll::Chunks for RecvStream {
    type Error = TransportError;

    fn poll_read_chunk(
        &mut self,
        waiter: &Waiter,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, TransportError>> {
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }
        // A read future that is dropped while pending reads nothing, and the
        // waker it registered stays with the stream, so a fresh one per poll
        // is enough.
        let read = pin!(self.stream.read_chunk(max));
        read.poll(&mut waiter.context()).map_err(Into::into)
    }

    fn stop(&mut self, code: u32) {
        self.stream.stop(code.into()).ok();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bytes::Bytes;
    use iroh::endpoint;
    use moq_net::transport::poll::{
        RecvStream as _, SendGroup as _, SendOptions, SendStream as _, Session as _,
    };

    use super::SendStream;
    use crate::{
        group,
        transport::tests::{TIMEOUT, pair, pair_configured},
    };

    /// Returns the urgency iroh schedules `send` at.
    fn urgency(send: &SendStream) -> i32 {
        group::lock(&send.stream).priority().expect("open")
    }

    /// An ungrouped stream opens at its option's priority, above every group,
    /// and a priority past the ungrouped range saturates.
    #[tokio::test]
    async fn ungrouped_priorities_reach_iroh_and_saturate() {
        let (client, _server, _endpoints) = pair().await;
        let options = SendOptions {
            group: None,
            priority: 5,
        };
        let mut send = client.open_uni(options).await.expect("open");
        assert_eq!(urgency(&send), group::ungrouped(5));
        let at_zero = client.open_uni(SendOptions::default()).await.expect("open");
        assert!(urgency(&send) > urgency(&at_zero));
        send.set_priority(i64::MAX);
        assert_eq!(urgency(&send), group::ungrouped(i64::MAX));
        send.set_priority(i64::MIN);
        assert_eq!(urgency(&send), group::ungrouped(i64::MIN));

        let top = client.send_group(i64::MAX).expect("a send group");
        let grouped = client
            .open_uni(SendOptions {
                group: Some(&top),
                priority: i64::MAX,
            })
            .await
            .expect("open");
        assert!(urgency(&send) > urgency(&grouped), "ungrouped goes first");
    }

    /// The heads of equal-priority groups tie, a group's older streams rank
    /// below every head, and a higher group outranks them all.
    #[tokio::test]
    async fn groups_rank_their_streams_in_tiers() {
        let (client, _server, _endpoints) = pair().await;
        let mut first = client.send_group(7).expect("a send group");
        let second = client.send_group(7).expect("a send group");
        let open = |group, priority| {
            client.open_uni(SendOptions {
                group: Some(group),
                priority,
            })
        };
        // Orders as moq-net gives them: group sequences that differ between
        // the subscriptions.
        let newest = open(&first, 100).await.expect("open");
        let older = open(&first, 98).await.expect("open");
        let mut other = open(&second, 4).await.expect("open");
        assert_eq!(urgency(&newest), urgency(&other), "heads share");
        assert!(
            urgency(&older) < urgency(&other),
            "a backlog waits for every head"
        );

        // A newer stream in the second group demotes its head.
        let newer = open(&second, 6).await.expect("open");
        assert_eq!(urgency(&newer), urgency(&newest));
        assert_eq!(urgency(&other), urgency(&older));

        // Leaving promotes the stream behind.
        drop(newest);
        assert_eq!(urgency(&older), urgency(&newer));

        // A stream that moves past its group's head becomes the head.
        other.set_priority(8);
        assert!(urgency(&other) > urgency(&newer));
        assert_eq!(urgency(&other), urgency(&older));

        // Raising a group lifts all of its streams above the other's.
        first.set_priority(8);
        assert!(urgency(&older) > urgency(&other));

        // Moving a stream to another group reranks both.
        let mut moved = newer;
        moved.set_group(&first);
        assert!(urgency(&moved) > urgency(&other));
        assert!(
            urgency(&older) > urgency(&moved),
            "it ranks behind the head"
        );
    }

    /// Two equal-priority groups share the link, whatever their streams' orders.
    ///
    /// The first group's stream has the higher order, as a newer group sequence
    /// would. Ranked by order alone it would drain before the second sent a
    /// byte, so the second must be well along when the first completes.
    #[tokio::test]
    async fn equal_priority_groups_progress_together() {
        let streams = progress_of_the_lagging_stream(true).await;
        assert!(
            streams >= LEN / 4,
            "the other stream had {streams} of {LEN} bytes when the first finished",
        );
    }

    /// The same two streams outside any group, ranked by order alone, do not
    /// share: the lower one waits for the higher. This keeps the test above
    /// honest, since a loopback link with small windows would let any two
    /// streams interleave.
    #[tokio::test]
    async fn ungrouped_streams_drain_in_order() {
        let streams = progress_of_the_lagging_stream(false).await;
        assert!(
            streams < LEN / 4,
            "the other stream had {streams} of {LEN} bytes when the first finished",
        );
    }

    /// The bytes each stream sends after its tag.
    const LEN: usize = 8 << 20;

    /// Sends [`LEN`] bytes on each of two streams at orders 1000 and 0, in two
    /// equal-priority groups if `grouped`, and returns how much of one stream
    /// had arrived when the other finished.
    ///
    /// The flow control windows exceed what the streams send, so only the
    /// sender's scheduler decides which goes first.
    async fn progress_of_the_lagging_stream(grouped: bool) -> usize {
        let window = endpoint::VarInt::from_u32(64 << 20);
        let config = endpoint::QuicTransportConfig::builder()
            .stream_receive_window(window)
            .receive_window(window)
            .send_window(64 << 20)
            .build();
        let (client, server, _endpoints) = pair_configured(config).await;
        let groups = [7, 7].map(|priority| client.send_group(priority).expect("a send group"));
        let mut sends = Vec::new();
        for (tag, (group, order)) in groups.iter().zip([1000, 0]).enumerate() {
            let options = SendOptions {
                group: grouped.then_some(group),
                priority: order,
            };
            let mut send = client.open_uni(options).await.expect("open");
            send.write_chunk(Bytes::from(vec![tag as u8]))
                .await
                .expect("write the tag");
            sends.push(send);
        }
        let write = |mut send: SendStream| async move {
            send.write_chunk(Bytes::from(vec![0; LEN]))
                .await
                .expect("write");
            send.finish().expect("finish");
            send
        };
        let received = [AtomicUsize::new(0), AtomicUsize::new(0)];
        // What the other stream had received when each finished.
        let other_at_finish = [AtomicUsize::new(0), AtomicUsize::new(0)];
        let read = || async {
            let mut recv = server.accept_uni().await.expect("accept");
            let tag = recv.read_chunk(1).await.expect("read").expect("a tag")[0] as usize;
            while let Some(chunk) = recv.read_chunk(usize::MAX).await.expect("read") {
                received[tag].fetch_add(chunk.len(), Ordering::Relaxed);
            }
            other_at_finish[tag]
                .store(received[1 - tag].load(Ordering::Relaxed), Ordering::Relaxed);
        };
        let [first, second]: [SendStream; 2] = sends.try_into().expect("two streams");
        tokio::time::timeout(TIMEOUT, async {
            tokio::join!(write(first), write(second), read(), read())
        })
        .await
        .expect("both streams arrive in time");

        // The stream that finished second saw all of the first, so the
        // smaller figure is the first one's view of the second.
        other_at_finish
            .iter()
            .map(|other| other.load(Ordering::Relaxed))
            .min()
            .expect("two streams")
    }
}
