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
iroh-live a broadcast lives at a *path* on the publisher's node origin, which is
the string in a ticket: `hello` from `irl publish`, `pi-zero` from the Pi demo,
`calls/<endpoint id>` from a call.

A **track** is one media stream inside a broadcast: one video rendition, or the
audio. Track names come from the publisher. A single-rendition video publish uses
`video`; a simulcast ladder uses whatever the rungs are called, which is where
`irl publish --renditions low:320x180,720p` gets `low` and `720p`. Audio uses the
codec name unless the caller sets one.

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

iroh-live extends the catalog rather than replacing it. `moq_media::catalog`
flattens `chat` and `user` sections alongside hang's `video` and `audio`, so a
plain hang player ignores them and still plays the media. That is how a
subscriber finds the chat track without guessing at a name, and how a publisher's
display name travels with its stream.

## Where the boundary is

`moq-media` speaks `moq_net` types and nothing else: a publish is a
`broadcast::Producer`, a subscription is a `broadcast::Consumer`. It does not know
whether those arrived over iroh, over WebTransport, or through a local loopback.

`iroh-moq` is the half that knows about iroh, and [the transport
page](../architecture/transport.md) covers what it does: the node origin, session
deduplication, and ALPN negotiation.
