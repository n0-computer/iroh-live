# Send data and state

A broadcast is a set of tracks, and a track is a stream of groups of frames.
Nothing requires them to be video, so a node can carry control messages,
telemetry or application state over the same sessions, routes and access
rules as its media.

## A control channel

This program publishes a broadcast with one track that ticks every second.
Run it with no argument to serve, and with the printed id to read. It needs no
media feature.

```rust,no_run
use std::time::Duration;

use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    Audience, EndpointConfig, Node, Reach,
    layout::LiveLayout,
    moq_net::{Timescale, Timestamp, track},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let layout = LiveLayout::default();
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let node = Node::new(endpoint.clone(), layout.config());
    let router = node.mount(Router::builder(endpoint)).spawn();

    match std::env::args().nth(1) {
        None => {
            let me = node.id();
            println!("read with: {me}");
            let path = layout.path(me, "control")?;
            let publication = node.publish(path, Audience::Granted)?;
            let mut control = publication.broadcast().create_track(
                "control",
                track::Info::default().with_timescale(Timescale::MILLI),
            )?;
            for n in 0u32.. {
                control.write_frame(Timestamp::now(), format!("tick {n}").into_bytes())?;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        Some(id) => {
            let peer: EndpointId = id.parse()?;
            let path = layout.path(peer, "control")?;
            let subscription = node.subscribe(path, Reach::Direct(peer)).await?;
            let mut control = subscription
                .broadcast()
                .track("control")?
                .subscribe(None)
                .await?
                .ordered();
            while let Some(mut group) = control.next_group().await? {
                while let Some(frame) = group.read_frame().await? {
                    println!("{}", String::from_utf8_lossy(&frame.payload));
                }
            }
        }
    }
    router.shutdown().await?;
    Ok(())
}
```

Frames are bytes, so the format is yours. A reader that falls behind skips to
the newest group, so put a self-contained message in each group when a reader
should never wait for old ones.

## Latest-value state

Some state needs only its newest value, such as a setting or a status. For
it, `Node::publish_value` publishes a latest-value JSON track of any
`serde` type, and `Subscription::value_track` reads it. `latest()` skips to
the newest value, and a late reader gets the current one at once:

```rust,no_run
use iroh_live::{Audience, Node, Publication, Subscription, json::snapshot};

fn publish_latency(node: &Node, path: &str) -> Result<Publication, Box<dyn std::error::Error>> {
    let (publication, mut latency) =
        node.publish_value::<u64>(path, Audience::Granted, "latency")?;
    latency.update(&150)?;
    // Unpublished when `publication` drops.
    Ok(publication)
}

async fn read_latency(subscription: &Subscription) -> Result<(), Box<dyn std::error::Error>> {
    let mut latency: snapshot::Consumer<u64> = subscription.value_track("latency")?;
    while let Some(timed) = latency.latest().await? {
        println!("latency {} ms", timed.value);
    }
    Ok(())
}
```

`LocalBroadcast::value_track` puts such a track next to the video and audio.
Create it in the fill closure of `Node::publish_with`, so it exists before
anyone can ask for it. `iroh_live::json` re-exports moq-json for other typed
JSON tracks.

## Where to go next

- [Use the node without media](node-only.md)
- [Broadcasts, tracks and renditions](../concepts/broadcasts.md)
