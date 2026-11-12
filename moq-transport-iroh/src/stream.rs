//! The streams of a [`Transport`](crate::Transport), as moq-net's poll traits.

use std::{
    fmt,
    pin::{Pin, pin},
    task::Poll,
};

use bytes::Bytes;
use iroh::endpoint::{self, StoppedError, VarInt};
use kio::Waiter;
use moq_net::transport::poll::{self, Ungrouped};

use crate::{
    TransportError,
    transport::{OpSlot, poll_op},
};

/// The sending half of a stream.
pub struct SendStream {
    stream: endpoint::SendStream,
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
    /// Wraps `stream`, sending at `priority`.
    pub(crate) fn new(stream: endpoint::SendStream, priority: i64) -> Self {
        let mut stream = Self {
            stream,
            ended: false,
            closed: OpSlot::default(),
        };
        if priority != 0 {
            poll::SendStream::set_priority(&mut stream, priority);
        }
        stream
    }
}

impl fmt::Debug for SendStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendStream")
            .field("id", &self.stream.id())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl poll::SendStream for SendStream {
    type Error = TransportError;
    type Group = Ungrouped;

    fn poll_write(&mut self, waiter: &Waiter, buf: &[u8]) -> Poll<Result<usize, TransportError>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // iroh's write takes nothing from `buf` until it returns ready, so a
        // pending write leaves the stream as it was.
        Pin::new(&mut self.stream)
            .poll_write(&mut waiter.context(), buf)
            .map_err(Into::into)
    }

    fn set_priority(&mut self, priority: i64) {
        // Both order higher first. iroh's priority is 32 bits wide.
        let priority = priority.clamp(i32::MIN.into(), i32::MAX.into()) as i32;
        self.stream.set_priority(priority).ok();
    }

    fn set_group(&mut self, group: &Ungrouped) {
        match *group {}
    }

    fn finish(&mut self) -> Result<(), TransportError> {
        self.ended = true;
        self.stream.finish().map_err(Into::into)
    }

    fn reset(&mut self, code: u32) {
        self.ended = true;
        self.stream.reset(code.into()).ok();
    }

    fn poll_closed(&self, waiter: &Waiter) -> Poll<Result<(), TransportError>> {
        let stream = &self.stream;
        poll_op(&self.closed, waiter, || Box::pin(stream.stopped())).map(|stopped| match stopped {
            // Acknowledged after a finish, or freed after a reset.
            Ok(None) => Ok(()),
            Ok(Some(code)) => Err(TransportError::Stopped(code)),
            Err(err) => Err(err.into()),
        })
    }
}

impl Drop for SendStream {
    /// Resets a stream that was neither finished nor reset.
    ///
    /// iroh finishes a dropped stream, so a writer that went away would read
    /// as a complete stream to the peer. moq-net asks for a reset instead.
    fn drop(&mut self) {
        if !self.ended {
            self.stream.reset(VarInt::from_u32(0)).ok();
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
    use moq_net::transport::poll::{SendOptions, SendStream as _, Session as _};

    use crate::transport::tests::pair;

    /// A stream opens at its option's priority, and a priority past iroh's range saturates.
    #[tokio::test]
    async fn priorities_reach_iroh_and_saturate() {
        let (client, _server, _endpoints) = pair().await;
        let options = SendOptions {
            group: None,
            priority: 5,
        };
        let mut send = client.open_uni(options).await.expect("open");
        assert_eq!(send.stream.priority().expect("open"), 5);
        send.set_priority(i64::MAX);
        assert_eq!(send.stream.priority().expect("open"), i32::MAX);
        send.set_priority(i64::MIN);
        assert_eq!(send.stream.priority().expect("open"), i32::MIN);
    }
}
