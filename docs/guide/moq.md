# MoQ, as it appears here

The wire protocol is [Media over QUIC](https://moq.dev/), through the
[moq-dev/moq](https://github.com/moq-dev/moq) Rust implementation. The protocol
is documented at [doc.moq.dev](https://doc.moq.dev/concept/layer/). Start with
[moq-lite](https://doc.moq.dev/concept/layer/moq-lite) for the pub/sub model,
[hang](https://doc.moq.dev/concept/layer/hang) for how media is described, and
[iroh](https://doc.moq.dev/concept/layer/iroh) for the peer-to-peer transport.
This page covers the terms the rest of these docs use.

## The model

A **broadcast** is a named set of tracks that one endpoint publishes. It lives
at a *path*. `iroh-moq` takes any path. `iroh-live` publishes at
`live/<endpoint id>/<name>`: `live/<id>/hello` from `irl publish`,
`live/<id>/pi-zero` from the Pi demo, `live/<id>/call` from a call. Rooms use
`rooms/<topic>/<endpoint id>/<name>`. A ticket holds the endpoint id and the
name, which give the path.

A **track** is one stream inside a broadcast: one video rendition, or the
audio. A video track has the name of its `VideoRendition`. `irl publish` names
a single rendition `video`, and `irl publish --renditions low:320x180,720p`
publishes `low` and `720p`. The audio track is named after
its codec, such as `opus`.

A **group** is a run of frames that starts with a keyframe. A receiver that
falls behind skips to the newest group, so latency does not build up under
congestion. Each group travels on its own QUIC stream, so a lost video packet
does not delay audio.

## The catalog

hang adds a **catalog**: a track that describes the other tracks, with each
rendition's codec, resolution, and bitrate. A subscriber reads it before it
subscribes to anything else, and watches it for changes.

iroh-live publishes hang's catalog unchanged, so any hang player can read it.
Applications read it as `iroh_live_media::Catalog`.

## Where the boundary is

`iroh-live-media` uses only `moq_net` types. A `LocalBroadcast` is published
through `moq_net::Consume`, and a `RemoteBroadcast` is built from what a
transport subscribed to. The media crate does not know whether that came over
iroh, a relay, or a local loopback.

`iroh-moq` is the iroh side. [The transport page](../architecture/transport.md)
covers the route table, publications and their audiences, sessions, and relay
links.
