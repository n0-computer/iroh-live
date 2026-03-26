# Publish Pipeline

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | moq-media |

`LocalBroadcast` manages encoder pipelines and publishes a catalog that
subscribers use to discover available renditions. It owns a
`BroadcastProducer` (from moq-lite) and coordinates video and audio
track lifecycles.

## Slot handles

`LocalBroadcast` exposes video and audio configuration through slot
handles:

- `broadcast.video()` returns a `VideoPublisher` (borrows the broadcast).
- `broadcast.audio()` returns an `AudioPublisher` (borrows the broadcast).

Both use `&self` (interior mutability). `VideoPublisher::set(input)` tears
down any existing video pipeline and installs the new one.
`VideoPublisher::clear()` removes video entirely. The audio side follows
the same pattern.

## VideoInput

`VideoInput` is an enum that unifies the two ways video enters the
pipeline:

**`VideoInput::Renditions(VideoRenditions)`** wraps a raw video source
with one or more encoder presets. Each preset produces a separate
rendition (simulcast layer) at a different quality level. The source
captures frames; the encoders compress them in parallel.

**`VideoInput::PreEncoded(Vec<PreEncodedTrack>)`** wraps one or more
pre-encoded video tracks that pass through without re-encoding. This
exists for hardware encoders that produce compressed output directly,
such as rpicam-vid on the Raspberry Pi, which outputs H.264 from its ISP.
Each `PreEncodedTrack` carries a name, a `VideoConfig` for the catalog,
and a factory closure that creates a fresh source instance per subscriber.

A single `VideoPublisher::set(input)` call handles both cases. The
internal `State` struct stores the `VideoInput` and branches at track
start time.

## VideoRenditions

`VideoRenditions` holds a `SharedVideoSource` and a map of rendition
names to encoder factories. When a subscriber requests a rendition, the
broadcast's run loop calls `start_encoder()`, which creates the encoder
from the factory and spawns a `VideoEncoderPipeline` on a dedicated OS
thread. The pipeline reads frames from the shared source and writes
encoded packets to a `MoqPacketSink`.

Multiple renditions share the same `SharedVideoSource` via
`watch::Receiver<Option<VideoFrame>>`. Each encoder sees the latest
frame; if an encoder is slower than the source, intermediate frames are
silently skipped by the watch channel's last-writer-wins semantics.

## SharedVideoSource

`SharedVideoSource` runs the capture source on a dedicated OS thread. It
parks (stops calling `pop_frame()`) when no subscribers are connected and
unparks when the first subscriber arrives. This avoids wasting CPU and
camera bandwidth when nobody is watching.

The source thread writes frames into a `watch::Sender`, and each encoder
thread holds a `watch::Receiver`. The watch channel always contains the
latest frame, so slow encoders never cause backpressure on the source.

## AudioRenditions

`AudioRenditions` follows the same pattern as `VideoRenditions`: a shared
audio source with per-rendition encoder factories. It supports two source
modes: a single `AudioSource` that is leased to one encoder at a time
(returned on drop), or a factory that creates independent sources per
rendition for parallel multi-rendition encoding.

## Encoder pipelines on OS threads

All encoder pipelines (`VideoEncoderPipeline`, `AudioEncoderPipeline`,
`PreEncodedVideoPipeline`) run on dedicated OS threads spawned via
`spawn_thread()`. This is a deliberate choice: codec operations are
CPU-intensive and sometimes block on hardware (VAAPI, V4L2), so running
them on tokio tasks would starve other async work. The threads
communicate with the async runtime through `mpsc` and `watch` channels.

## PacketSink and MoQ transport

`MoqPacketSink` wraps a hang `OrderedProducer` and implements the
`PacketSink` trait. When it receives an `EncodedFrame` with
`is_keyframe = true`, it calls `keyframe()` on the producer to start a
new MoQ group. Subsequent frames within the group are written
sequentially. This keyframe-to-group mapping is how subscribers can join
at any group boundary and decode from the keyframe forward.

## Catalog

`LocalBroadcast` maintains a catalog track (hang's built-in catalog
mechanism) that lists all available video and audio renditions with their
codec configuration, dimensions, and bitrate. The catalog is updated
whenever video or audio is set or cleared. Subscribers watch the catalog
to discover what renditions exist and select the best match.

## Demand-driven track startup

The broadcast's run loop (`LocalBroadcast::run`) calls
`producer.requested_track().await` to wait for subscriber demand. When a
subscriber requests a specific rendition, the loop looks it up in the
current `VideoInput` or `AudioRenditions` and starts the corresponding
encoder pipeline. When all subscribers disconnect (tracked via
`track.unused().await`), the pipeline is stopped. This means encoder
threads only run when someone is actually consuming the output.

## Open areas

There is no backpressure from the transport to the encoder. If the
network cannot keep up with the encoder's output rate, packets queue in
the MoQ transport layer. The adaptive rendition switching on the subscribe
side compensates by switching to lower renditions, but the publisher does
not currently reduce its encoding rate in response to congestion.

`VideoPublisher::set_enabled()` and `AudioPublisher::set_muted()` are
not yet implemented (marked as TODOs in the source). They are intended
to pause/resume the encoder pipeline and send silence, respectively.

## Local preview

`LocalBroadcast::preview()` creates a `VideoTrack` that decodes the
local broadcast's own output, useful for showing the publisher what
subscribers see. Returns `None` for pre-encoded sources or when no
video is set.
