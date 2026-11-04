# Using iroh-live

iroh-live gives you a node that publishes live audio and video to other iroh
nodes and plays theirs. Broadcasts are named by path, found through a route
table, and carried over [Media over QUIC](https://moq.dev/). Direct sessions
and relays both feed the table, so a viewer does not care which one serves it.

Most programs need one crate, `iroh-live`, with the features you want:

```toml
iroh-live = { version = "0.1", features = ["full"] }  # add "rooms" for rooms
```

| You want | Use | Crate |
|---|---|---|
| Publish, watch, call, rooms | `Live`, `LocalBroadcast`, `RemoteBroadcast` | `iroh-live` |
| Your own paths, audiences or grants | `Live::moq()`, `Moq` | `iroh-moq` |
| Several participants | `Rooms`, `Room` | `iroh-live-rooms` |
| Encode sources, decode and play | `LocalBroadcast`, `Player` | `moq-ext-publisher`, `moq-ext-player` |
| Browsers, or peers behind a relay | `Moq::attach_relay`, the relay binary | `iroh-live-relay` |

`iroh-live` re-exports the rest: `iroh_live::media` has the media crates,
`iroh_live::moq` is `iroh-moq`, and `iroh_live::rooms` is `iroh-live-rooms`
behind the `rooms` feature. The examples below are complete programs. Each one
needs `tokio` with the `full` feature.

## Publish a test pattern

A node binds an endpoint, mounts the transport on a router, and publishes a
broadcast under a name. The ticket names that broadcast for viewers. The
generated sources need no camera or microphone.

```rust
use iroh_live::{
    EndpointOptions, Live,
    media::{
        AudioEncoding, AudioSource, LocalBroadcast, VideoEncoding, VideoRendition, VideoSource,
        audio::Layout, video,
    },
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let live = Live::builder(EndpointOptions::default().bind().await?)
        .with_router()
        .spawn();

    let broadcast = LocalBroadcast::new();
    let size = video::Size::new(1280, 720);
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(size, rate),
        VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
    )?;
    broadcast.set_audio(
        AudioSource::test_pattern(Layout::Mono),
        AudioEncoding::voice(),
    )?;

    live.publish("pattern", &broadcast)?;
    println!("{}", live.ticket("pattern"));

    tokio::signal::ctrl_c().await?;
    live.shutdown().await;
    Ok(())
}
```

The broadcast is at `live/<endpoint id>/pattern`, and a rendition encodes only
while someone watches it. For a camera and microphone, see
[`iroh-live/examples/publish.rs`](../../iroh-live/examples/publish.rs).

## Watch a ticket

`subscribe` resolves the ticket's path, dialing the publisher if needed.
`remote_broadcast` reads the media, and `play` starts a player that picks a
rendition as the link allows. A player with no `audio` target does not
subscribe to audio.

```rust
use std::time::Duration;

use iroh_live::{BroadcastTicket, EndpointOptions, Live, media::PlayerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ticket: BroadcastTicket = std::env::args()
        .nth(1)
        .ok_or("usage: watch <ticket>")?
        .parse()?;
    let live = Live::builder(EndpointOptions::default().bind().await?).spawn();

    let subscription = live.subscribe(&ticket).await?;
    let player = live
        .remote_broadcast(&subscription)
        .play(PlayerConfig::default())?;

    let mut frames = player.video();
    for count in 1..=100 {
        let frame = tokio::time::timeout(Duration::from_secs(10), frames.next())
            .await?
            .ok_or("the video ended")?;
        println!("frame {count}: {}", frame.size());
    }
    live.shutdown().await;
    Ok(())
}
```

Hand each frame to a renderer instead of counting it: see
[Desktop rendering](desktop.md). Without a router the node only dials, which is
enough for a viewer.

## A one-to-one call

Each side publishes its own broadcast to the other peer only, and subscribes to
the other's. `call` offers at `live/<id>/call` to that peer and no one else, and
`answer` waits until the peer offers theirs. Both programs run the same code with
the other's endpoint id, which each prints on start.

```rust
use iroh::EndpointId;
use iroh_live::{
    EndpointOptions, Live,
    media::{
        AudioEncoding, AudioSource, LocalBroadcast, PlayerConfig, VideoEncoding, VideoRendition,
        VideoSource, audio::Layout, video,
    },
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let peer: EndpointId = std::env::args()
        .nth(1)
        .ok_or("usage: call <peer id>")?
        .parse()?;
    let live = Live::builder(EndpointOptions::default().bind().await?)
        .with_router()
        .spawn();
    println!("our id: {}", live.endpoint().id());

    let broadcast = LocalBroadcast::new();
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(video::Size::new(640, 360), rate),
        VideoEncoding::single(VideoRendition::p360()),
    )?;
    broadcast.set_audio(
        AudioSource::test_pattern(Layout::Mono),
        AudioEncoding::voice(),
    )?;
    let call = live.call(peer, &broadcast)?;

    let subscription = live.answer(peer).await?;
    let player = live
        .remote_broadcast(&subscription)
        .play(PlayerConfig::default())?;
    let mut frames = player.video();
    while let Some(frame) = frames.next().await {
        println!("peer frame {}", frame.size());
    }

    call.unpublish();
    live.shutdown().await;
    Ok(())
}
```

A node learns it is being called when the peer's `call` path appears in its
route table, and `Publication::unpublish` hangs up. `irl call` and the Android
demo use the same convention, so they call each other.

## Rooms

A room is a gossip topic. `spawn_with_rooms` mounts the gossip protocol and
lets members publish under their room paths. `join` returns a `Room` with a
watched membership. This program creates a room, prints its ticket, publishes a
test pattern, and plays the first broadcast another member announces. Pass a
ticket to join instead of creating.

```rust
use iroh::Watcher;
use iroh_live::{
    EndpointOptions, Live,
    media::{LocalBroadcast, PlayerConfig, VideoEncoding, VideoRendition, VideoSource, video},
    rooms::RoomTicket,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (live, rooms) = Live::builder(EndpointOptions::default().bind().await?).spawn_with_rooms();
    let ticket = match std::env::args().nth(1) {
        Some(ticket) => ticket.parse()?,
        None => RoomTicket::generate(),
    };
    let room = rooms.join(&ticket, Some("ada".into())).await?;
    println!("room ticket: {}", room.ticket());

    let broadcast = LocalBroadcast::new();
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(video::Size::new(640, 360), rate),
        VideoEncoding::single(VideoRendition::p360()),
    )?;
    room.publish("cam", &broadcast)?;

    let mut state = room.state();
    let (peer, name) = loop {
        let found = state.get().peers.iter().find_map(|(peer, member)| {
            member
                .broadcasts
                .iter()
                .next()
                .map(|name| (*peer, name.clone()))
        });
        if let Some(found) = found {
            break found;
        }
        state.updated().await?;
    };
    let subscription = room.subscribe(peer, &name).await?;
    let player = live
        .remote_broadcast(&subscription)
        .play(PlayerConfig::default())?;
    let mut frames = player.video();
    while let Some(frame) = frames.next().await {
        println!("{peer} {name}: {}", frame.size());
    }
    Ok(())
}
```

Room broadcasts are private to the members, and a member that leaves loses its
paths. A real application also resubscribes when a broadcast ends: see
[Rooms](rooms.md).

## Publish at your own path

`publish` and `subscribe` use the `live/<id>/<name>` layout and a ticket.
`publish_at` and `subscribe_path` take any path, and `Audience::Peers` limits
who is offered the broadcast. Nothing here is a ticket: the subscriber needs the
publisher's id and the path.

The subscriber's node also has to let the publisher announce that path. The
default grant allows only `live/<id>/`, so this program widens it with a scope
that names the peer, `stage/<id>/**`. Run it with no argument to publish and
print the id, and with that id to watch.

```rust
use std::collections::BTreeSet;

use iroh::EndpointId;
use iroh_live::{
    Audience, EndpointOptions, Live, Reach,
    media::{LocalBroadcast, PlayerConfig, VideoEncoding, VideoRendition, VideoSource, video},
    moq::net::Pattern,
    moq_config_with,
};
use n0_watcher::Watchable;

/// What a peer may publish on this node: `stage/<its id>/**`.
fn stage_scope(peer: EndpointId) -> Pattern {
    format!("stage/{peer}/**").parse().expect("a valid pattern")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = EndpointOptions::default().bind().await?;
    let live = Live::builder_with(endpoint, moq_config_with(&[stage_scope]))
        .with_router()
        .spawn();
    let me = live.endpoint().id();
    println!("our id: {me}");

    let mut args = std::env::args().skip(1);
    let (mode, other) = (args.next(), args.next());
    let other: EndpointId = other
        .ok_or("usage: stage <publish|watch> <other id>")?
        .parse()?;
    match mode.as_deref() {
        Some("publish") => {
            let broadcast = LocalBroadcast::new();
            let rate = video::FrameRate::new(30, 1).expect("a valid rate");
            broadcast.set_video(
                VideoSource::test_pattern(video::Size::new(640, 360), rate),
                VideoEncoding::single(VideoRendition::p360()),
            )?;
            // Offered to `other` only. The set is watched, so it can change.
            let audience = Audience::Peers(Watchable::new(BTreeSet::from([other])));
            live.publish_at(&format!("stage/{me}/main"), &broadcast, audience)?;
            tokio::signal::ctrl_c().await?;
        }
        _ => {
            let path = format!("stage/{other}/main");
            let subscription = live.subscribe_path(&path, Reach::Direct(other)).await?;
            let player = live
                .remote_broadcast(&subscription)
                .play(PlayerConfig::default())?;
            let mut frames = player.video();
            while let Some(frame) = frames.next().await {
                println!("frame {}", frame.size());
            }
        }
    }
    live.shutdown().await;
    Ok(())
}
```

Run `publish <viewer id>` on one machine and `watch <publisher id>` on the
other. A third node that dials the publisher cannot resolve the path.

## Attach a relay

A relay carries broadcasts to peers that cannot connect directly, and to
browsers. `attach_relay` stays attached and redials with backoff. Every
publication with the `Everyone` audience, which `Live::publish` uses, is then
offered to the relay as well. A publisher that does not need the relay's other
routes sets `consume: false`.

```rust
use iroh::EndpointId;
use iroh_live::{
    EndpointOptions, Live,
    media::{LocalBroadcast, VideoEncoding, VideoRendition, VideoSource, video},
    moq::RelayConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let relay: EndpointId = std::env::args()
        .nth(1)
        .ok_or("usage: relay <relay id>")?
        .parse()?;
    let live = Live::builder(EndpointOptions::default().bind().await?)
        .with_router()
        .spawn();

    let link = live.moq().attach_relay(RelayConfig {
        consume: false,
        ..RelayConfig::iroh(relay)
    })?;

    let broadcast = LocalBroadcast::new();
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(video::Size::new(640, 360), rate),
        VideoEncoding::single(VideoRendition::p360()),
    )?;
    live.publish("pattern", &broadcast)?;
    println!("{}", live.ticket("pattern"));

    tokio::signal::ctrl_c().await?;
    link.detach().await;
    live.shutdown().await;
    Ok(())
}
```

A viewer that has the relay attached with `consume: true` and no direct route
reaches the broadcast through it, with `Reach::Relays` or a ticket. The
[browser relay guide](browser-relay.md) covers running the relay and watching
from a browser, and
[`iroh-live/examples/subscribe_test.rs`](../../iroh-live/examples/subscribe_test.rs)
is the viewing side in Rust.

## Data without media

A broadcast is a set of tracks, and a track is a stream of groups of frames.
Nothing requires them to be video. `live.moq()` is the transport, and a
`moq_net` broadcast is the unit it publishes, so a control channel is a
broadcast with one track. Run it with no argument to serve, and with the printed
id to read.

```rust
use std::time::Duration;

use iroh::EndpointId;
use iroh_live::{
    Audience, EndpointOptions, Live, Reach,
    moq::net::{Timescale, Timestamp, broadcast, track},
    paths,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let live = Live::builder(EndpointOptions::default().bind().await?)
        .with_router()
        .spawn();

    match std::env::args().nth(1) {
        None => {
            let me = live.endpoint().id();
            println!("read with: {me}");
            let broadcast = broadcast::Info::new().produce();
            let mut control = broadcast.create_track(
                "control",
                track::Info::default().with_timescale(Timescale::MILLI),
            )?;
            let path = paths::broadcast(me, "control");
            let _publication = live.moq().publish(path, &broadcast, Audience::Everyone)?;
            for n in 0u32.. {
                control.write_frame(Timestamp::now(), format!("tick {n}").into_bytes())?;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        Some(id) => {
            let peer: EndpointId = id.parse()?;
            let path = paths::broadcast(peer, "control");
            let subscription = live.moq().subscribe(path, Reach::Direct(peer)).await?;
            let mut control = subscription
                .as_moq()
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
    live.shutdown().await;
    Ok(())
}
```

Frames are bytes, so the format is yours. The route table, the audiences and the
grants apply as they do for media.

## Without `Live`

`Live` is a convenience over `Moq` and an endpoint. A program with its own
router, its own ALPNs or its own access rules mounts `Moq` itself and gets the
same sessions, route table and relays, with no media dependency. The default
`MoqConfig` lets peers read anything and publish nothing, so the reader below
grants the publisher `demo/**`. The publishing side is
[`iroh-live/examples/moq_only.rs`](../../iroh-live/examples/moq_only.rs), which
serves `demo/hello`.

```rust
use std::sync::Arc;

use iroh::EndpointId;
use iroh_live::moq::{Grant, Moq, MoqConfig, MoqPreset, Reach, net::Pattern};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let publisher: EndpointId = std::env::args()
        .nth(1)
        .ok_or("usage: reader <id>")?
        .parse()?;
    let endpoint = iroh::Endpoint::bind(MoqPreset).await?;
    let demo: Pattern = "demo/**".parse()?;
    let moq = Moq::new(
        endpoint.clone(),
        MoqConfig {
            grant: Some(Arc::new(move |_peer| Grant::publish_under(demo.clone()))),
            ..Default::default()
        },
    );
    // Mount your own protocols on the same router.
    let router = moq
        .mount(iroh::protocol::Router::builder(endpoint.clone()))
        .spawn();

    let subscription = moq
        .subscribe("demo/hello", Reach::Direct(publisher))
        .await?;
    println!("subscribed to {}", subscription.path());
    if let Some(link) = subscription.link() {
        println!(
            "served by a {:?} link, rtt {:?}",
            link.kind, link.sample.rtt
        );
    }

    moq.shutdown().await;
    router.shutdown().await?;
    endpoint.close().await;
    Ok(())
}
```

`Moq::sessions`, `Moq::accept` with `Admission::Manual`, and `Session::link`
cover the rest of the node. See [the transport page](../architecture/transport.md).
