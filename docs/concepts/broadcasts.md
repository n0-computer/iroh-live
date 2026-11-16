# Broadcasts, tracks and renditions

A *broadcast* is a named set of tracks that one node publishes, such as a
camera's video and its microphone. A *track* is one stream inside it. These
are the units of [Media over QUIC](https://moq.dev/), through moq's Rust
implementation, [moq-dev/moq](https://github.com/moq-dev/moq).

## The model

A broadcast lives at a [path](paths.md). `irl publish` publishes at
`live/<endpoint id>/hello`, a call at `live/<endpoint id>/call`, and a room at
`rooms/<topic>/<endpoint id>/<name>`.

A track is one video rendition, the audio, or any data. A video track has the
name of its `VideoRendition`. `irl publish` names a single rendition `video`,
and `irl publish --renditions low:320x180,720p` publishes `low` and `720p`.
The audio track is named after its codec, such as `opus`.

A *group* is a run of frames that starts with a keyframe. A receiver that
falls behind skips to the newest group, so latency does not build up under
congestion. Each group travels on its own QUIC stream, so a lost video packet
does not delay audio.

## Renditions and the ladder

A *rendition* is one encoding of the video source, at one size and bitrate.
A *ladder* is a set of renditions of the same source, which
`VideoEncoding::ladder` builds. The source is captured once, and each rung
encodes the same frames.

Each viewer's player subscribes to one rendition at a time, and picks the one
its link can carry. A rendition encodes only while at least one viewer
subscribes to it, so an unwatched rung costs no CPU.

## The catalog

A *catalog* is a track that describes the other tracks: each rendition's
codec, size and bitrate. It is
[hang's catalog](https://doc.moq.dev/concept/layer/hang), unchanged, so any
hang player can read it. A subscriber reads the catalog before anything else,
and watches it for changes. `RemoteBroadcast::catalog()` returns it.

## Not only media

A broadcast can carry tracks of any bytes, next to the media or without it.
[Send data and state](../how-to/data.md) shows a control channel and a
latest-value JSON track.

## Where to go next

- [Publish a camera, screen or file](../how-to/publish.md)
- [Tune quality and latency](../how-to/tune.md) sets the ladder.
- Upstream, [moq-lite](https://doc.moq.dev/concept/layer/moq-lite) explains
  the publish and subscribe model.
- [Publishing](../architecture/publish.md) covers the encoder side in depth.
