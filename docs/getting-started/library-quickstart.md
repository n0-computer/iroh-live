# Quickstart: library

This page builds two programs: a publisher that prints a ticket, and a viewer
that plays it. They are the canonical examples, and the other pages build on
them. Both use generated test media, so they need no camera.

## Add the dependency

iroh-live is not on crates.io yet, so it comes from git (see
[Install](install.md#use-the-library)):

```toml
[dependencies]
iroh = "1.4"
iroh-live = { git = "https://github.com/n0-computer/iroh-live", features = ["full"] }
tokio = { version = "1", features = ["full"] }
```

`full` turns on the media stack with capture, rendering and recording. The
[features reference](../reference/features.md) lists the others.

## Publish

The publisher binds an endpoint, creates a node, and mounts the node on a
router so that viewers can connect. It then publishes a broadcast named
`hello` with two video renditions and a test tone, and prints its ticket.

```rust,no_run
use iroh::protocol::Router;
use iroh_live::{
    EndpointConfig, Node,
    layout::LiveLayout,
    media::{
        AudioSource, LocalBroadcast, VideoEncoding, VideoRendition, VideoSource,
        audio::Layout, audio_encoding, video,
    },
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reads the key from `IROH_SECRET`, so the ticket survives a restart.
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let layout = LiveLayout::default();
    let node = Node::new(endpoint.clone(), layout.config());
    // Accepts the viewers. Dropping the router stops accepting.
    let router = node.mount(Router::builder(endpoint)).spawn();

    // Published while `broadcast` lives.
    let broadcast: LocalBroadcast = layout.publish(&node, "hello")?;
    let size = video::Size::new(1280, 720);
    let rate = video::FrameRate::new(30, 1).expect("a valid rate");
    broadcast.set_video(
        VideoSource::test_pattern(size, rate)?,
        VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
    )?;
    broadcast.set_audio(
        AudioSource::test_pattern(Layout::Mono)?,
        audio_encoding::voice(),
    )?;
    println!("{}", layout.ticket(node.id(), "hello")?);

    tokio::signal::ctrl_c().await?;
    // Shuts down the node, then closes the endpoint.
    router.shutdown().await?;
    Ok(())
}
```

The broadcast is at the path `live/<endpoint id>/hello`, and the ticket names
that path. A rendition encodes only while someone watches it.

## Watch

The viewer resolves the ticket, dials the publisher, and starts a player. The
player picks a rendition as the link allows. This one prints the size of
each decoded frame:

```rust,no_run
use std::time::Duration;

use iroh_live::{
    BroadcastTicket, EndpointConfig, Node, NodeConfig,
    media::{PlayerConfig, SubscriptionExt},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ticket: BroadcastTicket = std::env::args()
        .nth(1)
        .ok_or("usage: watch <ticket>")?
        .parse()?;
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    // A viewer only dials, so it needs no router and no layout.
    let node = Node::new(endpoint.clone(), NodeConfig::default());

    let subscription = node.subscribe_ticket(&ticket).await?;
    let player = subscription.remote_broadcast().play(PlayerConfig::default())?;

    let mut frames = player.video_frames();
    for count in 1..=100 {
        let frame = tokio::time::timeout(Duration::from_secs(10), frames.next())
            .await?
            .ok_or("the video ended")?;
        println!("frame {count}: {}", frame.size());
    }
    node.shutdown().await;
    endpoint.close().await;
    Ok(())
}
```

Run the publisher, then pass its ticket to the viewer, on the same machine or
another one. `irl watch <TICKET>` plays the same ticket in a window.

`PlayerConfig::default()` does not subscribe to audio. To hear it, use
`with_audio(AudioMode::Play(engine))` with a `playback::Engine`, as
[Watch a broadcast](../how-to/watch.md) shows.

## Keep the same ticket across restarts

A ticket names the publisher's endpoint id, which is the public half of its
secret key. Without a key, `EndpointConfig::from_env()` makes a new one on
every start, so every run prints a new ticket. Set `IROH_SECRET` to a fixed
key to keep it:

```sh
export IROH_SECRET=$(openssl rand -hex 32)   # once, then keep the value
```

`irl` reads the same variable. The key is a secret: whoever has it can act as
this endpoint. An application that stores its own key sets
`EndpointConfig::with_secret_key`, and `iroh_live::load_or_create_secret_key(path)`
keeps one in a file.

## Next steps

- To publish a camera, a screen or a file, see
  [Publish](../how-to/publish.md).
- To draw the frames, see [Watch a broadcast](../how-to/watch.md).
- For calls and rooms, see [Make a call](../how-to/call.md) and
  [Run a room](../how-to/rooms.md).
- To reach browsers, see [Serve browsers](../how-to/browsers.md).
- To learn what a node, a path and a route are, start at
  [How it fits together](../concepts/overview.md).
