//! Admission and authorization of sessions, decided by a [`Policy`].
//!
//! A node asks its policy when a session opens, in either direction, and
//! again later (see [`Event`]). The answer is a [`Decision`]. The building
//! blocks compose:
//!
//! - [`Open`] and [`Closed`] admit or reject everyone.
//! - [`from_fn`] decides with a synchronous closure.
//! - [`Policy::filter`] narrows a policy by a predicate.
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

use n0_future::future;

#[cfg(feature = "jwt")]
pub use self::jwt::JwtVerifier;
#[cfg(feature = "rcan")]
pub use self::rcan::{MAX_LINK_BYTES, MAX_LINKS, MoqCapability, RcanVerifier, encode_chain};
pub use self::{
    queue::{Pending, Queue},
    verify::{TokenVerifier, Verify},
};
use crate::{Direction, Grant, PeerId, SessionRequest};

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
    direction: Direction,
    event: Event,
}

impl<P: PeerId> PeerRequest<P> {
    /// Creates a request of `peer` for a session it opened, as a node asks its policy.
    #[must_use]
    pub fn new(peer: P, request: SessionRequest, event: Event) -> Self {
        Self {
            peer,
            request,
            direction: Direction::Accepted,
            event,
        }
    }

    /// Returns this request with `direction` saying which side opens the session.
    ///
    /// For a session this node dials, the request is the one the node sends.
    #[must_use]
    pub fn with_direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Returns the peer, as the transport authenticated it.
    #[must_use]
    pub fn peer(&self) -> &P {
        &self.peer
    }

    /// Returns which side opened the session.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Returns why the node asks.
    #[must_use]
    pub fn event(&self) -> Event {
        self.event
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A session opens.
    Connect,
    /// A live session is decided again, because its authorization expired or the application asked.
    Reauthorize,
}

/// A policy's answer: the session's rights, or a rejection that closes it.
pub type Decision = Result<Authorization, Rejection>;

/// What an admitted session may do, and until when.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Authorization {
    /// What the peer may subscribe to and publish.
    pub grant: Grant,
    /// When the node decides again, such as when a token expires.
    pub expires: Option<SystemTime>,
}

impl From<Grant> for Authorization {
    fn from(grant: Grant) -> Self {
        Self {
            grant,
            ..Default::default()
        }
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

/// A rule that decides whether a peer may hold a session, and with which rights.
///
/// Implement it with `async fn decide`.
pub trait Policy<P: PeerId>: fmt::Debug + Send + Sync + 'static {
    /// Decides whether the peer may hold a session, and with which rights.
    ///
    /// A node drops the future on its decide timeout and when it shuts down,
    /// so an implementation must tolerate cancellation at any await.
    fn decide(&self, request: &PeerRequest<P>) -> impl Future<Output = Decision> + Send;

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
}

impl<P: PeerId, T: Policy<P>> DynPolicy<P> for T {
    fn decide<'a>(
        &'a self,
        request: &'a PeerRequest<P>,
    ) -> Pin<Box<dyn Future<Output = Decision> + Send + 'a>> {
        Box::pin(Policy::decide(self, request))
    }
}

impl<P: PeerId> Policy<P> for Arc<dyn DynPolicy<P>> {
    async fn decide(&self, request: &PeerRequest<P>) -> Decision {
        DynPolicy::decide(&**self, request).await
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
}

/// Asks `policy` about `request` within `timeout`, as a node does before it admits a session.
///
/// A policy that takes longer is a rejection with
/// [`moq_net::Error::Timeout`].
///
/// Needs a tokio runtime with time enabled. Dropping the future drops the
/// policy's.
pub async fn decide_within<P: PeerId>(
    policy: &dyn DynPolicy<P>,
    request: &PeerRequest<P>,
    timeout: Duration,
) -> Decision {
    n0_future::time::timeout(timeout, policy.decide(request))
        .await
        .unwrap_or_else(|_| Err(too_slow()))
}

/// Returns the rejection of a policy that did not decide in time.
fn too_slow() -> Rejection {
    Rejection::new("the policy did not decide in time").with_code(moq_net::Error::Timeout)
}

#[cfg(test)]
mod tests;
