//! Ticket claims and ticket subscriptions: a peer's grant, widened to one path.

mod common;

use std::{sync::Arc, time::Duration};

use common::{Peer, announced, ends, publish, reading, stays_pending, step};
use iroh_live_node::{Audience, Error, Grant, NodeConfig, Reach};
use moq_net::Path;
use n0_tracing_test::traced_test;

/// Returns a node whose grant lets every peer subscribe and publish nothing.
async fn viewer() -> Peer {
    Peer::bare(NodeConfig {
        grant: Some(Arc::new(|_| Grant::subscribe_only())),
        ..Default::default()
    })
    .await
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
    reading(&subscription.broadcast()).await;
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
    // The live session applies the wider grant a moment after the claim, so a
    // read that starts at once can still be refused. Retry until it is let in.
    let deadline = tokio::time::Instant::now() + common::TIMEOUT;
    let subscription = loop {
        let subscription = step(
            "subscribe",
            bob.moq
                .subscribe(publication.path(), Reach::Direct(alice.id())),
        )
        .await
        .expect("subscribe over the accepted session");
        let track = subscription
            .broadcast()
            .track("video")
            .expect("track")
            .subscribe(common::subscription())
            .await;
        if let Ok(mut track) = track
            && matches!(track.recv_group().await, Ok(Some(_)))
        {
            break subscription;
        }
        assert!(tokio::time::Instant::now() < deadline, "never let in");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
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
        format!(
            "live/{}/cam",
            data_encoding::BASE32_NOPAD
                .encode(alice.id().as_bytes())
                .to_ascii_lowercase()
        ),
        format!("live/{}/{}", alice.id(), alice.id()),
    ] {
        let path = Path::new(&forged).expect("path");
        let err = bob
            .moq
            .claim_path(alice.id(), &path)
            .expect_err("alice does not own it");
        assert!(matches!(err, Error::NotOwner { .. }), "{forged}: {err:#}");
    }

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
    let mut track = reading(&subscription.broadcast()).await;
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
    assert_eq!(session.base_grant(), Grant::nothing());
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

#[tokio::test]
#[traced_test]
async fn a_claim_stays_on_its_peers_links() {
    let alice = Peer::spawn().await;
    let carol = Peer::spawn().await;
    let bob = viewer().await;
    let with_alice = step("dial", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let with_carol = step("dial", bob.moq.connect(carol.endpoint.addr()))
        .await
        .expect("connect");

    let path = path_of(&alice, "cam");
    let _claim = bob.moq.claim_path(alice.id(), &path).expect("claim");
    assert!(with_alice.grant().allows_publish(path.as_str()));
    assert_eq!(with_carol.grant(), with_carol.base_grant());

    alice.shutdown().await;
    carol.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn concurrent_claims_all_land() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let session = step("dial", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");

    let tasks: Vec<_> = (0..8)
        .map(|n| {
            let node = bob.moq.clone();
            let (peer, path) = (alice.id(), path_of(&alice, &format!("cam{n}")));
            tokio::spawn(async move { node.claim_path(peer, &path).expect("claim") })
        })
        .collect();
    let mut claims = Vec::new();
    for task in tasks {
        claims.push(task.await.expect("task"));
    }
    for n in 0..8 {
        let path = path_of(&alice, &format!("cam{n}"));
        assert!(session.grant().allows_publish(path.as_str()), "cam{n}");
    }
    drop(claims);
    assert_eq!(session.grant(), session.base_grant());

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn claims_are_capped_by_bytes_and_repeats_are_free() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;

    let long = |n: usize| {
        let name = format!("{n}{}", "x".repeat(3000));
        path_of(&alice, &name)
    };
    let held: Vec<_> = (0..5)
        .map(|n| bob.moq.claim_path(alice.id(), &long(n)).expect("room"))
        .collect();
    let err = bob
        .moq
        .claim_path(alice.id(), &long(5))
        .expect_err("over 16 KiB");
    assert!(matches!(err, Error::TooManyGrants { .. }), "{err:#}");
    bob.moq
        .claim_path(alice.id(), &long(0))
        .expect("a held path repeats freely");
    drop(held);

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn an_owner_function_replaces_the_generic_rule() {
    let alice = Peer::spawn().await;
    let alice_id = alice.id();
    let bob = Peer::bare(NodeConfig {
        grant: Some(Arc::new(|_| Grant::nothing())),
        owner: Some(Arc::new(move |path: &Path| {
            (path.as_str() == "fleet/cam").then_some(alice_id)
        })),
        ..Default::default()
    })
    .await;

    bob.moq
        .claim_path(alice.id(), &Path::new("fleet/cam").expect("path"))
        .expect("the owner function admits it");
    let err = bob
        .moq
        .claim_path(alice.id(), &path_of(&alice, "cam"))
        .expect_err("the generic rule no longer applies");
    assert!(matches!(err, Error::NotOwner { .. }), "{err:#}");

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_forged_claim_reaches_nothing() {
    let alice = Peer::spawn().await;
    let mallory = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let mut announces = bob.moq.origin().announced();

    bob.moq
        .claim_path(mallory.id(), publication.path())
        .expect_err("alice's path is not mallory's");
    let session = step("dial", mallory.moq.connect(bob.endpoint.addr()))
        .await
        .expect("mallory dials bob");
    let forged = publish(&mallory.moq, alice.path("cam"), Audience::Everyone);
    drop(forged);
    stays_pending(
        "nothing reaches bob's table",
        Duration::from_millis(500),
        announced(&mut announces, publication.path().as_str()),
    )
    .await;
    assert_eq!(session.grant(), session.base_grant());

    alice.shutdown().await;
    mallory.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_default_viewer_subscribes_to_a_ticket() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let ticket = publication.ticket();
    assert_eq!(ticket.peer(), alice.id());

    let subscription = step("subscribe", bob.moq.subscribe_ticket(&ticket))
        .await
        .expect("a ticket subscribe needs no grant");
    reading(&subscription.broadcast()).await;

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_ticket_subscription_releases_its_claim_on_drop() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");
    let subscription = step("subscribe", bob.moq.subscribe_ticket(&publication.ticket()))
        .await
        .expect("subscribe");
    let session = subscription.session().expect("a session");
    let clone = subscription.clone();
    assert_ne!(session.grant(), session.base_grant());

    drop(subscription);
    assert_ne!(session.grant(), session.base_grant(), "a clone holds it");
    drop(clone);
    assert_eq!(session.grant(), session.base_grant());

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_cancelled_ticket_subscribe_releases_its_claim() {
    let alice = Peer::spawn().await;
    let bob = viewer().await;
    let session = step("dial", bob.moq.connect(alice.endpoint.addr()))
        .await
        .expect("connect");
    let ticket = iroh_live_node::BroadcastTicket::at(alice.id(), path_of(&alice, "never"));

    let waited = tokio::time::timeout(
        Duration::from_millis(500),
        bob.moq.subscribe_ticket(&ticket),
    )
    .await;
    assert!(waited.is_err(), "nothing is published at the path");
    assert_eq!(session.grant(), session.base_grant());

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_covered_ticket_claims_nothing() {
    let alice = Peer::spawn().await;
    let bob = Peer::spawn().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");

    let subscription = step("subscribe", bob.moq.subscribe_ticket(&publication.ticket()))
        .await
        .expect("subscribe");
    let session = subscription.session().expect("a session");
    assert_eq!(session.grant(), session.base_grant());

    alice.shutdown().await;
    bob.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn an_own_ticket_resolves_locally() {
    let alice = Peer::spawn().await;
    let publication = publish(&alice.moq, alice.path("cam"), Audience::Everyone).expect("publish");

    let subscription = step(
        "subscribe",
        alice.moq.subscribe_ticket(&publication.ticket()),
    )
    .await
    .expect("subscribe");
    assert!(subscription.session().is_none(), "no session with oneself");
    reading(&subscription.broadcast()).await;

    alice.shutdown().await;
}

#[tokio::test]
#[traced_test]
async fn a_forged_ticket_fails_not_owner() {
    let alice = Peer::spawn().await;
    let mallory = Peer::spawn().await;
    let bob = viewer().await;
    let forged = iroh_live_node::BroadcastTicket::at(mallory.id(), path_of(&alice, "cam"));

    let err = step("subscribe", bob.moq.subscribe_ticket(&forged))
        .await
        .expect_err("mallory does not own the path");
    assert!(matches!(err, Error::NotOwner { .. }), "{err:#}");

    alice.shutdown().await;
    mallory.shutdown().await;
    bob.shutdown().await;
}
