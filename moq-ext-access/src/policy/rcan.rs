//! A token verifier for rcan capability chains, bound to the peer's key.

use std::{
    borrow::Borrow,
    fmt,
    str::FromStr,
    time::{Duration, SystemTime},
};

use ed25519_dalek::{SigningKey, VerifyingKey};
use moq_net::{Pattern, Patterns};
use n0_error::stack_error;
pub use rcan::Expires;
use rcan::{Authorizer, Capability, Rcan};
use serde::{Deserialize, Serialize};

use super::{Authorization, Decision, PeerRequest, Rejection, TokenVerifier};
use crate::{Grant, PeerId};

/// The most links a capability chain may have.
pub const MAX_LINKS: usize = 8;

/// The most bytes one encoded link of a capability chain may take.
pub const MAX_LINK_BYTES: usize = 2 * 1024;

/// The prefix of an encoded capability chain.
///
/// The `1` names rcan's version-1 token format, which this crate reads. A
/// token that starts with it is a chain, and any other is not.
pub(crate) const CHAIN_PREFIX: &str = "rcan1:";

/// An error from building or decoding a [`Chain`].
#[stack_error(derive)]
#[non_exhaustive]
pub enum ChainError {
    /// The token does not start with the prefix of an rcan chain.
    #[error("not an rcan capability chain")]
    NotAChain,
    /// The token holds no link.
    #[error("the capability chain is empty")]
    Empty,
    /// The token, or one of its links, is larger than the limits allow.
    #[error("the capability chain is too large")]
    TooLarge,
    /// The chain holds more than [`MAX_LINKS`] links, or a delegation would make it.
    #[error("the capability chain has {links} links, more than {MAX_LINKS} allow")]
    TooManyLinks {
        /// The links the chain holds or would hold.
        links: usize,
    },
    /// The signing key is not the one the chain's last link names.
    #[error("the signing key does not hold the capability chain: its last link names another key")]
    NotHolder,
    /// The capability to delegate is wider than the chain's last link.
    #[error("the capability is wider than the one the chain delegates")]
    Wider,
    /// The token is not base32, or a link does not decode or has an invalid signature.
    #[error("invalid capability chain: {reason}")]
    Invalid {
        /// Why the token does not decode.
        reason: String,
    },
}

impl From<ChainError> for Rejection {
    /// Returns the rejection of a session that presented a chain that does not decode.
    fn from(err: ChainError) -> Self {
        Self::new(err.to_string())
    }
}

/// What a holder may do at a moq node: publish and subscribe under a root.
///
/// The patterns are relative to [`root`](Self::root), as in a moq-auth JWT.
/// One capability permits another when its paths cover the other's. Build
/// one with [`new`](Self::new) and the `with_*` methods, since later kinds of
/// rights may add fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "Wire", try_from = "Wire")]
#[non_exhaustive]
pub struct MoqCapability {
    /// The path the patterns are relative to.
    pub root: String,
    /// The paths the holder may publish, relative to the root.
    pub publish: Patterns,
    /// The paths the holder may subscribe to, relative to the root.
    pub subscribe: Patterns,
}

impl MoqCapability {
    /// Returns a capability under `root` that grants nothing yet.
    #[must_use]
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            publish: Patterns::new(),
            subscribe: Patterns::new(),
        }
    }

    /// Returns this capability with `publish` as the paths the holder may publish.
    #[must_use]
    pub fn with_publish(mut self, publish: impl Into<Patterns>) -> Self {
        self.publish = publish.into();
        self
    }

    /// Returns this capability with `subscribe` as the paths the holder may subscribe to.
    #[must_use]
    pub fn with_subscribe(mut self, subscribe: impl Into<Patterns>) -> Self {
        self.subscribe = subscribe.into();
        self
    }

    /// Returns the grant this capability gives, with absolute patterns.
    fn grant(&self) -> Result<Grant, moq_net::InvalidPattern> {
        Ok(Grant {
            subscribe: self.subscribe.rooted(&self.root)?,
            publish: self.publish.rooted(&self.root)?,
        })
    }
}

impl Capability for MoqCapability {
    fn permits(&self, other: &Self) -> bool {
        match (self.grant(), other.grant()) {
            (Ok(mine), Ok(theirs)) => {
                mine.publish.covers(&theirs.publish) && mine.subscribe.covers(&theirs.subscribe)
            }
            _ => false,
        }
    }
}

/// The wire form of a [`MoqCapability`].
///
/// An enum, so a later kind of capability, such as one to serve paths to a
/// set of peers, can join without breaking the tokens issued before it.
#[derive(Clone, Serialize, Deserialize)]
enum Wire {
    Access {
        root: String,
        publish: Vec<String>,
        subscribe: Vec<String>,
    },
}

impl From<MoqCapability> for Wire {
    fn from(capability: MoqCapability) -> Self {
        let strings =
            |patterns: &Patterns| patterns.iter().map(|p| p.as_str().to_owned()).collect();
        Self::Access {
            publish: strings(&capability.publish),
            subscribe: strings(&capability.subscribe),
            root: capability.root,
        }
    }
}

impl TryFrom<Wire> for MoqCapability {
    type Error = moq_net::InvalidPattern;

    fn try_from(wire: Wire) -> Result<Self, Self::Error> {
        let Wire::Access {
            root,
            publish,
            subscribe,
        } = wire;
        let patterns = |strings: Vec<String>| {
            strings
                .iter()
                .map(|pattern| pattern.parse::<Pattern>())
                .collect::<Result<Patterns, _>>()
        };
        Ok(Self {
            root,
            publish: patterns(publish)?,
            subscribe: patterns(subscribe)?,
        })
    }
}

/// A [`TokenVerifier`] of rcan capability chains that a principal issued.
///
/// The principal is the key whose space the verifier shares, such as a
/// node's own key or an app's account key. A chain starts with a
/// [`MoqCapability`] the principal issued and may delegate it, narrowing
/// it, up to [`MAX_LINKS`] links. Its last link must name the peer that
/// presents it, as the transport authenticated it, so a stolen chain is
/// useless without the peer's key.
///
/// A valid chain admits the peer with the last link's capability, which must
/// lie within the verifier's root. The authorization expires with the
/// earliest link: rcan's `valid_until` names the last second a link is
/// valid, so the authorization expires one second after it.
///
/// The peer id must borrow as its 32-byte ed25519 public key, as iroh's
/// `EndpointId` does.
///
/// # Examples
///
/// ```
/// use ed25519_dalek::SigningKey;
/// use moq_ext_access::policy::{Chain, Expires, MoqCapability, RcanVerifier};
///
/// let alice = SigningKey::from_bytes(&[1; 32]);
/// let device = SigningKey::from_bytes(&[2; 32]);
/// let capability =
///     MoqCapability::new("spaces/alice").with_publish("device/**".parse::<moq_net::Pattern>()?);
/// let chain = Chain::issue(
///     &alice,
///     device.verifying_key(),
///     capability,
///     Expires::valid_for(std::time::Duration::from_secs(3600)),
/// );
/// // The device presents this as its token:
/// let token = chain.to_token();
///
/// // The node that shares alice's space verifies it:
/// let verifier = RcanVerifier::new(alice.verifying_key(), "spaces/alice");
/// # drop((token, verifier));
/// # Ok::<_, moq_net::InvalidPattern>(())
/// ```
#[derive(Debug, Clone)]
pub struct RcanVerifier {
    principal: VerifyingKey,
    root: String,
}

impl RcanVerifier {
    /// Creates a verifier for the chains `principal` issued under `root`.
    #[must_use]
    pub fn new(principal: VerifyingKey, root: impl Into<String>) -> Self {
        Self {
            principal,
            root: root.into(),
        }
    }
}

impl RcanVerifier {
    /// Verifies `chain` as presented by `holder`, and returns what it authorizes.
    ///
    /// The holder is the key the chain's last link must name, such as the
    /// peer of a session, or the sender of a signed announcement. Any id
    /// that borrows as a 32-byte ed25519 public key works, as iroh's
    /// `EndpointId` and `PublicKey` do.
    ///
    /// This is [`TokenVerifier::verify`] for a chain that is decoded already,
    /// with the request's peer as the holder, so a caller that picks a
    /// verifier by the chain's [`principal`](Chain::principal) decodes the
    /// token once.
    pub fn verify_chain<H>(&self, chain: &Chain, holder: &H) -> Decision
    where
        H: Borrow<[u8; 32]>,
    {
        let invoker = VerifyingKey::from_bytes(holder.borrow())
            .map_err(|_| Rejection::new("the holder is not an ed25519 key"))?;
        let leaf = chain.capability().clone();
        if !within(&leaf.root, &self.root) {
            return Err(Rejection::new(format!(
                "the capability's root {:?} is outside {:?}",
                leaf.root, self.root
            )));
        }
        let links = chain.links();
        let proofs: Vec<&Rcan<MoqCapability>> = links.iter().collect();
        if let Err(err) =
            Authorizer::new(self.principal).check_invocation_from(invoker, leaf.clone(), &proofs)
        {
            // rcan fails an expired link like any other. Only a chain that
            // runs from the principal to the invoker counts as expired, so a
            // forged or foreign chain never picks its own rejection code.
            let now = SystemTime::now();
            if links.iter().any(|link| !link.expires().is_valid_at(now))
                && runs_between(links, &self.principal, &invoker)
            {
                return Err(Rejection::new("a link of the capability chain expired")
                    .with_code(moq_net::Error::Expired));
            }
            return Err(Rejection::new(format!("invalid capability chain: {err:#}")));
        }
        let grant = leaf
            .grant()
            .map_err(|err| Rejection::new(format!("invalid capability: {err}")))?;
        Ok(Authorization {
            grant,
            expires: chain.expires(),
        })
    }
}

impl<P> TokenVerifier<P> for RcanVerifier
where
    P: PeerId + Borrow<[u8; 32]>,
{
    fn verify(&self, token: &str, request: &PeerRequest<P>) -> Decision {
        self.verify_chain(&token.parse()?, request.peer())
    }
}

/// A capability chain, from the principal that issued it to the key that presents it.
///
/// The first link is the principal's, and each later link delegates the
/// capability of the one before it, the same or narrower. A chain is never
/// empty. A peer presents it as a token, from [`to_token`](Self::to_token),
/// and a [`RcanVerifier`] that trusts the principal checks it.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use ed25519_dalek::SigningKey;
/// use moq_ext_access::policy::{Chain, Expires, MoqCapability};
///
/// let team = SigningKey::from_bytes(&[1; 32]);
/// let alice = SigningKey::from_bytes(&[2; 32]);
/// let camera = SigningKey::from_bytes(&[3; 32]);
/// let member = MoqCapability::new("").with_subscribe(moq_net::Pattern::all());
/// let to_alice = Chain::issue(
///     &team,
///     alice.verifying_key(),
///     member,
///     Expires::valid_for(Duration::from_secs(30 * 24 * 3600)),
/// );
/// // Alice hands the same capability on to her camera, for eight hours.
/// let to_camera = to_alice
///     .delegate(&alice, camera.verifying_key(), None, Expires::valid_for(Duration::from_secs(8 * 3600)))
///     .expect("alice holds the chain");
/// let token = to_camera.to_token();
/// assert_eq!(token.parse::<Chain>().expect("decodes"), to_camera);
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Chain(Vec<Rcan<MoqCapability>>);

impl Chain {
    /// Returns a chain of one link: `principal` issues `capability` to `to`.
    #[must_use]
    pub fn issue(
        principal: &SigningKey,
        to: VerifyingKey,
        capability: MoqCapability,
        expires: Expires,
    ) -> Self {
        Self(vec![
            Rcan::issuing_builder(principal, to, capability).sign(expires),
        ])
    }

    /// Returns this chain with one more link, from its holder to `to`.
    ///
    /// The new link carries `narrower`, or the last link's capability when it
    /// is `None`. The principal's capability stays the one the chain
    /// delegates, so a verifier that trusts the principal accepts the result.
    ///
    /// # Errors
    ///
    /// Fails with [`ChainError::NotHolder`] if `holder` is not the key the
    /// last link names, with [`ChainError::Wider`] if `narrower` is wider
    /// than the last link's capability, and with
    /// [`ChainError::TooManyLinks`] if the chain already has [`MAX_LINKS`]
    /// links.
    pub fn delegate(
        &self,
        holder: &SigningKey,
        to: VerifyingKey,
        narrower: Option<MoqCapability>,
        expires: Expires,
    ) -> Result<Self, ChainError> {
        if self.0.len() >= MAX_LINKS {
            return Err(ChainError::TooManyLinks {
                links: self.0.len() + 1,
            });
        }
        if holder.verifying_key() != *self.holder() {
            return Err(ChainError::NotHolder);
        }
        let capability = match narrower {
            Some(narrower) if !self.capability().permits(&narrower) => {
                return Err(ChainError::Wider);
            }
            Some(narrower) => narrower,
            None => self.capability().clone(),
        };
        let link =
            Rcan::delegating_builder(holder, to, *self.principal(), capability).sign(expires);
        let mut links = self.0.clone();
        links.push(link);
        Ok(Self(links))
    }

    /// Returns the links, the principal's first.
    #[must_use]
    pub fn links(&self) -> &[Rcan<MoqCapability>] {
        &self.0
    }

    /// Returns the key that issued the first link.
    #[must_use]
    pub fn principal(&self) -> &VerifyingKey {
        self.0[0].issuer()
    }

    /// Returns the key the last link names, which presents the chain.
    #[must_use]
    pub fn holder(&self) -> &VerifyingKey {
        self.0[self.0.len() - 1].audience()
    }

    /// Returns the last link's capability, which the chain grants its holder.
    #[must_use]
    pub fn capability(&self) -> &MoqCapability {
        self.0[self.0.len() - 1].capability()
    }

    /// Returns when the chain stops authorizing, or `None` if no link expires.
    ///
    /// rcan's expiry names the last second a link is valid, so the chain
    /// expires one second after its earliest link's.
    #[must_use]
    pub fn expires(&self) -> Option<SystemTime> {
        self.0
            .iter()
            .filter_map(|link| match link.expires() {
                Expires::Never => None,
                // Valid through the whole second it names. A time beyond the
                // clock's range is never, and the other links still bound it.
                Expires::At(secs) => {
                    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs.saturating_add(1)))
                }
            })
            .min()
    }

    /// Returns the chain as a token.
    ///
    /// The token is `rcan1:` and the lowercase, unpadded base32 of the
    /// postcard encoding of the links. [`FromStr`] reads it back.
    #[must_use]
    pub fn to_token(&self) -> String {
        encode_chain(&self.0)
    }

    /// Reports whether `token` has the form of a chain, without decoding it.
    ///
    /// A token that starts with `rcan1:` is a chain, and any other is not.
    #[must_use]
    pub fn is_chain(token: &str) -> bool {
        token.starts_with(CHAIN_PREFIX)
    }
}

impl fmt::Debug for Chain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chain")
            .field("links", &self.0.len())
            .field("capability", self.capability())
            .finish()
    }
}

impl fmt::Display for Chain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_token())
    }
}

impl FromStr for Chain {
    type Err = ChainError;

    /// Decodes a token from [`to_token`](Chain::to_token), within the size limits.
    ///
    /// Decoding checks each link's signature, but not that the links form a
    /// chain: a [`RcanVerifier`] checks that. The token's base32 may be in
    /// either case.
    fn from_str(token: &str) -> Result<Self, Self::Err> {
        let links = decode_chain(token)?;
        if links.is_empty() {
            return Err(ChainError::Empty);
        }
        Ok(Self(links))
    }
}

/// Reports whether `chain` delegates `principal`'s capability link by link to `invoker`.
fn runs_between(
    chain: &[Rcan<MoqCapability>],
    principal: &VerifyingKey,
    invoker: &VerifyingKey,
) -> bool {
    let mut issuer = principal;
    for link in chain {
        if link.issuer() != issuer || link.capability_issuer() != principal {
            return false;
        }
        issuer = link.audience();
    }
    issuer == invoker
}

/// Encodes a capability chain as a token, first link first.
///
/// The token is `rcan1:` and the lowercase, unpadded base32 of the postcard
/// encoding of the links.
#[must_use]
pub(crate) fn encode_chain(chain: &[Rcan<MoqCapability>]) -> String {
    let bytes = postcard::to_stdvec(chain).expect("encoding into a vec cannot fail");
    let encoded = data_encoding::BASE32_NOPAD
        .encode(&bytes)
        .to_ascii_lowercase();
    format!("{CHAIN_PREFIX}{encoded}")
}

/// Decodes a token from [`encode_chain`], within the size limits.
///
/// # Errors
///
/// Fails if the token lacks [`CHAIN_PREFIX`], is larger than [`MAX_LINKS`]
/// links of [`MAX_LINK_BYTES`] allow, is not base32, holds more than
/// [`MAX_LINKS`] links, or holds a link that does not decode or whose
/// signature is invalid.
pub(crate) fn decode_chain(token: &str) -> Result<Vec<Rcan<MoqCapability>>, ChainError> {
    let encoded = token
        .strip_prefix(CHAIN_PREFIX)
        .ok_or(ChainError::NotAChain)?;
    let limit = data_encoding::BASE32_NOPAD.encode_len(MAX_LINKS * MAX_LINK_BYTES);
    if encoded.len() > limit {
        return Err(ChainError::TooLarge);
    }
    let bytes = data_encoding::BASE32_NOPAD_NOCASE
        .decode(encoded.as_bytes())
        .map_err(|err| ChainError::Invalid {
            reason: format!("not base32: {err}"),
        })?;
    let chain: Vec<Rcan<MoqCapability>> =
        postcard::from_bytes(&bytes).map_err(|err| ChainError::Invalid {
            reason: err.to_string(),
        })?;
    if chain.len() > MAX_LINKS {
        return Err(ChainError::TooManyLinks { links: chain.len() });
    }
    if chain
        .iter()
        .any(|link| link.encode().len() > MAX_LINK_BYTES)
    {
        return Err(ChainError::TooLarge);
    }
    Ok(chain)
}

/// Reports whether `path` is `root` or lies below it, segment by segment.
fn within(path: &str, root: &str) -> bool {
    let (path, root) = (path.trim_matches('/'), root.trim_matches('/'));
    root.is_empty()
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests;
