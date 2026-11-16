//! A token verifier for rcan capability chains, bound to the peer's key.

use std::{
    borrow::Borrow,
    time::{Duration, SystemTime},
};

use ed25519_dalek::VerifyingKey;
use moq_net::{Pattern, Patterns};
use rcan::{Authorizer, Capability, Expires, Rcan};
use serde::{Deserialize, Serialize};

use super::{Authorization, Decision, PeerRequest, Rejection, TokenVerifier};
use crate::{Grant, PeerId};

/// The most links a capability chain may have.
pub const MAX_LINKS: usize = 8;

/// The most bytes one encoded link of a capability chain may take.
pub const MAX_LINK_BYTES: usize = 2 * 1024;

/// The prefix of an encoded capability chain.
///
/// The `1` names rcan's version-1 token format, which this crate reads.
const PREFIX: &str = "rcan1:";

/// What a holder may do at a moq node: publish and subscribe under a root.
///
/// The patterns are relative to [`root`](Self::root), as in a moq-auth JWT.
/// One capability permits another when its paths cover the other's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "Wire", try_from = "Wire")]
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
/// use moq_ext_access::policy::{MoqCapability, RcanVerifier, encode_chain};
/// use rcan::{Expires, Rcan};
///
/// let alice = SigningKey::from_bytes(&[1; 32]);
/// let device = SigningKey::from_bytes(&[2; 32]);
/// let capability =
///     MoqCapability::new("spaces/alice").with_publish("device/**".parse::<moq_net::Pattern>()?);
/// let link = Rcan::issuing_builder(&alice, device.verifying_key(), capability)
///     .sign(Expires::valid_for(std::time::Duration::from_secs(3600)));
/// // The device presents this as its token:
/// let token = encode_chain(&[link]);
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

impl<P> TokenVerifier<P> for RcanVerifier
where
    P: PeerId + Borrow<[u8; 32]>,
{
    fn verify(&self, token: &str, request: &PeerRequest<P>) -> Decision {
        let invoker = VerifyingKey::from_bytes(request.peer().borrow())
            .map_err(|_| Rejection::new("the peer id is not an ed25519 key"))?;
        let chain = decode_chain(token)?;
        let leaf = chain
            .last()
            .ok_or_else(|| Rejection::new("the capability chain is empty"))?
            .capability()
            .clone();
        if !within(&leaf.root, &self.root) {
            return Err(Rejection::new(format!(
                "the capability's root {:?} is outside {:?}",
                leaf.root, self.root
            )));
        }
        let proofs: Vec<&Rcan<MoqCapability>> = chain.iter().collect();
        if let Err(err) =
            Authorizer::new(self.principal).check_invocation_from(invoker, leaf.clone(), &proofs)
        {
            // rcan fails an expired link like any other. Only a chain that
            // runs from the principal to the invoker counts as expired, so a
            // forged or foreign chain never picks its own rejection code.
            let now = SystemTime::now();
            if chain.iter().any(|link| !link.expires().is_valid_at(now))
                && runs_between(&chain, &self.principal, &invoker)
            {
                return Err(Rejection::new("a link of the capability chain expired")
                    .with_code(moq_net::Error::Expired));
            }
            return Err(Rejection::new(format!("invalid capability chain: {err:#}")));
        }
        let grant = leaf
            .grant()
            .map_err(|err| Rejection::new(format!("invalid capability: {err}")))?;
        let expires = chain
            .iter()
            .filter_map(|link| match link.expires() {
                Expires::Never => None,
                // Valid through the whole second it names. A time beyond the
                // clock's range is never, and the other links still bound it.
                Expires::At(secs) => {
                    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs.saturating_add(1)))
                }
            })
            .min();
        Ok(Authorization { grant, expires })
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
/// encoding of the links. A peer presents it as its session token, and a
/// [`RcanVerifier`] reads it.
#[must_use]
pub fn encode_chain(chain: &[Rcan<MoqCapability>]) -> String {
    let bytes = postcard::to_stdvec(chain).expect("encoding into a vec cannot fail");
    let encoded = data_encoding::BASE32_NOPAD
        .encode(&bytes)
        .to_ascii_lowercase();
    format!("{PREFIX}{encoded}")
}

/// Decodes a token from [`encode_chain`], within the size limits.
///
/// Decoding checks each link's signature.
fn decode_chain(token: &str) -> Result<Vec<Rcan<MoqCapability>>, Rejection> {
    let encoded = token
        .strip_prefix(PREFIX)
        .ok_or_else(|| Rejection::new("not an rcan capability chain"))?;
    let limit = data_encoding::BASE32_NOPAD.encode_len(MAX_LINKS * MAX_LINK_BYTES);
    if encoded.len() > limit {
        return Err(Rejection::new("the capability chain is too large"));
    }
    let bytes = data_encoding::BASE32_NOPAD_NOCASE
        .decode(encoded.as_bytes())
        .map_err(|err| Rejection::new(format!("the capability chain is not base32: {err}")))?;
    let chain: Vec<Rcan<MoqCapability>> = postcard::from_bytes(&bytes)
        .map_err(|err| Rejection::new(format!("invalid capability chain: {err}")))?;
    if chain.len() > MAX_LINKS {
        return Err(Rejection::new(format!(
            "the capability chain has {} links, more than {MAX_LINKS}",
            chain.len()
        )));
    }
    if chain
        .iter()
        .any(|link| link.encode().len() > MAX_LINK_BYTES)
    {
        return Err(Rejection::new(
            "a link of the capability chain is too large",
        ));
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
