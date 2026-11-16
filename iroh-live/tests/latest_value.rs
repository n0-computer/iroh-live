//! A latest-value JSON track between two nodes, over a real QUIC connection.

use iroh::protocol::Router;
use iroh_live::{
    Audience, Node, Reach, ValueWriter,
    json::snapshot,
    layout::LiveLayout,
    moq_net::{Timescale, Timestamp, track},
};
use iroh_live_node::test_util::{endpoint, step};
use n0_tracing_test::traced_test;

/// A reader that subscribes after the writes gets the newest value, then each later one.
#[tokio::test]
#[traced_test]
async fn a_late_reader_gets_the_newest_value_and_then_the_next() {
    let publisher = Node::new(endpoint().await, LiveLayout::default().config());
    let _router = publisher
        .mount(Router::builder(publisher.endpoint().clone()))
        .spawn();
    let path = LiveLayout::default()
        .path(publisher.id(), "control")
        .expect("a path");
    let (_publication, track) = publisher
        .publish_with(path.clone(), Audience::Granted, |producer| {
            producer.create_track(
                "latency",
                track::Info::default().with_timescale(Timescale::MILLI),
            )
        })
        .expect("publish");
    // Fixed times, so the reader's stamps can be checked.
    let writer = ValueWriter::<u64>::new(track, snapshot::Config::default());
    for (at, latency) in [100, 150, 200].into_iter().enumerate() {
        writer
            .update_at(&latency, millis(at as u64))
            .expect("write");
    }

    let subscriber = Node::new(endpoint().await, LiveLayout::default().config());
    let subscription = step(
        "subscribed in time",
        subscriber.subscribe(&path, Reach::Direct(publisher.id())),
    )
    .await
    .expect("subscribe");
    let track = subscription
        .broadcast()
        .track("latency")
        .expect("the track");
    let mut reader = snapshot::Consumer::<u64>::new(track, Default::default());
    let mut read = async || {
        let newest = step("a value in time", reader.latest())
            .await
            .expect("a readable track")
            .expect("the track is open");
        (newest.value, newest.at)
    };
    assert_eq!(
        read().await,
        (200, millis(2)),
        "the newest value, not an older one"
    );
    writer.update_at(&80, millis(3)).expect("write");
    assert_eq!(read().await, (80, millis(3)));
    // A stamped update lands after the fixed times.
    writer.update(&90).expect("write");
    let (value, at) = read().await;
    assert_eq!(value, 90);
    assert!(at > millis(3), "stamped {at:?}, not after the last write");

    subscriber.shutdown().await;
    publisher.shutdown().await;
}

/// Returns millisecond `at` as a timestamp.
fn millis(at: u64) -> Timestamp {
    Timestamp::from_millis(at).expect("in range")
}
