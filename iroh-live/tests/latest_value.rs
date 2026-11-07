//! A latest-value JSON track between two nodes, over a real QUIC connection.

use std::{sync::OnceLock, time::Duration};

use iroh::{Endpoint, address_lookup::MemoryLookup, endpoint::presets, protocol::Router};
use iroh_live::layout::LiveLayout;
use iroh_live::{Audience, Node, Reach, json::snapshot, net::track};
use n0_tracing_test::traced_test;

const TIMEOUT: Duration = Duration::from_secs(30);

async fn endpoint() -> Endpoint {
    static LOOKUP: OnceLock<MemoryLookup> = OnceLock::new();
    let lookup = LOOKUP.get_or_init(MemoryLookup::new);
    let endpoint = Endpoint::builder(presets::Minimal)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("failed to bind endpoint");
    lookup.add_endpoint_info(endpoint.addr());
    endpoint
}

/// A reader that subscribes after the writes gets the newest value, then each later one.
#[tokio::test]
#[traced_test]
async fn a_late_reader_gets_the_newest_value_and_then_the_next() {
    let publisher = Node::new(endpoint().await, LiveLayout::default().config(&[]));
    let _router = publisher
        .mount(Router::builder(publisher.endpoint().clone()))
        .spawn();
    let path = LiveLayout::default()
        .path(publisher.id(), "control")
        .expect("a path");
    let (_publication, track) = publisher
        .publish_with(path.clone(), Audience::Everyone, |producer| {
            producer.create_track("latency", track::Info::default())
        })
        .expect("publish");
    let mut writer = snapshot::Producer::<u64>::new(track, snapshot::Config::default());
    for latency in [100, 150, 200] {
        writer.update(&latency).expect("write");
    }

    let subscriber = Node::new(endpoint().await, LiveLayout::default().config(&[]));
    let subscription = tokio::time::timeout(
        TIMEOUT,
        subscriber.subscribe(&path, Reach::Direct(publisher.id())),
    )
    .await
    .expect("subscribed in time")
    .expect("subscribe");
    let track = subscription
        .as_moq()
        .track("latency")
        .expect("the track")
        .subscribe(None)
        .await
        .expect("track subscription");
    let mut reader = snapshot::Consumer::<u64>::new(track, Default::default());
    let mut read = async || {
        tokio::time::timeout(TIMEOUT, reader.latest())
            .await
            .expect("a value in time")
            .expect("a readable track")
            .expect("the track is open")
            .value
    };
    assert_eq!(read().await, 200, "the newest value, not an older one");
    writer.update(&80).expect("write");
    assert_eq!(read().await, 80);

    subscriber.shutdown().await;
    publisher.shutdown().await;
}
