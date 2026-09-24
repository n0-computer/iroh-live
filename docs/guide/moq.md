# MoQ, as it appears here

The wire protocol is [Media over QUIC](https://moq.dev/), through the
[moq-dev/moq](https://github.com/moq-dev/moq) Rust implementation. The protocol
itself is documented at [doc.moq.dev](https://doc.moq.dev/concept/layer/): start
with [moq-lite](https://doc.moq.dev/concept/layer/moq-lite) for the pub/sub
model, [hang](https://doc.moq.dev/concept/layer/hang) for how media is described,
and [iroh](https://doc.moq.dev/concept/layer/iroh) for what changes when the
transport is peer-to-peer.

This page is the short version, in the vocabulary the rest of these docs use.

## The model

A **broadcast** is a named collection of tracks published by one endpoint. In
iroh-live a broadcast lives at a *path* that names its publisher:
`live/<endpoint id>/hello` from `irl publish`, `live/<endpoint id>/pi-zero` from
the Pi demo, `calls/<endpoint id>` from a call. A ticket names the endpoint and
the name, and maps to the path.

A **track** is one media stream inside a broadcast: one video rendition, or the
audio. Track names come from the publisher. A single-rendition video publish uses
`video`; a simulcast ladder uses whatever the rungs are called, which is where
`irl publish --renditions low:320x180,720p` gets `low` and `720p`. Audio uses the
codec name, such as `opus`.

A **group** is a sequence of frames starting with a keyframe, and it is the unit
a receiver can skip. Falling behind means jumping to the newest group boundary
rather than draining stale frames, which is what keeps latency from accumulating
under congestion.

Every track is its own set of QUIC streams, so a dropped video packet never
delays audio.

## The catalog

hang adds a **catalog**: a track that describes the other tracks, listing each
rendition's codec, resolution, and bitrate. A subscriber reads it to learn what
exists before subscribing to anything, and watches it for changes, since a
publisher can add a rendition mid-broadcast.

iroh-live publishes hang's catalog as it is, so a plain hang player reads it.
Applications read it as `iroh_live_media::Catalog`. Chat is not in the media
catalog: a room publishes it as a broadcast of its own.

## Where the boundary is

`iroh-live-media` speaks `moq_net` types and nothing else: a `LocalBroadcast` is
built on a `broadcast::Producer` and read through `moq_net::Consume`, and a
`RemoteBroadcast` is built from a `broadcast::Consumer`. It does not know
whether those arrived over iroh, over WebTransport, or through a local loopback.

`iroh-moq` is the half that knows about iroh, and [the transport
page](../architecture/transport.md) covers what it does: the route table fed by
every link, publications and their audiences, session deduplication, relay
links, and ALPN negotiation.
