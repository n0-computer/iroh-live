//! Tokens: a device publishes into another node's space on a delegated rcan capability.

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use common::{Peer, eventually, publish, read_counter, step};
use ed25519_dalek::SigningKey;
use iroh::EndpointId;
use iroh_live_node::{
    Audience, ConnectOptions, Error, Grant, Node, NodeConfig, Reach,
    policy::{Authorization, Decision, Open, PeerRequest, Policy},
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
        from.node.connect_with(to.endpoint.addr(), options),
    )
    .await
    .map(drop)
}

/// Waits until `node` stores an authorization for `peer`, and returns it.
async fn admitted(node: &Peer, peer: &Peer) -> Authorization {
    eventually("the session registers", || {
        node.node.grant_for(peer.id()).is_some()
    })
    .await;
    node.node.grant_for(peer.id()).expect("an authorization")
}

#[tokio::test]
#[traced_test]
async fn a_delegated_device_publishes_into_the_space() {
    let alice = Peer::bare(alice_config()).await;
    let bob = Peer::spawn().await;
    let cam = publish(&bob.node, "spaces/alice/bob/cam", Audience::Granted).expect("publish");
    let elsewhere = publish(&bob.node, "spaces/carol/cam", Audience::Granted).expect("publish");

    connect_with_token(&bob, &alice, chain_to(&bob, Duration::from_secs(3600)))
        .await
        .expect("the chain admits bob's device");
    let grant = admitted(&alice, &bob).await.grant;
    assert!(grant.allows_publish("spaces/alice/bob/cam"));
    assert!(!grant.allows_publish("spaces/carol/cam"));

    let subscription = step(
        "alice reads bob's cam",
        alice.node.subscribe(cam.path(), Reach::Direct(bob.id())),
    )
    .await
    .expect("within the capability");
    read_counter(&subscription.broadcast()).await;
    let err = step(
        "outside the capability",
        alice
            .node
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
            eventually("alice drops mallory", || {
                alice.node.grant_for(mallory.id()).is_none()
                    && alice
                        .node
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

    eventually("the grant narrows to the fallback", || {
        alice.node.grant_for(bob.id()) == Some(Authorization::from(Grant::subscribe_only()))
    })
    .await;
    assert!(
        alice
            .node
            .sessions()
            .get()
            .iter()
            .any(|session| session.peer() == bob.id()),
        "narrowed, not closed"
    );

    alice.shutdown().await;
    bob.shutdown().await;
}

/// A device whose token arrives on a later session gets its delegated scope, and keeps it.
///
/// Alice dials the device first, so the stored authorization is the
/// token-less fallback. A later session without a token does not hide the
/// chain from a re-decide.
/// A policy that counts the decisions of the one it wraps.
#[derive(Debug)]
struct Counted<T> {
    inner: T,
    decisions: Arc<AtomicUsize>,
}

impl<T: Policy<EndpointId>> Policy<EndpointId> for Counted<T> {
    async fn decide(&self, request: &PeerRequest) -> Decision {
        let decision = self.inner.decide(request).await;
        self.decisions.fetch_add(1, Ordering::SeqCst);
        decision
    }
}

#[tokio::test]
#[traced_test]
async fn a_token_on_a_later_session_widens_the_device_and_survives_a_reauthorize() {
    let decisions = Arc::new(AtomicUsize::new(0));
    let verifier = RcanVerifier::new(alice_account().verifying_key(), "spaces/alice");
    let alice = Peer::bare(NodeConfig::default().with_policy(Counted {
        inner: Verify::new(verifier).fallback(Open(Grant::subscribe_only())),
        decisions: decisions.clone(),
    }))
    .await;
    let bob = Peer::spawn().await;
    step("alice dials bob", alice.node.connect(bob.endpoint.addr()))
        .await
        .expect("a dial carries no token, so the fallback admits");
    assert_eq!(
        admitted(&alice, &bob).await,
        Authorization::from(Grant::subscribe_only())
    );

    // Bob's device opens a session of its own with the chain.
    let tokened = Node::new(bob.endpoint.clone(), NodeConfig::default());
    let options = ConnectOptions {
        token: Some(chain_to(&bob, Duration::from_secs(3600))),
        ..Default::default()
    };
    step(
        "bob presents the chain",
        tokened.connect_with(alice.endpoint.addr(), options),
    )
    .await
    .expect("admitted");
    eventually("the chain widens bob", || {
        alice
            .node
            .grant_for(bob.id())
            .is_some_and(|current| current.grant.allows_publish("spaces/alice/bob/cam"))
    })
    .await;

    // A newer session without a token, then a re-decide.
    let plain = Node::new(bob.endpoint.clone(), NodeConfig::default());
    step(
        "bob dials without a token",
        plain.connect(alice.endpoint.addr()),
    )
    .await
    .expect("admitted");
    eventually("alice lists three sessions with bob", || {
        alice
            .node
            .sessions()
            .get()
            .iter()
            .filter(|session| session.peer() == bob.id())
            .count()
            == 3
    })
    .await;
    let before = decisions.load(Ordering::SeqCst);
    alice.node.reauthorize(bob.id());
    eventually("alice decides again", || {
        decisions.load(Ordering::SeqCst) > before
    })
    .await;
    assert!(
        alice
            .node
            .grant_for(bob.id())
            .is_some_and(|current| current.grant.allows_publish("spaces/alice/bob/cam")),
        "the re-decide verified the chain again"
    );

    tokened.shutdown().await;
    plain.shutdown().await;
    alice.shutdown().await;
    bob.shutdown().await;
}
