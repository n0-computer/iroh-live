# moq-ext-player

A player on top of
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio). It reads a broadcast's
catalog, picks the rendition the link can carry, switches renditions without a
blank picture, and keeps audio and video in step.

It does not depend on iroh or on a publisher. A transport builds a
`RemoteBroadcast` from a `moq_net::broadcast::Consumer` (`from_consumer`) or from
a route table (`from_resolved`), and attaches its view of the
link with `set_network`. A broadcast announced before its catalog track exists
is read once the catalog appears.

```rust
use moq_ext_player::{PlayerConfig, RemoteBroadcast};

let player = RemoteBroadcast::from_consumer(consumer).play(PlayerConfig::default())?;
let mut frames = player.video_frames();
while let Some(frame) = frames.next().await {
    // draw `frame`
}
```

`PlayerConfig::rendition` takes a `RenditionMode`: automatic, pinned to one
rendition, or off. `PlayerConfig::audio` takes an `AudioMode`: off, decoded
only for `Player::audio_frames`, or played on a `moq_audio::playback::Engine`
(feature `playback`). Dropping the `Player` aborts its tasks and returns at
once. A session that closes ends playback quietly: a cancel from the peer is
not a failed decoder.

A broadcast published in the same process is read the same way: pass
`broadcast.consume()` of a `moq_ext_publisher::LocalBroadcast`.

## Feature flags

No feature is on by default. Without one, the crate decodes, but cannot play
sound or draw. [Cargo features](../docs/reference/features.md) lists them.
