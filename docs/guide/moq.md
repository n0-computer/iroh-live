# MoQ Protocol Overview

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | draft |
| Applies to | iroh-moq, moq-media |
| Platforms | all |

## Transport layer

iroh-live uses [moq-lite](https://github.com/kixelated/moq), a Rust implementation of Media over QUIC, for its wire protocol. moq-lite carries media as named broadcasts, each containing one or more tracks. The connection is established over iroh's QUIC transport with the ALPN `moq-lite-03`.

The `iroh-moq` crate wraps moq-lite with iroh endpoint management: it handles connection setup, protocol negotiation, and session lifecycle. Application code interacts with `MoqSession`, which exposes publish and subscribe operations on named broadcasts.

## Broadcasts and tracks

A *broadcast* is a named collection of tracks published by a single endpoint. Each track carries an independent media stream, typically one video rendition or one audio stream. Tracks are identified by name within their broadcast.

iroh-live uses the naming convention `video/<codec>-<preset>` for video tracks and `audio/<codec>` for audio tracks. For example, a broadcast with two video quality levels and one audio stream might contain:

- `video/h264-720p`
- `video/h264-360p`
- `audio/opus`

This naming convention is not enforced by the protocol. It is a pattern established by iroh-live's `LocalBroadcast` and `RemoteBroadcast` types. Application developers building directly on moq-lite can use any naming scheme.

## Groups and keyframes

Each track is a sequence of *groups*. A group is the smallest independently decodable unit: it starts with a keyframe and contains all subsequent frames until the next keyframe. This structure lets receivers skip ahead when falling behind, jumping to the latest group boundary rather than buffering stale data.

Within a group, frames are delivered in order. Across groups, the protocol supports gap tolerance: if a group is lost or arrives too late, the receiver can skip it and start decoding from the next group's keyframe.

## The hang crate

The [hang](https://github.com/kixelated/moq) crate builds on top of moq-lite, adding two capabilities:

- **Catalog management**: a catalog track describes the available video and audio tracks in a broadcast, including codec parameters, resolution, bitrate, and sample rate. Subscribers watch the catalog to discover what tracks are available and react to changes (e.g., a new rendition added by adaptive encoding).
- **Ordered consumption**: `TrackConsumer` in hang provides latency-aware group ordering. It tracks PTS timestamps across pending groups and skips stale groups when the span exceeds a configurable `max_latency` threshold.

iroh-live's `RemoteBroadcast` uses hang internally to watch the catalog and manage track subscriptions. `LocalBroadcast` uses hang to publish the catalog alongside media tracks.

## Packet format

At the moq-lite level, each frame is a byte payload within a group. iroh-live's `MediaPacket` adds a thin header: a varint-encoded PTS timestamp, flags (keyframe, codec config), and the compressed frame data. This header is defined in `moq-media/src/transport.rs` and is codec-agnostic.

The `PacketSource` and `PacketSink` traits abstract over the transport, allowing the same encode/decode pipeline to work with moq-lite, local channels, or file I/O.

## Connection flow

A typical publish-subscribe flow:

1. The publisher creates an iroh endpoint and starts accepting connections with the `moq-lite-03` ALPN.
2. The publisher creates a `LocalBroadcast`, which produces a moq-lite `BroadcastProducer` with a catalog and media tracks.
3. A subscriber connects to the publisher's endpoint address, negotiates a `MoqSession`, and subscribes to the broadcast by name.
4. The subscriber receives the catalog, selects tracks, and begins consuming groups from each track.
5. Decoded frames are delivered to the application for rendering.

The same flow works through the relay: the relay accepts the subscriber's connection (via WebTransport or iroh), pulls the broadcast from the publisher if needed, and forwards groups to the subscriber.

## Further reading

- [moq-lite and hang source](https://github.com/kixelated/moq): protocol implementation and catalog layer.
- [Media over QUIC specification](https://moq.dev/): the IETF draft that moq-lite implements.
- [browser-relay.md](browser-relay.md): how the relay bridges moq-lite to browser WebTransport.
