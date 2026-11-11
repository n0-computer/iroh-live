//! Tokens: a device publishes into another node's space on a delegated rcan capability.

mod common;

use std::time::Duration;

use common::{Peer, publish, read_counter, step};
use ed25519_dalek::SigningKey;
use iroh_live_node::{
    Audience, ConnectOptions, Error, Grant, NodeConfig, Reach,
    policy::{Authorization, Open},
};
use moq_ext_access::policy::{MoqCapability, RcanVerifier, Verify, encode_chain};
use n0_tracing_test::traced_test;
use n0_watcher::Watcher;
use rcan::{Expires, Rcan};

/// The key of the account whose space alice's node shares.
fn alice_account() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

/// The key of bob's account, which delegates to bob's device.
fn bob_account() -> SigningKey {
    SigningKey::from_bytes(&[2; 32])
}

/// Returns a node configuration that verifies chains of alice's account.
///
/// Peers without a token may subscribe.
fn alice_config() -> NodeConfig {
    let verifier = RcanVerifier::new(alice_account().verifying_key(), "spaces/alice");
    NodeConfig::default().with_policy(Verify::new(verifier).fallback(Open(Grant::subscribe_only())))
}

/// Returns a chain from alice's account through bob's to `device`, valid for `valid`.
fn chain_to(device: &Peer, valid: Duration) -> String {
    let capability = MoqCapability::new("spaces/alice")
        .with_publish("**".parse::<moq_net::Pattern>().expect("a pattern"));
    let issued = Rcan::issuing_builder(
        &alice_account(),
        bob_account().verifying_key(),
        capability.clone(),
    )
    .sign(Expires::valid_for(Duration::from_secs(3600)));
    let delegated = Rcan::delegating_builder(
        &bob_account(),
        device.id().as_verifying_key(),
        alice_account().verifying_key(),
        capability,
    )
    .sign(Expires::valid_for(valid));
    encode_chain(&[issued, delegated])
}

/// Connects `from` to `to` presenting `token`.
async fn connect_with_token(from: &Peer, to: &Peer, token: String) -> Result<(), Error> {
    let options = ConnectOptions {
        token: Some(token),
        ..Default::default()
    };
    step(
        "connect",
        from.moq.connect_with(to.endpoint.addr(), options),
    )
    .await
    .map(drop)
}

/// Waits until `node` stores an authorization for `peer`, and returns it.
async fn admitted(node: &Peer, peer: &Peer) -> Authorization {
    let mut authorization = None;
    step("the session registers", async {
        loop {
            authorization = node.moq.grant_for(peer.id());
            if authorization.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    authorization.expect("an authorization")
}

/// Waits until `condition` holds, polling, within the test timeout.
async fn until(what: &str, condition: impl Fn() -> bool) {
    step(what, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test]
#[traced_test]
async fn a_delegated_device_publishes_into_the_space() {
    let alice = Peer::bare(alice_config()).await;
    let bob = Peer::spawn().await;
    let cam = publish(&bob.moq, "spaces/alice/bob/cam", Audience::Granted).expect("publish");
    let elsewhere = publish(&bob.moq, "spaces/carol/cam", Audience::Granted).expect("publish");

    connect_with_token(&bob, &alice, chain_to(&bob, Duration::from_secs(3600)))
        .await
        .expect("the chain admits bob's device");
    let grant = admitted(&alice, &bob).await.grant;
    assert!(grant.allows_publish("spaces/alice/bob/cam"));
    assert!(!grant.allows_publish("spaces/carol/cam"));

    let subscription = step(
        "alice reads bob's cam",
        alice.moq.subscribe(cam.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect("within the capability");
    read_counter(&subscription.broadcast()).await;
    let err = step(
        "outside the capability",
        alice
            .moq
            .subscribe(elsewhere.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect_err("resolved a path outside the capability");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_chain_presented_by_another_device_is_refused() {
    let alice = Peer::bare(alice_config()).await;
    let bob = Peer::spawn().await;
    let mallory = Peer::spawn().await;
    match connect_with_token(&mallory, &alice, chain_to(&bob, Duration::from_secs(3600))).await {
        Err(err) => assert!(matches!(err, Error::Refused { .. }), "{err:#}"),
        Ok(()) => {
            until("alice drops mallory", || {
                alice.moq.grant_for(mallory.id()).is_none()
                    && alice
                        .moq
                        .sessions()
                        .get()
                        .iter()
                        .all(|session| session.peer() != mallory.id())
            })
            .await;
        }
    }
    alice.shutdown().await;
    bob.shutdown().await;
    mallory.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn an_expired_delegation_narrows_the_device_to_the_fallback() {
    let alice = Peer::bare(alice_config()).await;
    let bob = Peer::spawn().await;
    connect_with_token(&bob, &alice, chain_to(&bob, Duration::from_secs(2)))
        .await
        .expect("admitted while the chain is valid");
    let authorization = admitted(&alice, &bob).await;
    assert!(authorization.grant.allows_publish("spaces/alice/bob/cam"));
    assert!(
        authorization.expires.is_some(),
        "the chain's expiry is kept"
    );

    until("the grant narrows to the fallback", || {
        alice.moq.grant_for(bob.id()) == Some(Authorization::from(Grant::subscribe_only()))
    })
    .await;
    assert!(
        alice
            .moq
            .sessions()
            .get()
            .iter()
            .any(|session| session.peer() == bob.id()),
        "narrowed, not closed"
    );

    alice.shutdown().await;
    bob.shutdown().await;
}
