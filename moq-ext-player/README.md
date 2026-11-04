# moq-ext-player

A player on top of
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio). It reads a broadcast's
catalog, picks the rendition the link can carry, switches renditions without a
blank picture, and keeps audio and video in step.

It does not depend on iroh or on a publisher. A transport builds a
`RemoteBroadcast` from a `moq_net::broadcast::Consumer` (`from_consumer`) or from
a route table (`from_origin`, `from_resolved`), and attaches its view of the
link with `set_network`.

```rust
use moq_ext_player::{PlayerConfig, RemoteBroadcast};

let player = RemoteBroadcast::from_consumer(consumer).play(PlayerConfig::default())?;
let mut frames = player.video();
while let Some(frame) = frames.next().await {
    // draw `frame`
}
```

`PlayerConfig::rendition` takes a `RenditionMode` (`moq_ext_adaptive::Mode`):
automatic, pinned to one rendition, or off. `PlayerConfig::audio` takes an
`AudioTarget`, which is a `moq_audio::playback::Engine` or nowhere. Dropping
the `Player` aborts its tasks and returns at once.

A broadcast published in the same process is read the same way: pass
`moq_net::Consume::consume(&broadcast)` of a `moq_ext_publisher::LocalBroadcast`.

## Feature flags

| Feature | Default | What it adds |
|---|---|---|
| `record` | no | `RemoteBroadcast::record`: remux to fragmented MP4 or Matroska without decoding |
| `playback` | no | Speaker output |
| `capture` | no | Lets an `AudioTarget` serve as a microphone's echo reference |
| `aec` | no | Echo cancellation. Implies `capture` and `playback` |
| `render` | no | The wgpu renderer |
| `vaapi`, `nvidia`, `v4l2` | no | Hardware H.264 decoding |
