//! The async-to-poll adapter moq-net needs, vendored from moq-tokio.
//!
//! moq-tokio's adapter (`moq_tokio::transport::Session`) only takes backends
//! that implement its sealed `Grouped` trait, so web-transport-iroh cannot use
//! it. This is a copy of `moq-tokio/src/transport.rs` and `transport/` at
//! moq-dev/moq@801fd9a2f (MIT OR Apache-2.0), minus its QUIC-backend impls and
//! tests, plus the impls for web-transport-iroh at the end.
//!
//! It goes away once web-transport-iroh implements moq-net's poll interface
//! itself, or moq-tokio opens `Grouped`.
#![allow(
    unreachable_pub,
    unnameable_types,
    missing_debug_implementations,
    reason = "vendored as is; only `Session` is reachable, through a doc-hidden re-export"
)]

use std::task::{Context, Poll, ready};

use bytes::Bytes;
use futures::FutureExt;
use web_transport_trait::poll as wt_poll;

mod grouped;
mod owned;
pub(crate) use grouped::{Grouped, GroupedSend};

/// A stored in-flight operation future. Native transports are Send, so the
/// plain boxed flavor suffices.
pub(crate) type OpBox<T> = futures::future::BoxFuture<'static, T>;

/// A stream op that hands the stream back with its result, or `None` if interrupted.
type StreamOp<S, T, E> = OpBox<(S, Option<Result<T, E>>)>;

/// Adapts this crate's async QUIC backend to the poll interface moq-net requires.
///
/// Only that backend fits: the bound goes through a sealed trait, so another
/// [`web_transport_trait::Session`] cannot be wrapped. A custom transport implements
/// moq-net's `transport::poll` traits instead.
///
/// Each pending session operation is stored as a boxed future built from a clone
/// of the underlying session; stream operations temporarily move the stream into
/// the future and take it back when the operation settles. Two costs follow: one
/// allocation per operation, and one copy per write (a poll write borrows its
/// buffer, an async write future cannot). Prefer a transport's native poll
/// implementation when it has one.
///
/// A few contract loosenings, all inherent to wrapping futures:
///
/// - A write must be retried with the same bytes until it resolves. The adapter
///   copies the buffer it was first given; a caller that swaps buffers while the
///   write is pending would corrupt the stream. Every caller in this crate
///   retries with the same buffer.
/// - `stop`, `reset`, and `finish` during an in-flight operation are deferred
///   until that operation settles. If the stream is dropped before then, the
///   pending operation is finished on the tokio runtime and the deferred code
///   applied after it, so the peer still sees the real reason; only without a
///   runtime does the underlying transport's drop behavior (a reset or stop
///   with code 0) stand in.
/// - Stream `poll_closed` watches are emulated rather than delegated: the
///   underlying async `closed()` future would own the stream for its whole
///   lifetime, deadlocking any later read or write (the async trait's
///   `closed(&mut self)` cannot be stored as a `'static` watch without taking
///   the stream with it). A receive stream reports closed through its reads
///   (FIN drained, or a read error), buffering bounded read-ahead so a FIN
///   behind undelivered payload still resolves the watch. At the cap it reads
///   no further: it watches the underlying `closed()` for a reset or
///   connection close, interruptibly so any read reclaims the stream, and
///   resumes reading ahead once the caller's reads drain below the cap.
///   Send-side watches are interruptible, so an idle watch can observe peer
///   cancellation while later writes reclaim the stream.
pub struct Session<S: web_transport_trait::Session> {
    session: S,
    address: moq_net::transport::Address,
    accept_uni: OpSlot<Result<S::RecvStream, S::Error>>,
    accept_bi: OpSlot<BiResult<S>>,
    open_uni: OpSlot<Result<S::SendStream, S::Error>>,
    open_bi: OpSlot<BiResult<S>>,
    request_bi: OpSlot<RequestResult<S>>,
    recv_datagram: OpSlot<Result<Bytes, S::Error>>,
    closed: OpSlot<S::Error>,
    // Wire counters for a backend that has none and reports bytes only in its stats.
    polled_wire: moq_net::transport::Wire,
}

/// The stream pair an async-interface accept or open resolves to.
type BiResult<S> = Result<
    (
        <S as web_transport_trait::Session>::SendStream,
        <S as web_transport_trait::Session>::RecvStream,
    ),
    <S as web_transport_trait::Session>::Error,
>;

/// A request's stream pair, or a refusal before consuming stream credit.
type RequestResult<S> = Result<
    Option<(
        <S as web_transport_trait::Session>::SendStream,
        <S as web_transport_trait::Session>::RecvStream,
    )>,
    <S as web_transport_trait::Session>::Error,
>;

/// A stored in-flight operation, `None` when idle, which every task polling it shares.
pub(crate) type OpSlot<T> = moq_net::kio::Bridge<Option<OpBox<T>>>;

/// Lazily start and poll the operation in `slot`, clearing it when it resolves.
///
/// Whichever caller polls it once it resolves takes the output; the rest start the next.
pub(crate) fn poll_op<T>(
    slot: &OpSlot<T>,
    waiter: &moq_net::kio::Waiter,
    start: impl FnOnce() -> OpBox<T>,
) -> Poll<T> {
    slot.poll(waiter, |slot, cx| {
        let fut = slot.get_or_insert_with(start);
        let output = ready!(fut.as_mut().poll(cx));
        *slot = None;
        Poll::Ready(output)
    })
}

impl<S: web_transport_trait::Session> Session<S> {
    /// Wrap an async-interface transport session.
    pub fn new(session: S, address: moq_net::transport::Address) -> Self {
        Self {
            session,
            address,
            accept_uni: OpSlot::default(),
            accept_bi: OpSlot::default(),
            open_uni: OpSlot::default(),
            open_bi: OpSlot::default(),
            request_bi: OpSlot::default(),
            recv_datagram: OpSlot::default(),
            closed: OpSlot::default(),
            polled_wire: moq_net::transport::Wire::new(),
        }
    }
}

// Manual impl: a clone starts with no in-flight operations, since each handle
// owns its own progress under the poll contract.
impl<S: web_transport_trait::Session> Clone for Session<S> {
    fn clone(&self) -> Self {
        Self {
            polled_wire: self.polled_wire.clone(),
            ..Self::new(self.session.clone(), self.address)
        }
    }
}

/// The send half produced by [`Session`]: an async-interface stream adapted to the
/// poll interface. See [`Session`] for the deferred `finish`/`reset` behavior.
pub struct SendStream<S: GroupedSend + 'static> {
    // Same never-locked Mutex as [`Session`]'s slots: `get_mut` is free on an
    // exclusive borrow, and the wrapper keeps the boxed future from stripping
    // `Sync` off the containing session types.
    state: std::sync::Mutex<Option<SendState<S>>>,
    // Deferred actions, applied when the in-flight operation settles.
    group: Option<S::Group>,
    priority: Option<i64>,
    finish: bool,
    reset: Option<u32>,
    /// Bytes already transmitted but not yet reported to the caller: a write the
    /// stored future finished (possibly inside
    /// [`poll_closed`](wt_poll::SendStream::poll_closed)) while the caller held a
    /// `Pending`. Later `poll_write` calls reconcile against the buffer actually
    /// presented (the poll contract allows a shorter retry), reporting and
    /// consuming a prefix per call, never writing these bytes a second time.
    completed: Bytes,
    /// Cancels the in-flight write so a reset applies immediately instead of
    /// waiting behind blocked I/O (whose partial progress a reset discards
    /// anyway). Fired by [`reset`](wt_poll::SendStream::reset) and by [`Drop`],
    /// and by [`set_priority`](wt_poll::SendStream::set_priority) during the
    /// closed() watch, which holds the stream but is safe to restart.
    interrupt: Option<futures::channel::oneshot::Sender<()>>,
}

enum SendState<S: GroupedSend + 'static> {
    Idle(S),
    /// A write in flight; `chunk` is the copied bytes, kept (refcounted, no
    /// extra copy) so a completion absorbed by `poll_closed` can verify the
    /// retrying caller supplied the same bytes. A `None` result means the write
    /// was interrupted by a reset (see [`SendStream::interrupt`]).
    Writing {
        fut: StreamOp<S, (), S::Error>,
        chunk: Bytes,
    },
    /// The closed() acknowledgement watch; a `None` result means it was
    /// interrupted by a late reset or priority change (see [`SendStream::interrupt`]).
    Closing(StreamOp<S, (), S::Error>),
}

impl<S: GroupedSend + 'static> SendStream<S> {
    /// Wrap an async-interface send stream.
    pub fn new(stream: S) -> Self {
        Self {
            state: std::sync::Mutex::new(Some(SendState::Idle(stream))),
            group: None,
            priority: None,
            finish: false,
            reset: None,
            completed: Bytes::new(),
            interrupt: None,
        }
    }

    /// Apply actions deferred while an operation was in flight.
    fn settle(&mut self, stream: &mut S) {
        if let Some(group) = self.group.take() {
            stream.set_group(&group);
        }
        if let Some(priority) = self.priority.take() {
            GroupedSend::set_priority(stream, priority);
        }
        if let Some(code) = self.reset.take() {
            self.finish = false;
            stream.reset(code);
        }
        if std::mem::take(&mut self.finish) {
            // A deferred finish has nowhere to report to; its failure surfaces
            // through closed() like any other terminal state.
            let _ = stream.finish();
        }
    }
}

impl<S: GroupedSend + 'static> SendStream<S> {
    /// Set the full-width priority, deferred while an operation holds the stream.
    fn prioritize(&mut self, priority: i64) {
        match self.state.get_mut().unwrap().as_mut() {
            Some(SendState::Idle(stream)) => GroupedSend::set_priority(stream, priority),
            // A finished stream still retransmits under its priority until the FIN is
            // acknowledged, so reclaim it from the watch rather than wait that out.
            Some(SendState::Closing(_)) => {
                self.priority = Some(priority);
                self.reclaim();
            }
            _ => self.priority = Some(priority),
        }
    }

    /// Schedule a newly opened stream by its options.
    fn schedule(&mut self, options: moq_net::transport::poll::SendOptions<'_, S::Group>) {
        self.prioritize(options.priority);
        if let Some(group) = options.group {
            self.join(group);
        }
    }

    /// Join `group`, deferred like [`prioritize`](Self::prioritize).
    fn join(&mut self, group: &S::Group) {
        match self.state.get_mut().unwrap().as_mut() {
            Some(SendState::Idle(stream)) => stream.set_group(group),
            Some(SendState::Closing(_)) => {
                self.group = Some(group.clone());
                self.reclaim();
            }
            _ => self.group = Some(group.clone()),
        }
    }

    /// Interrupt the closed() watch so a deferred action applies now.
    fn reclaim(&mut self) {
        if let Some(tx) = self.interrupt.take() {
            let _ = tx.send(());
        }
    }
}

impl<S: GroupedSend + 'static> wt_poll::SendStream for SendStream<S> {
    type Error = S::Error;

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
        loop {
            // Transmitted-but-unreported bytes from a completed write are
            // reconciled against the buffer presented NOW: the poll contract
            // allows a retry with a shorter buffer, so report (and consume) at
            // most its length and keep the rest for the next call.
            if !self.completed.is_empty() && !buf.is_empty() {
                let n = buf.len().min(self.completed.len());
                let reported = self.completed.split_to(n);
                // A hard assert, not a debug one: these bytes are already on the
                // wire and the bridge cannot un-send them, so a caller continuing
                // with different data would silently corrupt the stream. Failing
                // loudly is the only honest option left.
                assert_eq!(
                    &buf[..n],
                    &reported[..],
                    "poll_write must continue with the bytes the write bridge already transmitted"
                );
                return Poll::Ready(Ok(n));
            }
            match self.state.get_mut().unwrap().take().expect("in-flight") {
                SendState::Idle(mut stream) => {
                    if buf.is_empty() {
                        *self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
                        return Poll::Ready(Ok(0));
                    }
                    // write_chunk delivers the whole chunk, so the reported
                    // count is exact. The caller retries with the same bytes
                    // until this resolves (see [`Session`]).
                    let chunk = Bytes::copy_from_slice(buf);
                    let retained = chunk.clone();
                    let (tx, rx) = futures::channel::oneshot::channel::<()>();
                    self.interrupt = Some(tx);
                    let fut = async move {
                        let res = {
                            let mut op = std::pin::pin!(stream.write_chunk(chunk).fuse());
                            let mut rx = rx.fuse();
                            loop {
                                futures::select_biased! {
                                    res = op => break Some(res),
                                    cancel = rx => match cancel {
                                        Ok(()) => break None,
                                        // A dropped (unfired) sender must not interrupt;
                                        // only an explicit reset does. The fused arm goes
                                        // quiet and the write keeps driving.
                                        Err(_) => continue,
                                    },
                                }
                            }
                        };
                        (stream, res)
                    }
                    .boxed();
                    *self.state.get_mut().unwrap() = Some(SendState::Writing {
                        fut,
                        chunk: retained,
                    });
                }
                SendState::Writing { mut fut, chunk } => match fut.as_mut().poll(cx) {
                    Poll::Pending => {
                        *self.state.get_mut().unwrap() = Some(SendState::Writing { fut, chunk });
                        return Poll::Pending;
                    }
                    Poll::Ready((mut stream, res)) => {
                        self.interrupt = None;
                        self.settle(&mut stream);
                        *self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
                        // An interrupted write was abandoned for a reset, which
                        // settle just applied; the next iteration's write reports
                        // the stream state.
                        if let Some(res) = res {
                            res?;
                            // Reported through the reconciliation at the top of the
                            // loop, which caps it at the presented buffer's length.
                            self.completed = chunk;
                        }
                    }
                },
                SendState::Closing(mut fut) => {
                    if let Some(tx) = self.interrupt.take() {
                        let _ = tx.send(());
                    }
                    match fut.as_mut().poll(cx) {
                        Poll::Pending => {
                            *self.state.get_mut().unwrap() = Some(SendState::Closing(fut));
                            return Poll::Pending;
                        }
                        Poll::Ready((mut stream, _res)) => {
                            self.interrupt = None;
                            self.settle(&mut stream);
                            *self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
                        }
                    }
                }
            }
        }
    }

    fn set_priority(&mut self, order: i32) {
        self.prioritize(order.into());
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        match self.state.get_mut().unwrap().as_mut() {
            Some(SendState::Idle(stream)) => stream.finish(),
            _ => {
                self.finish = true;
                if matches!(self.state.get_mut().unwrap(), Some(SendState::Closing(_)))
                    && let Some(tx) = self.interrupt.take()
                {
                    let _ = tx.send(());
                }
                Ok(())
            }
        }
    }

    fn reset(&mut self, code: u32) {
        match self.state.get_mut().unwrap().as_mut() {
            Some(SendState::Idle(stream)) => stream.reset(code),
            _ => {
                self.reset = Some(code);
                // A reset discards the in-flight write's progress anyway, so
                // cancel it rather than letting blocked I/O delay the code.
                if let Some(tx) = self.interrupt.take() {
                    let _ = tx.send(());
                }
            }
        }
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        loop {
            match self.state.get_mut().unwrap().take().expect("in-flight") {
                SendState::Idle(mut stream) => {
                    self.settle(&mut stream);
                    // Interruptible like a write: a late reset (the group machines
                    // cancel a finished stream this way) must not wait for a FIN
                    // acknowledgement that may never come.
                    let (tx, rx) = futures::channel::oneshot::channel::<()>();
                    self.interrupt = Some(tx);
                    let fut = async move {
                        let res = {
                            let mut op = std::pin::pin!(stream.closed().fuse());
                            let mut rx = rx.fuse();
                            loop {
                                futures::select_biased! {
                                    res = op => break Some(res),
                                    cancel = rx => match cancel {
                                        Ok(()) => break None,
                                        Err(_) => continue,
                                    },
                                }
                            }
                        };
                        (stream, res)
                    }
                    .boxed();
                    *self.state.get_mut().unwrap() = Some(SendState::Closing(fut));
                }
                SendState::Writing { mut fut, chunk } => match fut.as_mut().poll(cx) {
                    Poll::Pending => {
                        *self.state.get_mut().unwrap() = Some(SendState::Writing { fut, chunk });
                        return Poll::Pending;
                    }
                    Poll::Ready((mut stream, res)) => {
                        self.interrupt = None;
                        self.settle(&mut stream);
                        *self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
                        match res {
                            // A failed write is a closed stream; report it here.
                            Some(Err(err)) => return Poll::Ready(Err(err)),
                            // The caller of poll_write was told Pending and will
                            // retry; poll_write's reconciliation reports these bytes
                            // instead of writing them a second time.
                            Some(Ok(())) => self.completed = chunk,
                            // Interrupted for a reset, applied by settle above; the
                            // next iteration starts the real closed watch.
                            None => {}
                        }
                    }
                },
                SendState::Closing(mut fut) => match fut.as_mut().poll(cx) {
                    Poll::Pending => {
                        *self.state.get_mut().unwrap() = Some(SendState::Closing(fut));
                        return Poll::Pending;
                    }
                    Poll::Ready((mut stream, res)) => {
                        self.interrupt = None;
                        self.settle(&mut stream);
                        *self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
                        // An interrupted watch was abandoned for a late reset or
                        // priority change, which settle just applied; the next
                        // iteration watches again.
                        if let Some(res) = res {
                            return Poll::Ready(res);
                        }
                    }
                },
            }
        }
    }
}

impl<S: GroupedSend + 'static> Drop for SendStream<S> {
    fn drop(&mut self) {
        match self.state.get_mut().unwrap().take() {
            // Apply a deferred reset if the stream is idle.
            Some(SendState::Idle(mut stream)) => {
                if let Some(code) = self.reset.take() {
                    stream.reset(code);
                }
            }
            // The stream lives inside the in-flight future; dropping it here
            // would fire the backend's default drop behavior (a code-0 reset)
            // and lose the deferred reset code or FIN the caller asked for. The
            // peer uses that code to classify the abort (Old vs Cancel vs
            // Evicted), so finish the operation on the runtime and apply the
            // terminal action then. A deferred reset fired the interrupt when it
            // was recorded, so the salvage task never waits on blocked I/O; a
            // deferred finish lets the write complete first (the FIN follows the
            // data).
            Some(SendState::Writing { fut, .. }) => {
                let reset = self.reset.take();
                let finish = std::mem::take(&mut self.finish);
                if reset.is_some() || finish {
                    detach(async move {
                        let (mut stream, _) = fut.await;
                        match reset {
                            Some(code) => stream.reset(code),
                            None => {
                                let _ = stream.finish();
                            }
                        }
                    });
                }
            }
            // Both deferred actions fired the watch's interrupt when recorded, so
            // the salvage task reclaims the stream without waiting on the peer.
            Some(SendState::Closing(fut)) => {
                let reset = self.reset.take();
                let finish = std::mem::take(&mut self.finish);
                if reset.is_some() || finish {
                    detach(async move {
                        let (mut stream, _) = fut.await;
                        match reset {
                            Some(code) => stream.reset(code),
                            None => {
                                let _ = stream.finish();
                            }
                        }
                    });
                }
            }
            None => {}
        }
    }
}

/// The receive half produced by [`Session`]: an async-interface stream adapted to
/// the poll interface. See [`Session`] for the deferred `stop` behavior.
pub struct RecvStream<S: web_transport_trait::RecvStream + 'static> {
    // See [`SendStream::state`] for why this Mutex exists; it is never locked.
    state: std::sync::Mutex<Option<RecvState<S>>>,
    /// Bytes already read from the transport but not yet handed to the caller.
    buffer: Bytes,
    /// Read-ahead pulled in by the closed-watch while payload sat undelivered, so
    /// a FIN right behind buffered data still resolves the watch. Drained by the
    /// read methods before the transport is polled again; bounded by
    /// [`READ_AHEAD_CAP`].
    queued: std::collections::VecDeque<Bytes>,
    /// Total bytes across `queued`, so the cap check is O(1).
    queued_len: usize,
    /// The peer finished the stream.
    fin: bool,
    /// A deferred STOP_SENDING, applied when the in-flight operation settles.
    stop: Option<u32>,
    /// Cancels the in-flight read so a stop applies immediately instead of
    /// waiting behind blocked I/O. Fired by [`stop`](wt_poll::RecvStream::stop)
    /// and by [`Drop`], and by a read that needs the stream back from the
    /// closed-watch.
    interrupt: Option<futures::channel::oneshot::Sender<()>>,
    /// The in-flight operation is the closed-watch's `closed()` at the cap,
    /// which any read interrupts rather than waits on.
    watching: bool,
    /// The closed-watch parked at the cap, woken once reads drain below it.
    parked: Option<std::task::Waker>,
}

/// How much the closed-watch may read ahead of the caller before it parks and
/// waits for the caller to drain. Bounds memory against a peer that floods a
/// stream nobody is reading.
const READ_AHEAD_CAP: usize = 64 * 1024;

enum RecvState<S: web_transport_trait::RecvStream + 'static> {
    Idle(S),
    /// A read in flight, or the closed-watch's `closed()` reporting its end
    /// as `Ok(None)`; a `None` result means it was interrupted (see
    /// [`RecvStream::interrupt`]).
    Reading(StreamOp<S, Option<Bytes>, S::Error>),
}

impl<S: web_transport_trait::RecvStream + 'static> RecvStream<S> {
    /// Wrap an async-interface receive stream.
    pub fn new(stream: S) -> Self {
        Self {
            state: std::sync::Mutex::new(Some(RecvState::Idle(stream))),
            buffer: Bytes::new(),
            queued: std::collections::VecDeque::new(),
            queued_len: 0,
            fin: false,
            stop: None,
            interrupt: None,
            watching: false,
            parked: None,
        }
    }

    /// Pop read-ahead into `self.buffer` if the head is empty.
    fn unqueue(&mut self) {
        if self.buffer.is_empty()
            && let Some(next) = self.queued.pop_front()
        {
            self.queued_len -= next.len();
            self.buffer = next;
        }
    }

    /// Wake a closed-watch parked at the cap once reads drain below it.
    fn drained(&mut self) {
        if self.buffer.len() + self.queued_len < READ_AHEAD_CAP
            && let Some(parked) = self.parked.take()
        {
            parked.wake();
        }
    }

    /// Start an interruptible operation on the idle `stream`: a read of up to
    /// `max` bytes, or with `None` the closed-watch's `closed()`, whose end
    /// resolves as `Ok(None)`.
    fn start(&mut self, mut stream: S, max: Option<usize>) {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        self.interrupt = Some(tx);
        self.watching = max.is_none();
        let fut = async move {
            let res = {
                let op = async {
                    match max {
                        Some(max) => stream.read_chunk(max).await,
                        None => stream.closed().await.map(|()| None),
                    }
                };
                let mut op = std::pin::pin!(op.fuse());
                let mut rx = rx.fuse();
                loop {
                    futures::select_biased! {
                        res = op => break Some(res),
                        cancel = rx => match cancel {
                            Ok(()) => break None,
                            // A dropped (unfired) sender must not interrupt;
                            // only an explicit stop or a read does. The fused
                            // arm goes quiet and the operation keeps driving.
                            Err(_) => continue,
                        },
                    }
                }
            };
            (stream, res)
        }
        .boxed();
        *self.state.get_mut().unwrap() = Some(RecvState::Reading(fut));
    }

    /// Read the next chunk from the transport, setting `self.fin` on the FIN.
    fn poll_fill(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, S::Error>> {
        loop {
            match self.state.get_mut().unwrap().take().expect("in-flight") {
                RecvState::Idle(mut stream) => {
                    if let Some(code) = self.stop.take() {
                        stream.stop(code);
                    }
                    self.start(stream, Some(max));
                }
                RecvState::Reading(fut) => {
                    // A read wants the stream back from the closed-watch, which
                    // would otherwise hold it until the stream ends.
                    let watching = self.watching;
                    if watching && let Some(tx) = self.interrupt.take() {
                        let _ = tx.send(());
                    }
                    match ready!(self.poll_op(cx, fut)) {
                        // Interrupted, or the watch saw the stream end: the
                        // read that follows reports how.
                        None => continue,
                        Some(Ok(_)) if watching => continue,
                        Some(res) => return Poll::Ready(res),
                    }
                }
            }
        }
    }

    /// Poll the in-flight operation, settling it once it resolves; `None` means
    /// it was interrupted, and the next operation reports the stream state.
    fn poll_op(
        &mut self,
        cx: &mut Context<'_>,
        mut fut: StreamOp<S, Option<Bytes>, S::Error>,
    ) -> Poll<Option<Result<Option<Bytes>, S::Error>>> {
        let Poll::Ready((mut stream, res)) = fut.as_mut().poll(cx) else {
            *self.state.get_mut().unwrap() = Some(RecvState::Reading(fut));
            return Poll::Pending;
        };
        self.interrupt = None;
        let watched = std::mem::take(&mut self.watching);
        // Apply a stop requested while the operation was in flight now, not on
        // the next read: a caller that stops and never reads again must still
        // send STOP_SENDING.
        if let Some(code) = self.stop.take() {
            stream.stop(code);
        }
        *self.state.get_mut().unwrap() = Some(RecvState::Idle(stream));
        // Only a read's end is the FIN: closed() also ends at a reset.
        if !watched && let Some(Ok(None)) = &res {
            self.fin = true;
        }
        Poll::Ready(res)
    }
}

impl<S: web_transport_trait::RecvStream + 'static> wt_poll::RecvStream for RecvStream<S> {
    type Error = S::Error;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Self::Error>> {
        if dst.is_empty() {
            return Poll::Ready(Ok(Some(0)));
        }

        loop {
            self.unqueue();
            if !self.buffer.is_empty() {
                let n = dst.len().min(self.buffer.len());
                dst[..n].copy_from_slice(&self.buffer.split_to(n));
                self.drained();
                return Poll::Ready(Ok(Some(n)));
            }
            if self.fin {
                return Poll::Ready(Ok(None));
            }
            if let Some(bytes) = ready!(self.poll_fill(cx, dst.len()))? {
                self.buffer = bytes;
            }
        }
    }

    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, Self::Error>> {
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }

        loop {
            self.unqueue();
            if !self.buffer.is_empty() {
                let n = max.min(self.buffer.len());
                let chunk = self.buffer.split_to(n);
                self.drained();
                return Poll::Ready(Ok(Some(chunk)));
            }
            if self.fin {
                return Poll::Ready(Ok(None));
            }
            if let Some(bytes) = ready!(self.poll_fill(cx, max))? {
                self.buffer = bytes;
            }
        }
    }

    fn stop(&mut self, code: u32) {
        match self.state.get_mut().unwrap().as_mut() {
            Some(RecvState::Idle(stream)) => stream.stop(code),
            _ => {
                self.stop = Some(code);
                // The caller is discarding the stream, so cancel the in-flight
                // read rather than letting blocked I/O delay the stop.
                if let Some(tx) = self.interrupt.take() {
                    let _ = tx.send(());
                }
            }
        }
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Emulated with reads rather than held on the underlying closed(): that
        // future owns the stream, so it is only watched at the read-ahead cap, and
        // interruptibly, so any read reclaims the stream. Below the cap a FIN or
        // reset surfaces through reads; payload arriving before it is queued as
        // read-ahead (up to READ_AHEAD_CAP) so a FIN right behind undelivered data
        // still resolves the watch without an external read.
        loop {
            if self.fin {
                return Poll::Ready(Ok(()));
            }
            if self.buffer.len() + self.queued_len >= READ_AHEAD_CAP {
                // Read no further, but still see a reset or connection close
                // behind the backlog, and resume reading ahead once the caller's
                // reads drain below the cap.
                self.parked = Some(cx.waker().clone());
                match self.state.get_mut().unwrap().take().expect("in-flight") {
                    RecvState::Idle(stream) => {
                        self.start(stream, None);
                        continue;
                    }
                    RecvState::Reading(fut) if self.watching => {
                        match ready!(self.poll_op(cx, fut)) {
                            Some(Err(err)) => return Poll::Ready(Err(err)),
                            // A reset ends closed() as well as the FIN, and after
                            // either no payload is left to wait on, so one read
                            // says which without growing the backlog.
                            Some(Ok(_)) => {
                                let Some(RecvState::Idle(stream)) =
                                    self.state.get_mut().unwrap().take()
                                else {
                                    unreachable!("a settled watch leaves the stream idle");
                                };
                                self.start(stream, Some(8 * 1024));
                            }
                            // Interrupted by a stop; watch again.
                            None => {}
                        }
                        continue;
                    }
                    // A read in flight, ours or the caller's: settle it below.
                    reading => *self.state.get_mut().unwrap() = Some(reading),
                }
            }
            match ready!(self.poll_fill(cx, 8 * 1024)) {
                Ok(Some(bytes)) => {
                    self.queued_len += bytes.len();
                    self.queued.push_back(bytes);
                }
                Ok(None) => {}
                // A read error is also a closed stream.
                Err(err) => return Poll::Ready(Err(err)),
            }
        }
    }
}

impl<S: web_transport_trait::RecvStream + 'static> Drop for RecvStream<S> {
    fn drop(&mut self) {
        match self.state.get_mut().unwrap().take() {
            // Apply a deferred stop if the stream is idle.
            Some(RecvState::Idle(mut stream)) => {
                if let Some(code) = self.stop.take() {
                    stream.stop(code);
                }
            }
            // The stream lives inside the in-flight read; dropping it here would
            // fire the backend's default drop behavior (a code-0 stop) and lose
            // the deferred code the peer classifies the abort by. Finish the
            // read on the runtime and stop with the real code then; a deferred
            // stop fired the interrupt when it was recorded, so the task never
            // waits on blocked I/O.
            Some(RecvState::Reading(fut)) => {
                if let Some(code) = self.stop.take() {
                    detach(async move {
                        let (mut stream, _) = fut.await;
                        stream.stop(code);
                    });
                }
            }
            None => {}
        }
    }
}

/// Runs a cleanup future on its own task, like moq-tokio's `runtime::detach`.
fn detach<F: std::future::Future + Send + 'static>(future: F)
where
    F::Output: Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        drop(handle.spawn(future));
    }
}

impl Grouped for web_transport_iroh::Session {
    type Recv = moq_net::transport::poll::Buffered<RecvStream<web_transport_iroh::RecvStream>>;

    fn recv(stream: RecvStream<web_transport_iroh::RecvStream>) -> Self::Recv {
        moq_net::transport::poll::Buffered::new(stream)
    }
}

impl GroupedSend for web_transport_iroh::SendStream {
    type Group = moq_net::transport::poll::Ungrouped;

    fn set_group(&mut self, _group: &Self::Group) {}
}
