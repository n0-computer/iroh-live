//! Policies that decide on the token a peer presents.

use std::fmt;

use n0_future::boxed::BoxStream;

use super::{Closed, Decision, Event, PeerRequest, Policy};
use crate::PeerId;

/// Checks a token a peer presents, and returns what it authorizes.
///
/// A verifier is synchronous, since checking a signature is CPU work. Wrap
/// it in [`Verify`] to use it as a [`Policy`].
pub trait TokenVerifier<P: PeerId>: fmt::Debug + Send + Sync + 'static {
    /// Verifies `token` for the request's peer and returns what it authorizes.
    ///
    /// The token is an argument of its own, so a token that does not come
    /// from the session's setup can go through the same verifier.
    fn verify(&self, token: &str, request: &PeerRequest<P>) -> Decision;
}

/// A policy that decides with a [`TokenVerifier`] when the peer presents a token.
///
/// With a token, the verifier decides. A token it rejects is a rejection,
/// and never falls through to the fallback, so a peer cannot get the
/// fallback's rights with a forged token. Without a token, the fallback
/// policy decides. The default fallback, [`Closed`], makes the token
/// required.
///
/// One case falls through: when a node decides again about a live session
/// and the verifier rejects its token with [`moq_net::Error::Expired`], the
/// fallback decides. A session whose token expires thus narrows to the
/// fallback's rights, or closes under [`Closed`]. A new session with an
/// expired token is rejected.
///
/// A node that dials a peer decides with no token, so the fallback decides
/// every dial, and under [`Closed`] the node can dial nobody.
///
/// The token is the one [`SessionRequest::token`](crate::SessionRequest::token)
/// returns.
///
/// # Examples
///
/// Admit peers with a valid token, and let others subscribe:
///
/// ```
/// use moq_ext_access::{
///     Grant,
///     policy::{self, Decision, PeerRequest, Rejection, TokenVerifier, Verify},
/// };
///
/// #[derive(Debug)]
/// struct Password;
///
/// impl TokenVerifier<String> for Password {
///     fn verify(&self, token: &str, _request: &PeerRequest<String>) -> Decision {
///         match token {
///             "secret" => Ok(Grant::everything().into()),
///             _ => Err(Rejection::new("wrong password")),
///         }
///     }
/// }
///
/// let policy = Verify::new(Password).with_fallback(policy::Open(Grant::subscribe_only()));
/// # drop(policy);
/// ```
#[derive(Debug, Clone)]
pub struct Verify<V, F = Closed> {
    verifier: V,
    fallback: F,
}

impl<V> Verify<V> {
    /// Creates a policy that requires a token, which `verifier` checks.
    #[must_use]
    pub fn new(verifier: V) -> Self {
        Self {
            verifier,
            fallback: Closed,
        }
    }
}

impl<V, F> Verify<V, F> {
    /// Returns this policy with `fallback` deciding the requests without a token.
    #[must_use]
    pub fn with_fallback<G>(self, fallback: G) -> Verify<V, G> {
        Verify {
            verifier: self.verifier,
            fallback,
        }
    }
}

impl<P, V, F> Policy<P> for Verify<V, F>
where
    P: PeerId,
    V: TokenVerifier<P>,
    F: Policy<P>,
{
    async fn decide(&self, request: &PeerRequest<P>) -> Decision {
        let Some(token) = request.token() else {
            return self.fallback.decide(request).await;
        };
        match self.verifier.verify(token, request) {
            Err(rejection)
                if request.event() != Event::Connect
                    && matches!(rejection.code(), moq_net::Error::Expired) =>
            {
                self.fallback.decide(request).await
            }
            decision => decision,
        }
    }

    fn changes(&self) -> Option<BoxStream<()>> {
        self.fallback.changes()
    }
}
