# Publish at your own path

This page publishes at a path of your own choosing, to a chosen set of peers.
`LiveLayout` publishes at `live/<id>/<name>` to every granted session. For
anything else, use `Node::publish` and `Node::subscribe` directly.
[Paths and layouts](../concepts/paths.md) explains the idea.

## The program

`Node::publish(path, audience)` takes any path and any audience, and
`Node::subscribe(path, reach)` any path. `Audience::peers` limits who sees the
broadcast, and moq enforces it on each live session. Nothing here is a
ticket: the subscriber needs the publisher's id and the path.

The subscriber's node also has to let the publisher announce that path. The
live layout allows only `live/<id>/`, so this program adds a second layout,
`stage/<id>/`, and gives the node the union of both. Run `stage publish
<viewer id>` on one machine and `stage watch <publisher id>` on the other.
Each prints its own id on start.

```rust,no_run
use iroh::{EndpointId, protocol::Router};
use iroh_live::{
    Audience, EndpointConfig, Node, Reach,
    layout::{self, Layout, LiveLayout},
    media::{
        LocalBroadcast, PlayerConfig, SubscriptionExt, VideoEncoding, VideoRendition, VideoSource,
        video,
    },
    moq_net::{Pattern, Patterns},
};

/// Puts a peer's broadcasts at `stage/<peer>/<name>`.
#[derive(Debug, Clone)]
struct Stage;

impl Layout<EndpointId> for Stage {
    fn scope(&self, peer: &EndpointId) -> Patterns {
        let pattern: Pattern = format!("stage/{peer}/**").parse().expect("a valid pattern");
        Patterns::from(pattern)
    }

    fn owner(&self, path: &str) -> Option<EndpointId> {
        let (peer, name) = path.strip_prefix("stage/")?.split_once('/')?;
        let id: EndpointId = peer.parse().ok()?;
        (id.to_string() == peer && !name.is_empty()).then_some(id)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let config = layout::config((LiveLayout::default(), Stage));
    let node = Node::new(endpoint.clone(), config);
    let router = node.mount(Router::builder(endpoint)).spawn();
    let me = node.id();
    println!("our id: {me}");

    let mut args = std::env::args().skip(1);
    let (mode, other) = (args.next(), args.next());
    let other: EndpointId = other
        .ok_or("usage: stage <publish|watch> <other id>")?
        .parse()?;
    match mode.as_deref() {
        Some("publish") => {
            // Visible to `other` only.
            let audience = Audience::peers([other]);
            let publication = node.publish(&format!("stage/{me}/main"), audience)?;
            let broadcast = LocalBroadcast::new_in(publication.broadcast().clone())?;
            let rate = video::FrameRate::new(30, 1).expect("a valid rate");
            broadcast.set_video(
                VideoSource::test_pattern(video::Size::new(640, 360), rate)?,
                VideoEncoding::single(VideoRendition::p360()),
            )?;
            tokio::signal::ctrl_c().await?;
        }
        _ => {
            let path = format!("stage/{other}/main");
            let subscription = node.subscribe(&path, Reach::Direct(other)).await?;
            let player = subscription.remote_broadcast().play(PlayerConfig::default())?;
            let mut frames = player.video_frames();
            while let Some(frame) = frames.next().await {
                println!("frame {}", frame.size());
            }
        }
    }
    router.shutdown().await?;
    Ok(())
}
```

A third node that dials the publisher cannot resolve the path, since the
audience leaves it out.

## Changing the audience

To change the set later, publish with `Audience::Peers(set)` and keep a clone
of the `n0_watcher::Watchable` set. A peer taken out of the set loses the
path at once. `Publication::set_audience` replaces the audience of a running
publication.

`Node::publish_with(path, audience, fill)` fills the broadcast before it is
announced, so a reader never sees it empty. With `LiveLayout::path(id, name)`
it publishes under the live layout to another audience.

## Where to go next

- [Paths and layouts](../concepts/paths.md) explains the `Layout` trait.
- [Access reference](../reference/access.md) lists the audiences.
