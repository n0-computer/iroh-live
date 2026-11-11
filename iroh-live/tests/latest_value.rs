//! A latest-value JSON track between two nodes, over a real QUIC connection.

use std::time::Duration;

use iroh::protocol::Router;
use iroh_live::{Audience, Node, Reach, json::snapshot, layout::LiveLayout, net::track};
use iroh_live_node::test_util::endpoint;
use n0_tracing_test::traced_test;

const TIMEOUT: Duration = Duration::from_secs(30);

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
        .publish_with(path.clone(), Audience::Granted, |producer| {
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
        .broadcast()
        .track("latency")
        .expect("the track");
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
