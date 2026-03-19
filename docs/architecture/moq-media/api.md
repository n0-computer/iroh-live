# moq-media API Design

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | moq-media |

## Design principle: transport-agnostic

moq-media has no dependency on iroh. It defines the media pipeline (capture, encode, decode, playout) and the transport boundary (`PacketSink`/`PacketSource`) but does not implement networking. This separation lets moq-media serve non-RTC use cases: studio links, recording pipelines, file-based playback, and local loopback testing.

## Publish side

`LocalBroadcast` is the publish-side entry point. It owns a `BroadcastProducer`, manages encoder pipelines, and publishes a catalog that subscribers use to discover available renditions.

`VideoPublisher` and `AudioPublisher` are slot handles with three operations: `set()` assigns a source, `replace()` swaps the source while keeping the track alive, and `clear()` removes it. Calling `set()` starts an encoder pipeline on a dedicated OS thread. Dropping the publisher stops its pipeline.

`VideoRenditions` and `AudioRenditions` publish simulcast layers from a single source. Each rendition gets its own encoder configuration (resolution, bitrate) but shares the same capture source.

`SharedVideoSource` wraps a `VideoSource` in a thread-safe container that parks (blocks the calling thread) when no subscribers are active. This avoids burning CPU cycles encoding frames that nobody is watching.

## Subscribe side

`RemoteBroadcast` wraps a `BroadcastConsumer` and watches the catalog for track additions and removals. It owns a `PlayoutClock` for A/V synchronization.

`VideoTrack` represents a decoded video stream. It runs a decoder on a dedicated OS thread and outputs `VideoFrame`s through an mpsc channel. `AudioTrack` does the same for audio, outputting decoded PCM samples to an `AudioSink`.

`MediaTracks` is a convenience struct combining a `RemoteBroadcast` with an optional `VideoTrack` and optional `AudioTrack`. It provides `video_ready().await` and `audio_ready().await` for waiting until tracks become available from the catalog.

`CatalogSnapshot` captures a point-in-time view of the catalog with helpers for rendition selection. `AdaptiveVideoTrack` wraps a `VideoTrack` and automatically switches renditions based on `NetworkSignals` (bandwidth, RTT, loss rate).

## Transport boundary

`PacketSource` and `PacketSink` are the traits that separate media processing from transport:

- `PacketSink::write(EncodedFrame)` accepts encoded frames. `MoqPacketSink` implements this by writing to a MoQ `OrderedProducer`, starting a new group on each keyframe.
- `PacketSource::read()` returns the next `MediaPacket`. `MoqPacketSource` reads from a MoQ `OrderedConsumer`.

For local loopback (testing, same-process relay), `PipeSink` and `PipeSource` connect a publisher to a subscriber without any network transport. This is used by the pipeline integration tests.

## API conventions

All public types use `&self` with interior mutability. Callers never need `&mut self`, which makes types safe to share across async tasks and threads.

All major types are `Clone` (Arc-based). Cloning a `LocalBroadcast` or `VideoTrack` gives another handle to the same underlying state.

Cleanup is drop-based. Dropping a `LocalBroadcast` tears down all encoder pipelines. Dropping a `VideoTrack` stops its decoder thread. No explicit shutdown calls are required.

## Relay support

`RemoteBroadcast` can relay to a `LocalBroadcast` without transcoding. The encoded packets pass from `PacketSource` to `PacketSink` directly, bypassing the decode and encode stages. `VideoTrack` implements `VideoSource`, so a transcoding relay (decode then re-encode at a different resolution or bitrate) is also possible by feeding the decoded frames into a new encoder pipeline.
