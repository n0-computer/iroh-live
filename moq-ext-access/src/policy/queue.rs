//! A policy that hands each decision to the application.

use std::fmt;

use super::{Authorization, Decision, Event, PeerRequest, Policy, Rejection};
use crate::PeerId;

/// A policy that hands each decision to the application, through a bounded queue.
///
/// The application takes the requests with [`next`](Self::next) and answers
/// each [`Pending`] with [`admit`](Pending::admit) or
/// [`reject`](Pending::reject). Clones share one queue, so the node can hold
/// the policy while the application holds a clone to answer from.
///
/// A request that finds the queue full is rejected at once with
/// [`moq_net::Error::TooManyRequests`], so a slow application does not pile up
/// sessions whose peers wait. A node also gives up on a decision after its
/// decide timeout, such as `NodeConfig::decide_timeout` in iroh-live-node,
/// which is 10 seconds there by default. Raise it when a person answers.
///
/// The application sees [`Event::Connect`] and [`Event::Reauthorize`]. The
/// queue answers [`Event::Revalidate`] itself with the authorization the
/// peer holds, since a periodic check has nothing new to ask.
///
/// # Examples
///
/// Admit the peers that present a known token:
///
/// ```
/// use moq_ext_access::{
///     Grant,
///     policy::{Queue, Rejection},
/// };
///
/// # async fn run() {
/// let queue = Queue::<String>::new(16);
/// // Give `queue.clone()` to the node as its policy, then answer:
/// while let Some(pending) = queue.next().await {
///     if pending.request().token() == Some("secret") {
///         pending.admit(Grant::subscribe_only());
///     } else {
///         pending.reject(Rejection::new("unknown token"));
///     }
/// }
/// # }
/// ```
#[derive(Clone)]
pub struct Queue<P: PeerId> {
    sender: async_channel::Sender<Pending<P>>,
    receiver: async_channel::Receiver<Pending<P>>,
}

impl<P: PeerId> fmt::Debug for Queue<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queue")
            .field("waiting", &self.receiver.len())
            .field("closed", &self.receiver.is_closed())
            .finish()
    }
}

impl<P: PeerId> Queue<P> {
    /// Creates a queue that holds up to `capacity` requests.
    ///
    /// A request that finds the queue full is rejected with
    /// [`moq_net::Error::TooManyRequests`]. A node treats that as no answer
    /// when it asks again about a live peer: the peer keeps its
    /// authorization, and the node asks again later.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = async_channel::bounded(capacity);
        Self { sender, receiver }
    }

    /// Waits for the next request to answer.
    ///
    /// Returns `None` once the queue is closed and empty. Cancellation safe:
    /// dropping the future loses no request.
    pub async fn next(&self) -> Option<Pending<P>> {
        self.receiver.recv().await.ok()
    }

    /// Closes the queue for every clone.
    ///
    /// Requests that arrive later are rejected. The ones already queued stay
    /// for [`next`](Self::next), which returns `None` after them.
    pub fn close(&self) {
        self.sender.close();
    }
}

impl<P: PeerId> Policy<P> for Queue<P> {
    async fn decide(&self, request: &PeerRequest<P>) -> Decision {
        if request.event() == Event::Revalidate
            && let Some(current) = request.current()
        {
            return Ok(current.clone());
        }
        let (reply, answer) = async_channel::bounded(1);
        let pending = Pending {
            request: request.clone(),
            reply,
        };
        if let Err(err) = self.sender.try_send(pending) {
            return Err(match err {
                async_channel::TrySendError::Full(_) => {
                    Rejection::new("the admission queue is full")
                        .with_code(moq_net::Error::TooManyRequests)
                }
                // A closed queue admits nobody any more.
                async_channel::TrySendError::Closed(_) => {
                    Rejection::new("the admission queue is closed")
                }
            });
        }
        // A dropped `Pending` drops its sender, which ends the receive.
        answer
            .recv()
            .await
            .unwrap_or_else(|_| Err(Rejection::new("not answered")))
    }
}

/// A request waiting in a [`Queue`] for the application's answer.
///
/// Dropping it without an answer rejects the request.
pub struct Pending<P: PeerId> {
    request: PeerRequest<P>,
    reply: async_channel::Sender<Decision>,
}

impl<P: PeerId> fmt::Debug for Pending<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl<P: PeerId> Pending<P> {
    /// Returns the request to answer.
    #[must_use]
    pub fn request(&self) -> &PeerRequest<P> {
        &self.request
    }

    /// Admits the peer with `authorization`, such as a [`Grant`](crate::Grant).
    ///
    /// Does nothing if the node no longer waits, for example after its
    /// decide timeout.
    pub fn admit(self, authorization: impl Into<Authorization>) {
        self.answer(Ok(authorization.into()));
    }

    /// Rejects the peer, which closes its session with the rejection's code.
    pub fn reject(self, rejection: Rejection) {
        self.answer(Err(rejection));
    }

    fn answer(self, decision: Decision) {
        // The channel holds one answer, so it is never full. A closed one
        // means nobody waits anymore.
        self.reply.try_send(decision).ok();
    }
}
