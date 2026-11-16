use std::fmt;

use ed25519_dalek::SigningKey;

use super::*;
use crate::{SessionRequest, policy::Event};

/// A peer id that is an ed25519 public key, as iroh's is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Device([u8; 32]);

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&data_encoding::HEXLOWER.encode(&self.0))
    }
}

impl Borrow<[u8; 32]> for Device {
    fn borrow(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Returns the signing key made from 32 bytes of `seed`.
fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Returns the peer id of the holder of `key`.
fn device(key: &SigningKey) -> Device {
    Device(key.verifying_key().to_bytes())
}

/// Parses `pattern`, which must be valid.
fn pattern(pattern: &str) -> Pattern {
    pattern.parse().expect("a pattern")
}

/// Returns a capability under `spaces/alice` to publish under `prefix`.
fn publish_under(prefix: &str) -> MoqCapability {
    MoqCapability::new("spaces/alice").with_publish(pattern(&format!("{prefix}/**")))
}

/// Returns an expiry `hours` from now.
fn hours(hours: u64) -> Expires {
    Expires::valid_for(Duration::from_secs(hours * 3600))
}

/// Verifies `token` as presented by the holder of `peer` at its connect.
fn verify(verifier: &RcanVerifier, peer: &SigningKey, token: &str) -> Decision {
    let request = PeerRequest::new(device(peer), SessionRequest::default(), Event::Connect);
    verifier.verify(token, &request)
}

/// Returns a verifier rooted at alice's key and `spaces/alice`.
fn alice_verifier() -> RcanVerifier {
    RcanVerifier::new(key(1).verifying_key(), "spaces/alice")
}

#[test]
fn a_one_link_chain_admits_its_invoker() {
    let (alice, bob) = (key(1), key(2));
    let link =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    let authorization = verify(&alice_verifier(), &bob, &encode_chain(&[link])).expect("admitted");
    assert!(authorization.grant.allows_publish("spaces/alice/bob/cam"));
    assert!(!authorization.grant.allows_publish("spaces/alice/carol/cam"));
    assert!(!authorization.grant.allows_subscribe("spaces/alice/bob/cam"));
}

#[test]
fn a_two_link_chain_admits_the_device_with_the_narrower_capability() {
    let (alice, bob, phone) = (key(1), key(2), key(3));
    let issued =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(2));
    let delegated = Rcan::delegating_builder(
        &bob,
        phone.verifying_key(),
        alice.verifying_key(),
        publish_under("bob/phone"),
    )
    .sign(hours(1));
    let token = encode_chain(&[issued, delegated]);
    let grant = verify(&alice_verifier(), &phone, &token)
        .expect("admitted")
        .grant;
    assert!(grant.allows_publish("spaces/alice/bob/phone/cam"));
    assert!(!grant.allows_publish("spaces/alice/bob/laptop/cam"));
}

#[test]
fn a_chain_presented_by_another_peer_rejects() {
    let (alice, bob, mallory) = (key(1), key(2), key(9));
    let link =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    verify(&alice_verifier(), &mallory, &encode_chain(&[link])).expect_err("rejected");
}

#[test]
fn an_expired_link_rejects() {
    let (alice, bob) = (key(1), key(2));
    let link = Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob"))
        .sign(Expires::At(1));
    let rejection = verify(&alice_verifier(), &bob, &encode_chain(&[link])).expect_err("rejected");
    assert!(matches!(rejection.code(), moq_net::Error::Expired));
}

#[test]
fn a_chain_over_the_link_limit_rejects() {
    let alice = key(1);
    let keys: Vec<SigningKey> = (2..=10).map(key).collect();
    let mut chain = vec![
        Rcan::issuing_builder(&alice, keys[0].verifying_key(), publish_under("bob")).sign(hours(1)),
    ];
    for pair in keys.windows(2) {
        chain.push(
            Rcan::delegating_builder(
                &pair[0],
                pair[1].verifying_key(),
                alice.verifying_key(),
                publish_under("bob"),
            )
            .sign(hours(1)),
        );
    }
    assert_eq!(chain.len(), MAX_LINKS + 1);
    let last = keys.last().expect("a key");
    let rejection = verify(&alice_verifier(), last, &encode_chain(&chain)).expect_err("rejected");
    assert!(rejection.reason().contains("more than"), "{rejection}");
    // At the limit, the same chain admits.
    chain.pop();
    verify(
        &alice_verifier(),
        &keys[MAX_LINKS - 1],
        &encode_chain(&chain),
    )
    .expect("admitted");
}

#[test]
fn a_capability_outside_the_root_rejects() {
    let (alice, bob) = (key(1), key(2));
    let elsewhere = MoqCapability::new("spaces/alicea").with_publish(pattern("**"));
    let link = Rcan::issuing_builder(&alice, bob.verifying_key(), elsewhere).sign(hours(1));
    let rejection = verify(&alice_verifier(), &bob, &encode_chain(&[link])).expect_err("rejected");
    assert!(rejection.reason().contains("outside"), "{rejection}");
}

#[test]
fn a_delegation_that_widens_its_capability_rejects() {
    let (alice, bob, phone) = (key(1), key(2), key(3));
    let issued =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    let widened = Rcan::delegating_builder(
        &bob,
        phone.verifying_key(),
        alice.verifying_key(),
        publish_under("carol"),
    )
    .sign(hours(1));
    verify(&alice_verifier(), &phone, &encode_chain(&[issued, widened])).expect_err("rejected");
}

#[test]
fn a_chain_from_another_principal_rejects() {
    let (eve, bob) = (key(8), key(2));
    let link =
        Rcan::issuing_builder(&eve, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    verify(&alice_verifier(), &bob, &encode_chain(&[link])).expect_err("rejected");
}

#[test]
fn the_earliest_link_sets_the_expiry() {
    let (alice, bob, phone) = (key(1), key(2), key(3));
    let issued = Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob"))
        .sign(Expires::At(4_000_000_000));
    let delegated = Rcan::delegating_builder(
        &bob,
        phone.verifying_key(),
        alice.verifying_key(),
        publish_under("bob"),
    )
    .sign(Expires::At(3_000_000_000));
    let authorization = verify(
        &alice_verifier(),
        &phone,
        &encode_chain(&[issued, delegated]),
    )
    .expect("admitted");
    assert_eq!(
        authorization.expires,
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(3_000_000_001))
    );
    let forever = Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob"))
        .sign(Expires::Never);
    let authorization =
        verify(&alice_verifier(), &bob, &encode_chain(&[forever])).expect("admitted");
    assert_eq!(authorization.expires, None);
}

#[test]
fn a_malformed_token_rejects() {
    let bob = key(2);
    for token in ["", "rcan1:", "rcan1:!!", "jwt", "rcan1:aaaa"] {
        verify(&alice_verifier(), &bob, token).expect_err(token);
    }
    let huge = format!("rcan1:{}", "a".repeat(64 * 1024));
    let rejection = verify(&alice_verifier(), &bob, &huge).expect_err("rejected");
    assert!(rejection.reason().contains("too large"), "{rejection}");
}

#[test]
fn a_token_survives_a_case_change() {
    let (alice, bob) = (key(1), key(2));
    let link =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    let token = encode_chain(&[link]);
    let upper = format!("rcan1:{}", token["rcan1:".len()..].to_ascii_uppercase());
    verify(&alice_verifier(), &bob, &upper).expect("admitted");
}

#[test]
fn a_far_future_link_expires_with_its_parent() {
    let (alice, bob, phone) = (key(1), key(2), key(3));
    let issued =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    let delegated = Rcan::delegating_builder(
        &bob,
        phone.verifying_key(),
        alice.verifying_key(),
        publish_under("bob/phone"),
    )
    .sign(Expires::At(u64::MAX - 1));
    let authorization = verify(
        &alice_verifier(),
        &phone,
        &encode_chain(&[issued, delegated]),
    )
    .expect("admitted");
    let expires = authorization.expires.expect("the parent's expiry");
    assert!(
        expires <= SystemTime::now() + Duration::from_secs(3601),
        "{expires:?}"
    );
}

#[test]
fn an_expired_chain_from_someone_else_is_unauthorized_not_expired() {
    let (mallory, bob) = (key(9), key(2));
    let link = Rcan::issuing_builder(&mallory, bob.verifying_key(), publish_under("bob"))
        .sign(Expires::At(1));
    let rejection = verify(&alice_verifier(), &bob, &encode_chain(&[link])).expect_err("rejected");
    assert!(
        matches!(rejection.code(), moq_net::Error::Unauthorized),
        "{:?}",
        rejection.code()
    );
}

#[test]
fn an_empty_chain_rejects() {
    let rejection = verify(&alice_verifier(), &key(2), &encode_chain(&[])).expect_err("rejected");
    assert!(rejection.reason().contains("empty"), "{rejection}");
    assert!(matches!(rejection.code(), moq_net::Error::Unauthorized));
}

#[test]
fn a_peer_id_that_is_no_ed25519_key_rejects() {
    let (alice, bob) = (key(1), key(2));
    let link =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    // Some 32-byte strings are no point on the curve.
    let bytes = (0..=u8::MAX)
        .map(|byte| [byte; 32])
        .find(|bytes| VerifyingKey::from_bytes(bytes).is_err())
        .expect("a byte string that is no key");
    let request = PeerRequest::new(Device(bytes), SessionRequest::default(), Event::Connect);
    let rejection = alice_verifier()
        .verify(&encode_chain(&[link]), &request)
        .expect_err("rejected");
    assert!(rejection.reason().contains("ed25519"), "{rejection}");
}

#[test]
fn a_forged_chain_rejects_as_unauthorized() {
    let (alice, bob, phone) = (key(1), key(2), key(3));
    let issued =
        Rcan::issuing_builder(&alice, bob.verifying_key(), publish_under("bob")).sign(hours(1));
    // Signed by the phone, which nobody delegated to.
    let forged = Rcan::delegating_builder(
        &phone,
        phone.verifying_key(),
        alice.verifying_key(),
        publish_under("bob"),
    )
    .sign(hours(1));
    let rejection =
        verify(&alice_verifier(), &phone, &encode_chain(&[issued, forged])).expect_err("rejected");
    assert!(
        matches!(rejection.code(), moq_net::Error::Unauthorized),
        "{rejection}"
    );
}

#[test]
fn the_root_matches_whole_segments() {
    assert!(within("spaces/alice", "spaces/alice"));
    assert!(within("spaces/alice/bob", "spaces/alice"));
    assert!(within("/spaces/alice/", "spaces/alice"));
    assert!(within("anything", ""));
    assert!(!within("spaces/alicea", "spaces/alice"));
    assert!(!within("spaces", "spaces/alice"));
}

#[test]
fn a_verifier_at_the_empty_root_admits_any_root() {
    let (alice, bob) = (key(1), key(2));
    let elsewhere = MoqCapability::new("fleet").with_publish(pattern("**"));
    let link = Rcan::issuing_builder(&alice, bob.verifying_key(), elsewhere).sign(hours(1));
    let verifier = RcanVerifier::new(alice.verifying_key(), "");
    let authorization = verify(&verifier, &bob, &encode_chain(&[link])).expect("admitted");
    assert!(authorization.grant.allows_publish("fleet/cam"));
}
