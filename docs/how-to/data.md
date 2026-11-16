# Send data without media

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
    moq_net::{Timescale, Timestamp, time::Instant, track},
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
                let now = Timestamp::from(Instant::now());
                control.write_frame(now, format!("tick {n}").into_bytes())?;
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
            while let Some(item) = control.next_group().await? {
                let mut group = match item {
                    track::Item::Group(group) => group,
                    track::Item::Gap { range, .. } => {
                        println!("skipped groups {range:?}");
                        continue;
                    }
                };
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

Frames are bytes, so the format is yours. `ordered()` delivers groups in
sequence. A reader that falls behind still skips to the newest group and
reports the groups it gave up on as a gap, so put a self-contained message in
each group when a reader should never wait for old ones.

## Latest-value state

Some state needs only its newest value, such as a setting or a status. For
it, `Node::publish_value` publishes a latest-value JSON track of any
`serde` type, and `Subscription::value_track` reads it. `latest()` skips to
the newest value, and a late reader gets the current one at once. The reader
follows the subscription, so it reads on after the publisher restarts. The
track counts milliseconds, and the `ValueWriter` that `publish_value` returns
stamps each update with the time it is made, so two updates in the same
millisecond both succeed. `update_at` takes a time of your choice instead. Clones of the
writer share the track, and the last one to drop finishes it:

```rust,no_run
use iroh_live::{Audience, Node, Publication, Subscription, ValueReader};

fn publish_latency(node: &Node, path: &str) -> Result<Publication, Box<dyn std::error::Error>> {
    let (publication, latency) =
        node.publish_value::<u64>(path, Audience::Granted, "latency")?;
    latency.update(&150)?;
    latency.update(&120)?;
    // The track finishes when `latency` drops here, and the broadcast is
    // unpublished when `publication` drops.
    Ok(publication)
}

async fn read_latency(subscription: &Subscription) -> Result<(), Box<dyn std::error::Error>> {
    let mut latency: ValueReader<u64> = subscription.value_track("latency")?;
    while let Some(timed) = latency.latest().await? {
        println!("latency {} ms", timed.value);
    }
    Ok(())
}
```

`LocalBroadcast::value_track` puts such a track next to the video and audio,
and also returns a `ValueWriter`. Create it in the fill closure of
`Node::publish_with`, so it exists before anyone can ask for it.

`iroh_live::json` re-exports moq-json for other typed JSON tracks.

## Without the media crates

`LiveLayout` and the media crates are conveniences over `Node`. A program
with its own path layout or access rules, or with no media at all, uses
`Node` from `iroh_live::node` (the `iroh-live-node` crate). It gets the same
sessions, route table and relay links.

### Read a broadcast without media

The default `NodeConfig` lets peers read anything and publish nothing, so
this reader grants the publisher `demo/**`. The publishing side is
[`iroh-live/examples/moq_only.rs`](../../iroh-live/examples/moq_only.rs),
which serves `demo/hello`:

```sh
cargo run -p iroh-live --example moq_only     # prints its endpoint id
```

```rust,no_run
use iroh::EndpointId;
use iroh_live::node::{Grant, Node, NodeConfig, MoqPreset, Reach, moq_net::Pattern, policy};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let publisher: EndpointId = std::env::args()
        .nth(1)
        .ok_or("usage: reader <endpoint id>")?
        .parse()?;
    let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
    let demo: Pattern = "demo/**".parse()?;
    let node = Node::new(
        endpoint.clone(),
        NodeConfig::default().with_policy(policy::Open(Grant::publish_under(demo))),
    );
    // Mount your own protocols on the same router.
    let router = node.mount(iroh::protocol::Router::builder(endpoint)).spawn();

    let subscription = node
        .subscribe("demo/hello", Reach::Direct(publisher))
        .await?;
    println!("subscribed to {}", subscription.path());
    if let Some(link) = subscription.serving_link() {
        println!(
            "served by a {:?} link, rtt {:?}",
            link.kind, link.sample.rtt
        );
    }

    // Shuts down the node and closes the endpoint.
    router.shutdown().await?;
    Ok(())
}
```

`serving_link()` returns the link that serves the subscription now, with a
`NetworkSample` of its round trip, loss, and the rates bytes arrive and leave
at. `subscription.session()` returns the direct session that serves it now,
whose `connection().paths()` lists the network paths. Ask the subscription
each time instead of keeping the session from the dial: when the publisher
moves to another link, the subscription follows it.

### The rest of the node

- `Node::sessions()` watches the open sessions, and `Session::connection()`
  is the iroh connection, whose `paths()` lists the network paths to the
  peer.
- `Session::sample()` returns the session's latest `NetworkSample`, and
  `Session::moq()` is the moq-net session itself.
- `Node::accepted()` streams the sessions the node accepts.
- A `policy::Queue` lets the application admit each peer itself, see
  [Control access](control-access.md#ask-the-user).

## Where to go next

- [The node and its transport](../architecture/transport.md) explains the
  node in depth.
- [Broadcasts, tracks and renditions](../concepts/broadcasts.md)
