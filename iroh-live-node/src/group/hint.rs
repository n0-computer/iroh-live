//! Signed hints: what a group member tells the others about itself.
//!
//! A hint names the member's site and whether it forwards. The member signs
//! it with its endpoint key, so a hint relayed by anyone still proves who
//! wrote it. On the wire a hint is a version byte, the postcard encoding of
//! [`Hint`], and an ed25519 signature over a domain string, the version and
//! the payload. A hint expires [`HINT_LIFETIME`] after it is issued, and the
//! member renews it well before.

// TODO(groups): a `token` field, so a viewer can check a forwarder's
// capability chain and a principal authority can check members.

use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

use iroh::{EndpointId, SecretKey, Signature};
use moq_ext_access::GroupId;
use serde::{Deserialize, Serialize};

use super::site::SiteId;

/// The version byte of the hints this node writes and reads.
const VERSION: u8 = 1;

/// The domain string the signature covers, so a hint signature is never valid elsewhere.
const DOMAIN: &[u8] = b"iroh-live group hint v1";

/// How long a hint stays valid after it is issued.
pub(crate) const HINT_LIFETIME: Duration = Duration::from_secs(120);

/// The weight of a member that forwards.
///
/// Zero never forwards. Other values leave room for forward classes.
// TODO(groups): `ForwardClass`, with weighted rendezvous hashing and `No` on
// phones, which writes weight zero.
const FORWARDING_WEIGHT: u8 = 2;

/// A member's hint, as version 1 encodes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Hint {
    /// The group the hint is for.
    group: [u8; 32],
    /// The member's site, or `None` if it has no public IPv4 address.
    pub(crate) site: Option<SiteId>,
    /// How much the member forwards: zero never.
    pub(crate) weight: u8,
    /// When the member issued the hint, in milliseconds since the Unix epoch.
    pub(crate) issued_ms: u64,
    /// When the hint expires, in milliseconds since the Unix epoch.
    pub(crate) expires_ms: u64,
}

/// Why a hint was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum HintError {
    /// The hint is of a version this node does not read.
    #[display("unknown version")]
    Version,
    /// The hint does not decode.
    #[display("malformed")]
    Malformed,
    /// The signature does not verify against the signer.
    #[display("bad signature")]
    Signature,
    /// The hint is for another group.
    #[display("another group")]
    Group,
    /// The hint has expired.
    #[display("expired")]
    Expired,
}

impl Hint {
    /// Returns a hint for `group` at `site`, issued at `now_ms`.
    pub(crate) fn new(group: GroupId, site: Option<SiteId>, now_ms: u64) -> Self {
        Self {
            group: *group.as_bytes(),
            site,
            weight: FORWARDING_WEIGHT,
            issued_ms: now_ms,
            expires_ms: now_ms + HINT_LIFETIME.as_millis() as u64,
        }
    }

    /// Returns the hint signed with `key`, as the wire carries it.
    pub(crate) fn sign(&self, key: &SecretKey) -> Vec<u8> {
        let payload = postcard::to_stdvec(self).expect("a hint always encodes");
        let signature = key.sign(&signed_message(&payload));
        let mut bytes = Vec::with_capacity(1 + payload.len() + Signature::LENGTH);
        bytes.push(VERSION);
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&signature.to_bytes());
        bytes
    }

    /// Reports whether the hint has expired at `now_ms`.
    pub(crate) fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_ms <= now_ms
    }
}

/// Returns the message a hint signature covers: the domain, the version and the payload.
fn signed_message(payload: &[u8]) -> Vec<u8> {
    [DOMAIN, &[VERSION], payload].concat()
}

/// Verifies a hint that `signer` wrote for `group`, and returns it.
///
/// # Errors
///
/// Fails with a [`HintError`] for an unknown version, bytes that do not
/// decode, a signature that does not verify against `signer`, a hint for
/// another group, and a hint that expired at `now_ms`.
pub(crate) fn verify(
    bytes: &[u8],
    group: GroupId,
    signer: &EndpointId,
    now_ms: u64,
) -> Result<Hint, HintError> {
    let (&version, rest) = bytes.split_first().ok_or(HintError::Malformed)?;
    if version != VERSION {
        return Err(HintError::Version);
    }
    let split = rest
        .len()
        .checked_sub(Signature::LENGTH)
        .ok_or(HintError::Malformed)?;
    let (payload, signature) = rest.split_at(split);
    let signature =
        Signature::from_bytes(signature.try_into().expect("split at the signature length"));
    signer
        .verify(&signed_message(payload), &signature)
        .map_err(|_| HintError::Signature)?;
    let hint: Hint = postcard::from_bytes(payload).map_err(|_| HintError::Malformed)?;
    if hint.group != *group.as_bytes() {
        return Err(HintError::Group);
    }
    if hint.is_expired(now_ms) {
        return Err(HintError::Expired);
    }
    Ok(hint)
}

/// Returns the wall clock in milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The newest valid hint of each member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Hints(BTreeMap<EndpointId, Hint>);

impl Hints {
    /// Keeps `hint` from `signer` unless an equal or newer one is kept already.
    ///
    /// Returns whether the hint was kept.
    pub(crate) fn offer(&mut self, signer: EndpointId, hint: Hint) -> bool {
        match self.0.get(&signer) {
            Some(kept) if kept.issued_ms >= hint.issued_ms => false,
            _ => {
                self.0.insert(signer, hint);
                true
            }
        }
    }

    /// Drops the hints that expired at `now_ms`, and of signers outside `keep`.
    ///
    /// Returns whether a hint was dropped.
    pub(crate) fn retain(&mut self, now_ms: u64, keep: impl Fn(&EndpointId) -> bool) -> bool {
        let before = self.0.len();
        self.0
            .retain(|signer, hint| !hint.is_expired(now_ms) && keep(signer));
        self.0.len() != before
    }

    /// Returns the hint of `member`.
    pub(crate) fn get(&self, member: &EndpointId) -> Option<&Hint> {
        self.0.get(member)
    }

    /// Returns the earliest expiry of the kept hints, in milliseconds since the Unix epoch.
    pub(crate) fn next_expiry(&self) -> Option<u64> {
        self.0.values().map(|hint| hint.expires_ms).min()
    }

    /// Returns the members with a kept hint.
    pub(crate) fn signers(&self) -> impl Iterator<Item = &EndpointId> {
        self.0.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn group() -> GroupId {
        GroupId::from_bytes([7; 32])
    }

    fn site() -> SiteId {
        SiteId::new(group(), "198.18.0.1".parse().expect("addr"))
    }

    #[test]
    fn a_signed_hint_verifies() {
        let key = SecretKey::generate();
        let hint = Hint::new(group(), Some(site()), NOW);
        let bytes = hint.sign(&key);
        assert!(bytes.len() < 128, "{} bytes", bytes.len());
        assert_eq!(verify(&bytes, group(), &key.public(), NOW + 1), Ok(hint));
    }

    #[test]
    fn a_bad_signature_is_refused() {
        let key = SecretKey::generate();
        let mut bytes = Hint::new(group(), Some(site()), NOW).sign(&key);
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(
            verify(&bytes, group(), &key.public(), NOW),
            Err(HintError::Signature)
        );
    }

    #[test]
    fn a_hint_for_another_group_is_refused() {
        let key = SecretKey::generate();
        let other = GroupId::from_bytes([8; 32]);
        let bytes = Hint::new(other, None, NOW).sign(&key);
        assert_eq!(
            verify(&bytes, group(), &key.public(), NOW),
            Err(HintError::Group)
        );
    }

    #[test]
    fn an_expired_hint_is_refused() {
        let key = SecretKey::generate();
        let bytes = Hint::new(group(), None, NOW).sign(&key);
        let later = NOW + HINT_LIFETIME.as_millis() as u64;
        assert_eq!(
            verify(&bytes, group(), &key.public(), later),
            Err(HintError::Expired)
        );
    }

    #[test]
    fn a_hint_of_another_signer_is_refused() {
        let bytes = Hint::new(group(), None, NOW).sign(&SecretKey::generate());
        let other = SecretKey::generate().public();
        assert_eq!(
            verify(&bytes, group(), &other, NOW),
            Err(HintError::Signature)
        );
    }

    #[test]
    fn a_signature_without_the_domain_is_refused() {
        let key = SecretKey::generate();
        let hint = Hint::new(group(), None, NOW);
        let payload = postcard::to_stdvec(&hint).expect("encodes");
        let signature = key.sign(&[&[VERSION][..], &payload].concat());
        let bytes = [&[VERSION][..], &payload, &signature.to_bytes()].concat();
        assert_eq!(
            verify(&bytes, group(), &key.public(), NOW),
            Err(HintError::Signature)
        );
    }

    #[test]
    fn an_unknown_version_or_short_input_is_refused() {
        let key = SecretKey::generate();
        let mut bytes = Hint::new(group(), None, NOW).sign(&key);
        bytes[0] = 2;
        assert_eq!(
            verify(&bytes, group(), &key.public(), NOW),
            Err(HintError::Version)
        );
        assert_eq!(
            verify(&[VERSION, 1, 2], group(), &key.public(), NOW),
            Err(HintError::Malformed)
        );
        assert_eq!(
            verify(&[], group(), &key.public(), NOW),
            Err(HintError::Malformed)
        );
    }

    #[test]
    fn an_older_hint_does_not_replace_a_newer_one() {
        let signer = SecretKey::generate().public();
        let mut hints = Hints::default();
        let newer = Hint::new(group(), Some(site()), NOW + 10);
        assert!(hints.offer(signer, newer.clone()));
        assert!(!hints.offer(signer, Hint::new(group(), None, NOW)));
        assert!(!hints.offer(signer, newer.clone()), "an equal one");
        assert_eq!(hints.get(&signer), Some(&newer));
        assert!(hints.offer(signer, Hint::new(group(), None, NOW + 20)));
    }

    #[test]
    fn retain_drops_expired_hints_and_outsiders() {
        let (a, b) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let mut hints = Hints::default();
        hints.offer(a, Hint::new(group(), None, NOW));
        hints.offer(b, Hint::new(group(), None, NOW + 1000));
        assert!(!hints.retain(NOW, |_| true));
        assert!(hints.retain(NOW + HINT_LIFETIME.as_millis() as u64, |_| true));
        assert_eq!(hints.signers().collect::<Vec<_>>(), [&b]);
        assert!(hints.retain(NOW, |signer| *signer != b));
        assert_eq!(hints.next_expiry(), None);
    }
}
