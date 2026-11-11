//! Admission and authorization of sessions, decided by a [`Policy`].
//!
//! A node asks its policy when a session opens, in either direction, and
//! again later (see [`Event`]). The answer is a [`Decision`]. The building
//! blocks compose:
//!
//! - [`Open`] and [`Closed`] admit or reject everyone.
//! - [`from_fn`] decides with a synchronous closure.
//! - [`Policy::union`] and [`Policy::filter`] combine policies.
//! - [`Queue`] hands each decision to the application.
//! - [`Verify`] decides with a [`TokenVerifier`] when the peer presents a
//!   token, and with a fallback policy when it does not.
//!
//! # Examples
//!
//! Admit everyone to subscribe, except one peer:
//!
//! ```
//! use moq_ext_access::{
//!     Grant,
//!     policy::{self, Policy},
//! };
//!
//! let policy = policy::Open(Grant::subscribe_only())
//!     .filter(|request: &policy::PeerRequest<String>| request.peer() != "mallory");
//! # drop(policy);
//! ```

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use moq_net::Patterns;
use n0_future::{MergeBounded, boxed::BoxStream, future};

#[cfg(feature = "jwt")]
pub use self::jwt::JwtVerifier;
#[cfg(feature = "rcan")]
pub use self::rcan::{MAX_LINK_BYTES, MAX_LINKS, MoqCapability, RcanVerifier, encode_chain};
pub use self::{
    queue::{Pending, Queue},
    verify::{TokenVerifier, Verify},
};
use crate::{Grant, PeerId, SessionRequest};

#[cfg(feature = "jwt")]
mod jwt;
mod queue;
#[cfg(feature = "rcan")]
mod rcan;
mod verify;

/// A peer's request for a session, as a policy sees it.
#[derive(Debug, Clone)]
pub struct PeerRequest<P: PeerId> {
    peer: P,
    request: SessionRequest,
    dialed: bool,
    event: Event,
    current: Option<Authorization>,
}

impl<P: PeerId> PeerRequest<P> {
    /// Creates a request of `peer` for a session it opened, as a node asks its policy.
    #[must_use]
    pub fn new(peer: P, request: SessionRequest, event: Event) -> Self {
        Self {
            peer,
            request,
            dialed: false,
            event,
            current: None,
        }
    }

    /// Returns this request for a session this node dials.
    ///
    /// The request is then the one the node sends.
    #[must_use]
    pub fn with_dialed(mut self) -> Self {
        self.dialed = true;
        self
    }

    /// Returns this request with the authorization the peer holds now.
    #[must_use]
    pub fn with_current(mut self, authorization: Authorization) -> Self {
        self.current = Some(authorization);
        self
    }

    /// Returns the peer, as the transport authenticated it.
    #[must_use]
    pub fn peer(&self) -> &P {
        &self.peer
    }

    /// Reports whether this node dialed the session.
    #[must_use]
    pub fn dialed(&self) -> bool {
        self.dialed
    }

    /// Returns why the node asks.
    #[must_use]
    pub fn event(&self) -> Event {
        self.event
    }

    /// Returns the authorization the peer holds, when the node decides again.
    #[must_use]
    pub fn current(&self) -> Option<&Authorization> {
        self.current.as_ref()
    }

    /// Returns the path the session requested, without its query.
    #[must_use]
    pub fn path(&self) -> &str {
        self.request.path()
    }

    /// Returns a query parameter of the session's request.
    #[must_use]
    pub fn query(&self, key: &str) -> Option<&str> {
        self.request.query(key)
    }

    /// Returns the token the peer presented, as [`SessionRequest::token`] does.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.request.token()
    }
}

/// Why a node asks its policy.
///
/// `Connect` and `Revalidate` are moq-auth's events of the same names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A session opens.
    Connect,
    /// A live session's authorization is due for its [`Authorization::revalidate`] check.
    Revalidate,
    /// A live session is decided again, because something may have changed.
    ///
    /// Its authorization expired, the policy's change stream yielded, or the
    /// application asked.
    Reauthorize,
}

/// A policy's answer: the session's rights, or a rejection that closes it.
pub type Decision = Result<Authorization, Rejection>;

/// What an admitted session may do, and until when.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Authorization {
    /// What the peer may subscribe to and publish.
    pub grant: Grant,
    /// Which forwarded routes cross the link.
    pub transit: Transit,
    /// When the node decides again, such as when a token expires.
    pub expires: Option<SystemTime>,
    /// How often, at the longest, the node decides again.
    pub revalidate: Option<Duration>,
}

impl Authorization {
    /// Returns an authorization that allows what either this one or `other` allows.
    ///
    /// The earliest expiry and the shortest revalidation interval win, so the
    /// node decides again as soon as either half could change.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self {
            grant: self.grant.union(&other.grant),
            transit: self.transit.union(&other.transit),
            expires: earliest(self.expires, other.expires),
            revalidate: earliest(self.revalidate, other.revalidate),
        }
    }
}

impl From<Grant> for Authorization {
    fn from(grant: Grant) -> Self {
        Self {
            grant,
            ..Default::default()
        }
    }
}

/// Returns the smaller of two optional values, where `None` is no bound.
fn earliest<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Which forwarded routes cross a link.
///
/// The default forwards nothing, which is all a node supports today: a node
/// refuses a session whose authorization asks for more.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transit {
    /// The forwarded routes the peer may read: egress.
    pub forward_to: Patterns,
    /// The peer's routes this node may forward to other links: ingress.
    pub forward_from: Patterns,
    /// Whether the peer forwards, so it is offered no publication for a set of peers.
    pub peer_forwards: bool,
}

impl Transit {
    /// Reports whether the transit forwards nothing, as the default does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward_to.is_empty() && self.forward_from.is_empty() && !self.peer_forwards
    }

    /// Returns a transit that allows what either this one or `other` allows.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut union = self.clone();
        union.forward_to.extend(other.forward_to.iter().cloned());
        union
            .forward_from
            .extend(other.forward_from.iter().cloned());
        union.peer_forwards |= other.peer_forwards;
        union
    }
}

/// Why a policy rejected a session.
///
/// The peer sees [`code`](Self::code), and [`reason`](Self::reason) stays local.
#[derive(Debug, Clone)]
pub struct Rejection {
    code: moq_net::Error,
    reason: String,
}

impl Rejection {
    /// Creates a rejection that closes the session with [`moq_net::Error::Unauthorized`].
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            code: moq_net::Error::Unauthorized,
            reason: reason.into(),
        }
    }

    /// Returns this rejection with another close code, such as [`moq_net::Error::App`].
    #[must_use]
    pub fn with_code(mut self, code: moq_net::Error) -> Self {
        self.code = code;
        self
    }

    /// Returns the code the session closes with.
    #[must_use]
    pub fn code(&self) -> &moq_net::Error {
        &self.code
    }

    /// Returns why the policy rejected the session.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.reason, self.code)
    }
}

impl std::error::Error for Rejection {}

/// Decides whether a peer may hold a session, and with which rights.
///
/// Implement it with `async fn decide`.
pub trait Policy<P: PeerId>: fmt::Debug + Send + Sync + 'static {
    /// Decides whether the peer may hold a session, and with which rights.
    ///
    /// A node drops the future on its decide timeout and when it shuts down,
    /// so an implementation must tolerate cancellation at any await.
    fn decide(&self, request: &PeerRequest<P>) -> impl Future<Output = Decision> + Send;

    /// Returns a stream that yields when earlier decisions may be stale.
    ///
    /// A node calls it once and keeps the stream. On each item it decides
    /// again for every peer with a live session.
    fn changes(&self) -> Option<BoxStream<()>> {
        None
    }

    /// Returns a policy that admits what this one or `other` admits.
    ///
    /// When both admit, the authorizations are joined with
    /// [`Authorization::union`].
    ///
    /// [`Open`] and [`Closed`] are policies for every peer type, so a union
    /// of two of them names the peer type:
    /// `Policy::<EndpointId>::union(Open(grant), Closed)`.
    fn union<B: Policy<P>>(self, other: B) -> Union<Self, B>
    where
        Self: Sized,
    {
        Union { a: self, b: other }
    }

    /// Returns a policy that rejects the peers `admit` refuses, and asks this one otherwise.
    ///
    /// The predicate is not watched: when the state it reads changes, tell
    /// the node to decide again.
    fn filter<F>(self, admit: F) -> Filter<Self, F>
    where
        Self: Sized,
        F: Fn(&PeerRequest<P>) -> bool + Send + Sync + 'static,
    {
        Filter { inner: self, admit }
    }

    /// Returns a policy that rejects with [`moq_net::Error::Timeout`] when this one takes longer than `after`.
    ///
    /// A [`union`](Self::union) waits for both halves, so a half that never
    /// answers, such as a [`Queue`] nobody reads, holds the union until the
    /// node's own decide timeout rejects it as a whole. Wrap that half with
    /// a shorter timeout, and the union admits on the other half:
    /// `Open(grant).union(queue.timeout(Duration::from_secs(5)))`.
    ///
    /// Needs a tokio runtime with time enabled, as the node runs on.
    fn timeout(self, after: Duration) -> Timeout<Self>
    where
        Self: Sized,
    {
        Timeout { inner: self, after }
    }
}

/// A [`Policy`] with its type erased, as a node stores it.
///
/// `Arc<dyn DynPolicy<P>>` implements [`Policy`]. Implement [`Policy`], not
/// this trait.
pub trait DynPolicy<P: PeerId>: fmt::Debug + Send + Sync + 'static {
    /// Decides as [`Policy::decide`] does, with a boxed future.
    fn decide<'a>(
        &'a self,
        request: &'a PeerRequest<P>,
    ) -> Pin<Box<dyn Future<Output = Decision> + Send + 'a>>;

    /// Returns the change stream, as [`Policy::changes`] does.
    fn changes(&self) -> Option<BoxStream<()>>;
}

impl<P: PeerId, T: Policy<P>> DynPolicy<P> for T {
    fn decide<'a>(
        &'a self,
        request: &'a PeerRequest<P>,
    ) -> Pin<Box<dyn Future<Output = Decision> + Send + 'a>> {
        Box::pin(Policy::decide(self, request))
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        Policy::changes(self)
    }
}

impl<P: PeerId> Policy<P> for Arc<dyn DynPolicy<P>> {
    async fn decide(&self, request: &PeerRequest<P>) -> Decision {
        DynPolicy::decide(&**self, request).await
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        DynPolicy::changes(&**self)
    }
}

/// A policy that admits everyone with one grant.
///
/// A node's default policy is `Open(Grant::subscribe_only())`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open(pub Grant);

impl<P: PeerId> Policy<P> for Open {
    fn decide(&self, _request: &PeerRequest<P>) -> impl Future<Output = Decision> + Send {
        future::ready(Ok(self.0.clone().into()))
    }
}

/// A policy that rejects everyone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Closed;

impl<P: PeerId> Policy<P> for Closed {
    fn decide(&self, _request: &PeerRequest<P>) -> impl Future<Output = Decision> + Send {
        future::ready(Err(Rejection::new("closed")))
    }
}

/// Returns a policy that decides with the synchronous closure `decide`.
///
/// # Examples
///
/// ```
/// use moq_ext_access::{
///     Grant,
///     policy::{self, PeerRequest, Rejection},
/// };
///
/// let policy = policy::from_fn(|request: &PeerRequest<String>| {
///     if request.token() == Some("secret") {
///         Ok(Grant::everything().into())
///     } else {
///         Err(Rejection::new("no valid token"))
///     }
/// });
/// # drop(policy);
/// ```
#[must_use]
pub fn from_fn<P, F>(decide: F) -> FromFn<F>
where
    P: PeerId,
    F: Fn(&PeerRequest<P>) -> Decision + Send + Sync + 'static,
{
    FromFn(decide)
}

/// A policy that decides with a closure, built by [`from_fn`].
#[derive(Clone)]
pub struct FromFn<F>(F);

impl<F> fmt::Debug for FromFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FromFn(..)")
    }
}

impl<P, F> Policy<P> for FromFn<F>
where
    P: PeerId,
    F: Fn(&PeerRequest<P>) -> Decision + Send + Sync + 'static,
{
    fn decide(&self, request: &PeerRequest<P>) -> impl Future<Output = Decision> + Send {
        future::ready((self.0)(request))
    }
}

/// A policy that admits what either of two policies admits, built by [`Policy::union`].
///
/// It waits for both halves. Wrap a half that may not answer in time with
/// [`Policy::timeout`].
#[derive(Debug, Clone)]
pub struct Union<A, B> {
    a: A,
    b: B,
}

impl<P: PeerId, A: Policy<P>, B: Policy<P>> Policy<P> for Union<A, B> {
    async fn decide(&self, request: &PeerRequest<P>) -> Decision {
        let (a, b) = future::zip(self.a.decide(request), self.b.decide(request)).await;
        match (a, b) {
            (Ok(a), Ok(b)) => Ok(a.union(&b)),
            (Ok(admitted), Err(_)) | (Err(_), Ok(admitted)) => Ok(admitted),
            (Err(rejection), Err(_)) => Err(rejection),
        }
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        match (self.a.changes(), self.b.changes()) {
            (Some(a), Some(b)) => Some(Box::pin(MergeBounded::from_iter([a, b]))),
            (a, b) => a.or(b),
        }
    }
}

/// A policy that rejects the peers a predicate refuses, built by [`Policy::filter`].
#[derive(Clone)]
pub struct Filter<T, F> {
    inner: T,
    admit: F,
}

impl<T: fmt::Debug, F> fmt::Debug for Filter<T, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Filter")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<P, T, F> Policy<P> for Filter<T, F>
where
    P: PeerId,
    T: Policy<P>,
    F: Fn(&PeerRequest<P>) -> bool + Send + Sync + 'static,
{
    async fn decide(&self, request: &PeerRequest<P>) -> Decision {
        if !(self.admit)(request) {
            return Err(Rejection::new("filtered"));
        }
        self.inner.decide(request).await
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        self.inner.changes()
    }
}

/// A policy that gives up after a while, built by [`Policy::timeout`].
#[derive(Debug, Clone)]
pub struct Timeout<T> {
    inner: T,
    after: Duration,
}

impl<P: PeerId, T: Policy<P>> Policy<P> for Timeout<T> {
    async fn decide(&self, request: &PeerRequest<P>) -> Decision {
        n0_future::time::timeout(self.after, self.inner.decide(request))
            .await
            .unwrap_or_else(|_| Err(too_slow()))
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        self.inner.changes()
    }
}

/// Asks `policy` about `request` within `timeout`, as a node does before it admits a session.
///
/// A policy that takes longer is a rejection with
/// [`moq_net::Error::Timeout`]. So is an authorization with a [`Transit`],
/// since nothing forwards yet.
///
/// Needs a tokio runtime with time enabled. Dropping the future drops the
/// policy's.
pub async fn decide_within<P: PeerId>(
    policy: &dyn DynPolicy<P>,
    request: &PeerRequest<P>,
    timeout: Duration,
) -> Decision {
    let authorization = n0_future::time::timeout(timeout, policy.decide(request))
        .await
        .unwrap_or_else(|_| Err(too_slow()))?;
    if !authorization.transit.is_empty() {
        return Err(Rejection::new("transit is not supported yet"));
    }
    Ok(authorization)
}

/// Returns the rejection of a policy that did not decide in time.
fn too_slow() -> Rejection {
    Rejection::new("the policy did not decide in time").with_code(moq_net::Error::Timeout)
}

#[cfg(test)]
mod tests;
