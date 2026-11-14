# iroh-live

Live audio and video over [iroh](https://github.com/n0-computer/iroh).

A `Node` publishes broadcasts at paths and subscribes to paths, over a direct
session with a peer or an attached relay. `LiveLayout` puts each peer's
broadcasts at `live/<endpoint id>/<name>`, and a `BroadcastTicket` names one
for a viewer. The crate re-exports the node as `iroh_live::node`, moq-net as
`iroh_live::moq_net`, the media crates as `iroh_live::media`, and the rooms
crate as `iroh_live::rooms`.

The crate builds against moq's unreleased `main` branch, pinned by git rev, so
it is not on crates.io. Depend on it through git:

```toml
iroh-live = { git = "https://github.com/n0-computer/iroh-live", features = ["full"] }
```

## Example

Publish a camera, and share the ticket:

```rust,ignore
let layout = LiveLayout::default();
let endpoint = EndpointConfig::from_env()?.bind().await?;
let node = Node::new(endpoint.clone(), layout.config());
// Accepts the viewers. Keep it: dropping it stops accepting.
let router = node.mount(Router::builder(endpoint)).spawn();
let broadcast: LocalBroadcast = layout.publish(&node, "hello")?;
let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(camera, VideoEncoding::single(VideoRendition::new("video")))?;
println!("{}", layout.ticket(node.id(), "hello")?);
```

On the other side, subscribe and play:

```rust,ignore
let subscription = node.subscribe_ticket(&ticket).await?;
let player = subscription.remote_broadcast().play(PlayerConfig::default())?;
let mut frames = player.video_frames();
```

The [library quickstart](../docs/getting-started/library-quickstart.md) has
both sides as complete programs, with their imports.

## Examples

- `examples/publish.rs` publishes a camera and a microphone, with a simulcast
  ladder behind `--simulcast`. Needs `capture`.
- `examples/publish-pi.rs` publishes a Raspberry Pi camera. Needs `rpicam`.
- `examples/moq_only.rs` serves a broadcast with `Node` alone, without a
  layout.
- `examples/contacts_only.rs` admits only a node's contacts, and drops one on
  `Node::reauthorize`.
- `examples/subscribe_test.rs` is the helper the browser test suite runs.

## More

- The [how-to guides](../docs/SUMMARY.md) cover calls, rooms, custom paths,
  relays, recording, access control and data tracks.
- [Cargo features](../docs/reference/features.md) lists this crate's
  features. None is on by default.
- The crate docs list the main types and which futures are cancellation safe.
