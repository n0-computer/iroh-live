# Subscribe Pipeline

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | moq-media |

`RemoteBroadcast` wraps a `BroadcastConsumer` and watches its catalog for
available video and audio renditions. It provides handles to start
decoding individual tracks. The subscribe pipeline mirrors the publish
pipeline: where publishers encode on OS threads and write to
`PacketSink`, subscribers read from `PacketSource` and decode on OS
threads.

## RemoteBroadcast

Created with a `BroadcastConsumer` (from moq-lite), a `PlayoutClock`, and
a `CancellationToken`. On construction, it spawns a catalog-watching task
that monitors the broadcast's catalog track for changes and publishes
snapshots via a `Watchable<CatalogSnapshot>`.

`RemoteBroadcast` is `Clone` (Arc-based) and all methods take `&self`.
Multiple parts of an application can hold references to the same broadcast
and independently create tracks from it.

## CatalogSnapshot

A point-in-time view of the broadcast's catalog. It derefs to hang's
`Catalog` for direct access to video and audio rendition maps. Each
snapshot carries a sequence number for change detection.

`CatalogSnapshot` provides selection helpers:
`select_video_rendition(quality)` picks the best video rendition for a
`Quality` level (Highest, High, Mid, Low), and `video_renditions()`
iterates rendition names sorted by width ascending.

## VideoTarget

Subscribers describe what they need declaratively:

```rust
VideoTarget::default()
    .max_pixels(1280 * 720)
    .max_bitrate_kbps(2000)
```

The catalog selects the best matching rendition. If `rendition` is set
explicitly, it takes priority over pixel and bitrate constraints.
`VideoTarget` converts from `Quality` for convenience.

## VideoOptions and AudioOptions

`VideoOptions` bundles a `VideoTarget`, decoder configuration
(`DecodeConfig`), and optional viewport dimensions. `AudioOptions`
optionally pins a specific audio rendition by name.

## VideoTrack

Represents a decoded video stream from a remote broadcast. The decoder
runs on a dedicated OS thread, producing `VideoFrame` values into an
`mpsc` channel.

Construction follows this path:

1. The caller picks a rendition (via `VideoTarget` or explicit name).
2. A `TrackConsumer` is created from the `BroadcastConsumer` for that
   rendition's track, wrapped in an `OrderedConsumer` with the playout
   clock's `hang_max_latency`.
3. An `MoqPacketSource` wraps the `OrderedConsumer`.
4. A `forward_packets` async task reads from the `MoqPacketSource` and
   sends `MediaPacket` values into an `mpsc` channel.
5. A decoder thread reads from that channel, pushes packets to the
   decoder, drains decoded frames into a `PlayoutBuffer`, and releases
   frames at their playout time into the output channel.

The output channel is stable across the lifetime of the `VideoTrack`.
Callers receive frames via `current_frame()` (drain-to-latest, for
rendering) or the underlying receiver.

## AudioTrack

Follows the same pattern as `VideoTrack` but sends decoded audio samples
to an `AudioSink` (typically backed by cpal, firewheel, or sonora). The
audio decoder thread runs a 10ms tick loop, draining packets from the
transport and pushing decoded samples to the sink.

## MediaTracks

A convenience struct combining a `RemoteBroadcast` with an optional
`VideoTrack` and optional `AudioTrack`. Provides a single entry point
for the common case of subscribing to both audio and video from one
broadcast.

## Decoder thread architecture

Decoder threads are OS threads, not tokio tasks, for the same reason as
encoder threads: codec operations are CPU-bound and sometimes block on
hardware. The async-to-sync bridge works through two channels:

- `forward_packets` (async task) reads from the MoQ transport and sends
  `MediaPacket` values into a bounded `mpsc` channel.
- The decoder thread reads from the `mpsc` receiver synchronously,
  pushes packets to the decoder, and writes decoded frames to the
  output channel.

The `PlayoutBuffer` sits between the decoder and the output channel,
absorbing bursty decoder output (especially from hardware decoders with
DPB flush patterns). See [playout.md](playout.md).

## Lifecycle

`VideoTrack` and `AudioTrack` use drop-based cleanup. Dropping the track
cancels the decoder thread (via `CancellationToken`) and the
`forward_packets` task (via `AbortOnDropHandle`). The `OrderedConsumer`
is dropped, which signals the transport that we no longer need this
track's data.

## Error handling

`SubscribeError` covers broadcast-not-found, no catalog, rendition not
found, decoder failure, and broadcast ended. These are structured variants
rather than opaque `anyhow::Error`, so callers can match on specific
failure modes.
