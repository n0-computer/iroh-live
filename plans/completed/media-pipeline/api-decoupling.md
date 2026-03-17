# API Decoupling: Standalone Encoders & Decoders

## Problem

`moq-media` encoders and decoders are tightly coupled to hang/moq-lite transport types:

1. **`VideoDecoder` / `AudioDecoder` traits** use `OrderedFrame` (hang) as the packet type
2. **`subscribe.rs`** takes `OrderedConsumer` directly to feed decoders
3. **`publish.rs`** takes `TrackProducer` directly to drain encoders
4. **No way to use encode/decode pipelines without MoQ networking**

This makes it impossible to:
- Run a local capture->encode->decode->display pipeline (useful for testing, previews, benchmarking)
- Use moq-media's codec infrastructure from non-MoQ applications
- Unit test encode/decode round-trips without spinning up MoQ transport

## Goal

- `VideoEncoderPipeline` / `VideoDecoderPipeline` as public structs that own threads and can be used standalone
- A trait abstracting over hang/moq-lite producers/consumers at the API edge
- Implementations for hang types AND for an in-memory pipe
- Existing MoQ-based code in `subscribe.rs` / `publish.rs` refactored to use the new traits

---

## Design

### Layer 1: Own Packet Type

Replace `OrderedFrame` in decoder/encoder traits with our own `MediaPacket`:

```rust
// moq-media/src/format.rs

/// An encoded media packet, independent of transport.
#[derive(Clone, Debug)]
pub struct MediaPacket {
    /// Presentation timestamp.
    pub timestamp: Duration,
    /// Encoded payload.
    pub payload: bytes::Bytes,
    /// Whether this is a keyframe (first frame of a new group).
    pub is_keyframe: bool,
}
```

**Conversion from `OrderedFrame`:**
```rust
impl From<OrderedFrame> for MediaPacket {
    fn from(f: OrderedFrame) -> Self {
        Self {
            timestamp: f.timestamp.into(), // Timestamp -> Duration
            payload: f.payload.copy_to_bytes(f.payload.remaining()),
            is_keyframe: f.is_keyframe(),
        }
    }
}
```

**Conversion to hang `Frame`** (for encoder output → transport):
```rust
impl From<&MediaPacket> for hang::container::Frame {
    fn from(p: &MediaPacket) -> Self {
        Frame {
            timestamp: Timestamp::from(p.timestamp),
            payload: BufList::from(p.payload.clone()),
        }
    }
}
```

### Layer 2: Transport Traits

```rust
// moq-media/src/transport.rs (new module)

/// Async source of encoded media packets.
/// Implemented by OrderedConsumer (network) and PipeReceiver (local).
pub trait PacketSource: Send + 'static {
    fn read(&mut self) -> impl Future<Output = anyhow::Result<Option<MediaPacket>>> + Send;
}

/// Sync sink for encoded media packets.
/// Implemented by OrderedProducer (network) and PipeSender (local).
pub trait PacketSink: Send + 'static {
    fn keyframe(&mut self) -> anyhow::Result<()>;
    fn write_packet(&mut self, packet: EncodedFrame) -> anyhow::Result<()>;
    fn finish(&mut self) -> anyhow::Result<()>;
}
```

**Impl for hang `OrderedConsumer`:**
```rust
impl PacketSource for OrderedConsumer {
    async fn read(&mut self) -> Result<Option<MediaPacket>> {
        match self.read().await {
            Ok(Some(frame)) => Ok(Some(frame.into())),
            Ok(None) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
```

Problem: `OrderedConsumer::read` and `PacketSource::read` name collision. Solution: newtype wrapper:

```rust
/// Wraps an OrderedConsumer as a PacketSource.
pub struct MoqPacketSource(pub OrderedConsumer);

impl PacketSource for MoqPacketSource {
    async fn read(&mut self) -> Result<Option<MediaPacket>> {
        match self.0.read().await {
            Ok(Some(frame)) => Ok(Some(frame.into())),
            Ok(None) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
```

**Impl for hang `OrderedProducer`:**
```rust
/// Wraps an OrderedProducer as a PacketSink.
pub struct MoqPacketSink(pub OrderedProducer);

impl PacketSink for MoqPacketSink {
    fn keyframe(&mut self) -> Result<()> {
        self.0.keyframe().map_err(Into::into)
    }

    fn write_packet(&mut self, packet: EncodedFrame) -> Result<()> {
        if packet.is_keyframe {
            self.0.keyframe()?;
        }
        self.0.write(packet.frame).map_err(Into::into)
    }

    fn finish(&mut self) -> Result<()> {
        self.0.finish().map_err(Into::into)
    }
}
```

### Layer 3: In-Memory Pipe

For local encode->decode without network:

```rust
// moq-media/src/transport.rs

use tokio::sync::mpsc;

/// Create an in-memory media pipe.
/// Encoded packets flow from PipeSink to PipeSource without network.
pub fn media_pipe(capacity: usize) -> (PipeSink, PipeSource) {
    let (tx, rx) = mpsc::channel(capacity);
    (PipeSink(tx), PipeSource(rx))
}

pub struct PipeSink(mpsc::Sender<MediaPacket>);

impl PacketSink for PipeSink {
    fn keyframe(&mut self) -> Result<()> {
        Ok(()) // No-op: keyframe signaling is in the packet itself
    }

    fn write_packet(&mut self, packet: EncodedFrame) -> Result<()> {
        let media_packet = MediaPacket {
            timestamp: packet.frame.timestamp.into(),
            payload: packet.frame.payload.copy_to_bytes(packet.frame.payload.remaining()),
            is_keyframe: packet.is_keyframe,
        };
        self.0.blocking_send(media_packet)
            .map_err(|_| anyhow::anyhow!("pipe closed"))
    }

    fn finish(&mut self) -> Result<()> {
        Ok(()) // Dropping the sender closes the pipe
    }
}

pub struct PipeSource(mpsc::Receiver<MediaPacket>);

impl PacketSource for PipeSource {
    async fn read(&mut self) -> Result<Option<MediaPacket>> {
        Ok(self.0.recv().await)
    }
}
```

### Layer 4: Update Codec Traits

Change `VideoDecoder` and `AudioDecoder` to use `MediaPacket`:

```rust
// moq-media/src/traits.rs

pub trait VideoDecoder: Send + 'static {
    fn new(config: &VideoConfig, playback_config: &DecodeConfig) -> Result<Self>
    where Self: Sized;
    fn name(&self) -> &str;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn pop_frame(&mut self) -> Result<Option<DecodedVideoFrame>>;
    fn set_viewport(&mut self, w: u32, h: u32);
}

pub trait AudioDecoder: Send + 'static {
    fn new(config: &AudioConfig, target_format: AudioFormat) -> Result<Self>
    where Self: Sized;
    fn push_packet(&mut self, packet: MediaPacket) -> Result<()>;
    fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}
```

Each decoder impl changes from:
```rust
fn push_packet(&mut self, mut packet: OrderedFrame) -> Result<()> {
    let payload = packet.payload.copy_to_bytes(packet.payload.remaining());
```
to:
```rust
fn push_packet(&mut self, packet: MediaPacket) -> Result<()> {
    let payload = packet.payload;
```

This is a mechanical change. The `payload` is already `Bytes` so codec code gets simpler.

Similarly for `EncodedFrame` output from encoders - this currently wraps `hang::container::Frame`. We should make it transport-independent:

```rust
// moq-media/src/format.rs

pub struct EncodedFrame {
    pub is_keyframe: bool,
    pub timestamp: Duration,
    pub payload: bytes::Bytes,
}
```

Then the `PacketSink` impls handle the conversion to hang Frame or pipe MediaPacket.

### Layer 5: Pipeline Structs

These are the high-level public types that own encode/decode threads:

```rust
// moq-media/src/pipeline.rs (new module)

/// A video decoder pipeline. Reads packets from any PacketSource,
/// decodes on an OS thread, outputs DecodedVideoFrames.
pub struct VideoDecoderPipeline {
    pub frames: VideoTrackFrames,
    pub handle: VideoTrackHandle,
}

impl VideoDecoderPipeline {
    /// Create a new pipeline from any packet source.
    pub fn new<D: VideoDecoder>(
        name: String,
        source: impl PacketSource,
        config: &VideoConfig,
        decode_config: &DecodeConfig,
    ) -> Result<Self> {
        let shutdown = CancellationToken::new();
        let (packet_tx, packet_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let viewport = Watchable::new((1u32, 1u32));

        let decoder = D::new(config, decode_config)?;
        let decoder_name = decoder.name().to_string();
        let target_pixel_format = decode_config.pixel_format;

        // Async task: read from PacketSource → mpsc
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                forward_packets(source, packet_tx, &shutdown).await;
            }
        });

        // OS thread: mpsc → decode → frame output
        let thread = spawn_thread(format!("vdec-{name}"), {
            let shutdown = shutdown.clone();
            let viewport_watcher = viewport.watch();
            move || {
                VideoTrack::run_loop(
                    &shutdown, packet_rx, frame_tx,
                    viewport_watcher, decoder, target_pixel_format,
                ).ok();
                shutdown.cancel();
            }
        });

        let guard = VideoTrackGuard { /* ... */ };
        Ok(Self {
            frames: VideoTrackFrames { rx: frame_rx },
            handle: VideoTrackHandle {
                rendition: name,
                decoder_name,
                viewport,
                _guard: guard,
            },
        })
    }
}

/// A video encoder pipeline. Reads from a VideoSource,
/// encodes on an OS thread, writes to any PacketSink.
pub struct VideoEncoderPipeline {
    shutdown: CancellationToken,
    _thread: thread::JoinHandle<()>,
}

impl VideoEncoderPipeline {
    pub fn new(
        source: impl VideoSource,
        encoder: impl VideoEncoder,
        sink: impl PacketSink,
    ) -> Self {
        let shutdown = CancellationToken::new();
        let thread = spawn_thread("venc", {
            let shutdown = shutdown.clone();
            move || {
                // Same logic as EncoderThread::spawn_video but writes to PacketSink
                run_video_encode_loop(source, encoder, sink, &shutdown);
            }
        });
        Self { shutdown, _thread: thread }
    }
}

impl Drop for VideoEncoderPipeline {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// Audio equivalents
pub struct AudioDecoderPipeline { /* ... */ }
pub struct AudioEncoderPipeline { /* ... */ }
```

### Layer 6: Refactor subscribe.rs & publish.rs

**subscribe.rs:**
- `VideoTrack::from_consumer` becomes a thin wrapper that creates a `MoqPacketSource` and delegates to `VideoDecoderPipeline::new`
- `forward_frames` replaced by generic `forward_packets` that works with any `PacketSource`
- `VideoTrack::run_loop` stays as-is (it already takes `mpsc::Receiver<MediaPacket>` after the trait change)

**publish.rs:**
- `EncoderThread::spawn_video` becomes a thin wrapper that creates a `MoqPacketSink` and delegates to `VideoEncoderPipeline::new`
- Or `EncoderThread` is rewritten in terms of `VideoEncoderPipeline` internally

The existing `SubscribeBroadcast` and `PublishBroadcast` remain MoQ-aware (they manage catalog, track selection, etc.). The decoupling is at the encode/decode level.

### Layer 7: Local Loopback Helper

Convenience for local encode->decode without network:

```rust
// moq-media/src/pipeline.rs

/// Local video pipeline: capture → encode → decode → display.
/// No network involved.
pub struct LocalVideoPipeline {
    pub encoder: VideoEncoderPipeline,
    pub decoder: VideoDecoderPipeline,
}

impl LocalVideoPipeline {
    pub fn new<E: VideoEncoder, D: VideoDecoder>(
        source: impl VideoSource,
        encoder: E,
        video_config: &VideoConfig,
        decode_config: &DecodeConfig,
    ) -> Result<Self> {
        let (sink, source) = media_pipe(32);
        let encoder = VideoEncoderPipeline::new(source_video, encoder, sink);
        let decoder = VideoDecoderPipeline::new::<D>(
            "local".into(), source, video_config, decode_config,
        )?;
        Ok(Self { encoder, decoder })
    }
}
```

---

## Implementation Order

Each step leaves everything compiling + tests passing.

### Step 1: Add `MediaPacket` type + conversion impls
- Add to `format.rs`
- Add `From<OrderedFrame>` impl

### Step 2: Add `transport.rs` module
- `PacketSource`, `PacketSink` traits
- `MoqPacketSource`, `MoqPacketSink` wrappers
- `media_pipe()`, `PipeSink`, `PipeSource`

### Step 3: Update `VideoDecoder` / `AudioDecoder` traits
- Change `push_packet(OrderedFrame)` → `push_packet(MediaPacket)`
- Update all codec impls (h264, av1, vaapi, opus decoders)
- This is mechanical: each just stops calling `copy_to_bytes` since payload is already `Bytes`

### Step 4: Make `EncodedFrame` transport-independent
- Change from `hang::container::Frame` to own `timestamp: Duration` + `payload: Bytes`
- Update all encoder impls
- Update `MoqPacketSink` to convert back to hang Frame

### Step 5: Add `pipeline.rs` module
- `VideoDecoderPipeline`, `VideoEncoderPipeline`
- `AudioDecoderPipeline`, `AudioEncoderPipeline`
- `LocalVideoPipeline` helper
- Generic `forward_packets` async fn

### Step 6: Refactor `subscribe.rs`
- `VideoTrack::from_consumer` delegates to `VideoDecoderPipeline`
- `AudioTrack::spawn` delegates to `AudioDecoderPipeline`
- Remove direct `OrderedFrame`/`OrderedConsumer` usage from decode loops

### Step 7: Refactor `publish.rs`
- `EncoderThread::spawn_video` delegates to `VideoEncoderPipeline`
- `EncoderThread::spawn_audio` delegates to `AudioEncoderPipeline`
- Remove direct `OrderedProducer` usage from encode loops

### Step 8: Export new public API
- `pub mod transport;` in `lib.rs`
- `pub mod pipeline;` in `lib.rs`
- Re-export key types

---

## Module Structure After Refactor

```
moq-media/src/
  lib.rs
  format.rs          # MediaPacket, EncodedFrame (transport-independent)
  traits.rs          # VideoDecoder, VideoEncoder, etc. (use MediaPacket)
  transport.rs       # NEW: PacketSource, PacketSink, MoqPacketSource/Sink, media_pipe
  pipeline.rs        # NEW: VideoDecoderPipeline, VideoEncoderPipeline, Local*Pipeline
  subscribe.rs       # SubscribeBroadcast (MoQ-aware, delegates to pipeline)
  publish.rs         # PublishBroadcast (MoQ-aware, delegates to pipeline)
  codec/             # Codec impls (use MediaPacket)
  capture/           # Video/audio sources
  render.rs          # wgpu renderer (unchanged)
  ...
```

---

## Example: moq-media standalone viewer

See `plans/media-pipeline/standalone-viewer-example.md` for the egui example that exercises
this decoupled API with local capture, codec selection, and debug stats.

---

## Risks & Notes

- **Timestamp conversion**: `hang::Timestamp` is microseconds (`Timescale<1_000_000>`). Converting to/from `Duration` is lossless for our use case.
- **BufList → Bytes**: `OrderedFrame.payload` is `BufList`. Converting to `Bytes` via `copy_to_bytes` is a copy. For the pipe path this is fine. For the MoQ path, this copy already happens in every decoder today.
- **`EncodedFrame.frame` is `hang::container::Frame`**: Changing this to own types means encoder impls no longer produce hang types directly. The `MoqPacketSink` handles conversion. This is clean but changes every encoder.
- **Backward compat**: `subscribe.rs` and `publish.rs` public APIs don't change. Internal wiring changes.
- **`run_loop` packet type**: Currently `run_loop` takes `mpsc::Receiver<OrderedFrame>`. After Step 3, it takes `mpsc::Receiver<MediaPacket>`. The `forward_packets` function converts at the boundary.
