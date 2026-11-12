//! A device publishes into another account's space on a delegated rcan capability.
//!
//! Alice's node shares the space `spaces/alice`. Its policy verifies rcan
//! capability chains that alice's account key issued, and lets peers without a
//! token subscribe only. Alice's account delegates publishing under
//! `spaces/alice/bob/**` to bob's account, and bob delegates it on to his
//! device. The device presents the chain as its token when it dials alice's
//! node, and alice's node then reads the broadcast the device publishes there.
//!
//! The chain names the device's endpoint id, so a peer that copies the token
//! gains nothing without the device's key. The authorization expires with the
//! chain's earliest link, and the device then falls back to subscribe-only.
//!
//! Run it with `cargo run -p iroh-live-node --features rcan --example delegated_publisher`.

use std::time::Duration;

use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_live_node::{
    Audience, ConnectOptions, Grant, MoqPreset, Node, NodeConfig, Reach,
    moq_net::Pattern,
    policy::{MoqCapability, Open, RcanVerifier, Verify, encode_chain},
};
use rcan::{Expires, Rcan};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The account keys. A real app keeps them, apart from any node key.
    let alice_account = SecretKey::generate();
    let bob_account = SecretKey::generate();

    // Alice's node: a valid chain from alice's account decides, and a peer
    // without a token may subscribe.
    let verifier = RcanVerifier::new(alice_account.public().as_verifying_key(), "spaces/alice");
    let policy = Verify::new(verifier).with_fallback(Open(Grant::subscribe_only()));
    let alice_endpoint = Endpoint::bind(MoqPreset).await?;
    let alice = Node::new(
        alice_endpoint.clone(),
        NodeConfig::default().with_policy(policy),
    );
    let alice_router = alice.mount(Router::builder(alice_endpoint.clone())).spawn();

    // Bob's device: a node with the default policy, on a key of its own.
    let device_endpoint = Endpoint::bind(MoqPreset).await?;
    let device = Node::new(device_endpoint.clone(), NodeConfig::default());

    // Alice's account lets bob's account publish under `spaces/alice/bob/`
    // for a day, and bob's account delegates that to the device for an hour.
    // The patterns are relative to the capability's root.
    let capability = MoqCapability::new("spaces/alice").with_publish("bob/**".parse::<Pattern>()?);
    let issued = Rcan::issuing_builder(
        alice_account.as_signing_key(),
        bob_account.public().as_verifying_key(),
        capability.clone(),
    )
    .sign(Expires::valid_for(Duration::from_secs(24 * 3600)));
    let delegated = Rcan::delegating_builder(
        bob_account.as_signing_key(),
        device.id().as_verifying_key(),
        alice_account.public().as_verifying_key(),
        capability,
    )
    .sign(Expires::valid_for(Duration::from_secs(3600)));
    let token = encode_chain(&[issued, delegated]);

    // The device publishes in alice's space and dials alice with the chain.
    let publication = device.publish("spaces/alice/bob/cam", Audience::Granted)?;
    let session = device
        .connect_with(
            alice_endpoint.addr(),
            ConnectOptions {
                token: Some(token),
                ..Default::default()
            },
        )
        .await?;
    println!("device connected to {}", session.peer().fmt_short());

    // Alice's node admitted the device with the chain's capability. It may
    // take a moment for the session to register on alice's side.
    let authorization = loop {
        if let Some(authorization) = alice.grant_for(device.id()) {
            break authorization;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    println!(
        "alice lets the device publish {:?} until {:?}",
        authorization.grant.publish, authorization.expires
    );

    // Alice reads the device's broadcast over the session it opened.
    let subscription = tokio::time::timeout(
        Duration::from_secs(10),
        alice.subscribe(publication.path(), Reach::Direct(device.id())),
    )
    .await??;
    println!("alice reads {}", subscription.path());

    device.shutdown().await;
    device_endpoint.close().await;
    alice_router.shutdown().await?;
    Ok(())
}
