//! Ticket claims: widening a peer's grant to the one path a ticket names.

mod common;

use common::{Peer, ends, publish, reading, step};
use iroh_live_node::{Audience, Error, Grant, NodeConfig, Reach};
use moq_net::Path;
use n0_tracing_test::traced_test;

/// Returns a node whose config is the default: subscribe-only.
async fn viewer() -> Peer {
    Peer::bare(NodeConfig::default()).await
}

fn path_of(peer: &Peer, name: &str) -> Path {
    Path::new(&peer.path(name)).expect("path")
}

#[tokio::test]
#[traced_test]
async fn a_default_viewer_reads_a_claimed_path() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");

    let err = step(
        "refused",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect_err("the default grant lets alice publish nothing");
    assert!(matches!(err, Error::NotGranted { .. }), "{err:#}");

    let claim = bob
        .moq
        .claim_path(alice.id(), publication.path())
        .expect("claim");
    let subscription = step(
        "subscribe",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe with the claim");
    reading(&subscription.as_moq()).await;
    drop(claim);

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_claim_widens_a_session_bob_accepted() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let session = step("dial", alice.moq.connect(bob.endpoint.addr()))
        .await
        .expect("alice dials bob");
    assert!(session.dialed());

    let _claim = bob
        .moq
        .claim_path(alice.id(), publication.path())
        .expect("claim");
    let subscription = step(
        "subscribe",
        bob.moq
            .subscribe(publication.path(), Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe over the accepted session");
    reading(&subscription.as_moq()).await;
    assert!(!subscription.session().expect("a session").dialed());

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_forged_path_is_refused() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;

    for forged in [
        bob.path("cam"),
        format!("live/{}/{}/cam", alice.id(), bob.id()),
        "live/someone/cam".to_string(),
        format!("live/{}/{}", alice.id(), alice.id()),
    ] {
        let path = Path::new(&forged).expect("path");
        let err = bob
            .moq
            .claim_path(alice.id(), &path)
            .expect_err("alice does not own it");
        assert!(matches!(err, Error::NotOwner { .. }), "{forged}: {err:#}");
    }
    bob.moq
        .claim_path(bob.id(), &path_of(&alice, "cam"))
        .expect_err("the owner check applies to the node's own id too");

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn dropping_the_last_claim_narrows_the_grant_again() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let path = publication.path();

    let first = bob.moq.claim_path(alice.id(), path).expect("claim");
    let second = bob.moq.claim_path(alice.id(), path).expect("claim again");
    let subscription = step(
        "subscribe",
        bob.moq.subscribe(path, Reach::Direct(alice.id())),
    )
    .await
    .expect("subscribe");
    let mut track = reading(&subscription.as_moq()).await;
    let session = subscription.session().expect("a session");
    assert!(session.grant().allows_publish(path.as_str()));

    drop(first);
    assert!(
        session.grant().allows_publish(path.as_str()),
        "one claim still holds the path"
    );
    drop(second);
    assert!(!session.grant().allows_publish(path.as_str()));
    ends("the path left the grant", &mut track).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn set_grant_leaves_claimed_paths_alone() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let path = publication.path();
    let session = step("dial", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");

    let _claim = bob.moq.claim_path(alice.id(), path).expect("claim");
    session.set_grant(Grant::nothing());
    assert!(
        session.grant().allows_publish(path.as_str()),
        "a live claim survives a narrower base"
    );
    assert!(!session.grant().allows_publish("live/other"));

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_peer_holds_a_bounded_number_of_claims() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;

    let claims: Vec<_> = (0..64)
        .map(|n| {
            bob.moq
                .claim_path(alice.id(), &path_of(&alice, &format!("cam{n}")))
                .expect("within the limit")
        })
        .collect();
    let err = bob
        .moq
        .claim_path(alice.id(), &path_of(&alice, "one-too-many"))
        .expect_err("over the limit");
    assert!(matches!(err, Error::TooManyGrants { .. }), "{err:#}");
    drop(claims);
    bob.moq
        .claim_path(alice.id(), &path_of(&alice, "one-too-many"))
        .expect("room again");

    alice.shutdown().await;
    bob.shutdown().await;
}
