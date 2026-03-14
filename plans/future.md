# Future Features

Potential features not yet covered by existing plans, ordered by impact. Each
section starts with a use case, follows with an architecture sketch, and ends
with an effort estimate.

## Recording and playback

A teacher records a lecture for students who missed it. A support engineer
replays a customer's screen-share to investigate a bug report.

iroh-live currently has no recording path. The cleanest approach is a
`RecordingSink` that consumes encoded `MediaPacket`s from the same pipeline
the transport uses. Each group becomes a segment written to a container — MP4
fragmented (fMP4) is the natural fit because its segment boundaries align with
hang groups (one keyframe per group). The sink writes to an `AsyncWrite`
implementor, so callers can target local files, S3 via `object_store`, or any
custom backend.

Playback is the reverse: a `RecordingSource` implements `PacketSource` and
feeds segments back through the existing decode pipeline. The playout clock
already handles PTS mapping, so playback at 1x speed works out of the box.
Variable-speed playback (2x, 0.5x) requires scaling PTS before feeding the
clock — straightforward arithmetic.

**Effort**: ~400 lines for fMP4 mux/demux via the `mp4` crate, ~200 lines for
sink/source wrappers, ~100 lines for `Live::record()` API surface.

## Data channels

Two players synchronize game state alongside voice chat. A collaborative
editor sends cursor positions and document deltas in real time.

moq-lite already supports arbitrary named tracks. A data channel is a track
whose payload is application bytes instead of encoded media. The catalog
already distinguishes video and audio renditions; adding a `data` section is a
one-field extension. The subscriber side needs a `DataTrack` type that exposes
an async byte stream without running it through a decoder.

The transport layer (hang groups) provides ordering and reliability. For
unreliable data (game state that tolerates loss), we can expose a raw
`TrackConsumer` without `OrderedConsumer` wrapping.

**Effort**: ~150 lines for `DataTrack` in moq-media, ~50 lines for catalog
extension, ~100 lines for `LocalBroadcast::data()` publish API.

## Noise suppression

A user joins a call from a coffee shop. Background noise makes them hard to
understand.

The `rnnoise` crate wraps the RNNoise library (BSD, ~100KB model) and
processes 48kHz mono audio in 10ms frames — the same frame size Opus uses.
Integration point: a `NoiseSuppressor` processing node inserted between the
audio source and the Opus encoder in `AudioEncoderPipeline`. It runs on the
encoder thread, adding ~0.5ms of CPU per frame on modern hardware.

The suppressor should be toggleable at runtime via `AudioPublisher::set_noise_suppression(bool)`.
A `sonora`-based processing graph would also work but adds complexity we do
not need for a single-node pipeline.

**Effort**: ~200 lines. The `rnnoise` crate does the heavy lifting; we wire
it into the audio pipeline and expose a toggle.

## Echo cancellation

A user on laptop speakers hears their own voice echoed back. The far-end
audio leaks into the microphone and creates a feedback loop.

Echo cancellation requires a reference signal (the audio being played to the
speaker) and the microphone signal. The `speexdsp` crate provides a
well-tested AEC implementation. We already have an `aec.rs` module in
`audio_backend/` — it contains a partial implementation that needs completion.

The hard part is aligning the reference signal's timing with the microphone
capture. PipeWire and CoreAudio both provide loopback/monitor streams that
are hardware-synchronized with the output. On platforms without loopback, we
feed the decoded far-end audio back as the reference.

**Effort**: ~300 lines to complete `aec.rs`, ~100 lines for platform-specific
loopback capture.

## HLS/DASH egress for large audiences

A conference keynote needs to reach 10,000 viewers. Direct QUIC connections
do not scale to that audience size.

The approach: a server-side `EgressService` subscribes to a broadcast,
remuxes `MediaPacket` groups into CMAF segments (fMP4 with ISOBMFF), and
writes them to an HTTP origin. Each group is already a keyframe-aligned
segment, so the mapping is direct. The origin serves an HLS manifest
(`.m3u8`) and DASH MPD that reference the CMAF segments.

This is server-side infrastructure, not a client library feature. It belongs
in a separate `iroh-live-egress` crate or binary. The CMAF muxer shares code
with the recording sink (both produce fMP4).

**Effort**: ~800 lines for CMAF muxer + manifest generation, ~200 lines for
HTTP origin. Depends on the recording feature.

## Relay / SFU

Three participants in a call. Without a relay, each sends their stream to
every other participant — upload bandwidth scales linearly with participant
count.

A relay (Selective Forwarding Unit) receives each participant's encoded stream
once and fans it out to all subscribers. The relay never decodes — it forwards
`MediaPacket`s and catalog updates. The existing `plans/api/6-relay.md`
covers the design. The relay is a moq-lite `BroadcastConsumer` on the ingest
side and a `BroadcastProducer` on the egress side, with track-level
subscription routing.

iroh's QUIC transport gives us connection migration and 0-RTT reconnects for
free. The relay can run on commodity cloud infrastructure.

**Effort**: ~600 lines for relay binary, ~200 lines for client-side relay
discovery and connection helpers.

## Bandwidth estimation and congestion feedback

A user on a cellular connection moves from Wi-Fi to LTE. The available
bandwidth drops from 10 Mbps to 2 Mbps. The encoder should adapt before
packet loss becomes visible.

We already have `NetworkSignals` (bandwidth, RTT, loss) polled from QUIC
`PathStats`. The missing piece is a proper bandwidth estimator that smooths
QUIC's cwnd-derived bandwidth into a usable target bitrate. The existing
`adaptive.rs` uses raw bandwidth; a Kalman filter or exponential moving
average would reduce oscillation.

The encoder side (phase 3d in existing plans) needs a `set_bitrate()` method
on the `VideoEncoder` trait. openh264 supports runtime bitrate changes;
rav1e does not (requires encoder restart). VAAPI and VideoToolbox support
dynamic rate control natively.

**Effort**: ~200 lines for bandwidth estimator, ~300 lines for encoder rate
control (covered in `phase-3d-adaptive-encoding.md`).

## End-to-end encryption

A medical consultation where the relay must not see video content. Transport
encryption (QUIC TLS) protects against network eavesdropping but not against
a compromised relay.

E2EE in SFU architectures uses "insertable streams" — encrypt each media
frame before handing it to the transport, decrypt after receiving. The relay
forwards opaque ciphertext. Key exchange uses a separate signaling channel
(the room's data track or an out-of-band mechanism).

SFrame (RFC 9605) is the emerging standard. Each frame gets a short header
(key ID + counter) followed by AES-GCM ciphertext. The codec header
(SPS/PPS for H.264) can remain in the clear so the relay can still make
forwarding decisions based on codec metadata.

**Effort**: ~500 lines for SFrame encrypt/decrypt, ~200 lines for key
management, ~100 lines for API surface.

## Virtual backgrounds

A user wants to hide their messy room during a video call.

Requires real-time person segmentation. On GPU-capable devices, a lightweight
model (MediaPipe Selfie Segmentation, ~1MB) runs inference per frame and
produces a mask. The mask composites the person over a static image or blur.

This is a `VideoProcessor` inserted between capture and encode. On devices
without GPU compute, CPU fallback exists but is expensive (15–20ms per 720p
frame). The feature should be optional and behind a `virtual-bg` feature
flag.

Integration with wgpu: the segmentation model runs as a compute shader, and
compositing is a second render pass. Both happen before the frame reaches the
encoder.

**Effort**: ~1000 lines for segmentation model integration, ~300 lines for
compositing shader, ~200 lines for API. Largest single feature on this list.

## Stats and metrics API

A developer building a video call app needs to show connection quality
indicators and debug network issues.

We already expose `PlayoutClock::jitter()`, `reanchor_stats()`, and QUIC
`PathStats` through `StatsSmoother`. The gap is a unified `Stats` struct that
aggregates all relevant metrics into a single snapshot: encode/decode FPS,
frame drop count, bitrate (up/down), RTT, loss rate, jitter, buffer depth,
active rendition, codec name.

The struct should be watchable via `n0_watcher::Watchable<Stats>` so UI
components can subscribe to changes without polling.

**Effort**: ~200 lines for the `Stats` struct and aggregation, ~100 lines for
watchable integration.

## Simulcast vs. SVC

A mobile viewer on a slow connection receives a 180p stream while a desktop
viewer gets 1080p — from the same publisher encoding once.

We currently use simulcast: the publisher encodes each resolution
independently. SVC (Scalable Video Coding) encodes once with temporal or
spatial layers, and the relay strips layers to match each subscriber's
bandwidth. AV1 has native SVC support; H.264 SVC is poorly supported in
hardware decoders.

SVC requires relay support (layer stripping) and changes to the catalog
format (layer descriptions instead of separate renditions). The payoff is
lower publisher CPU and upload bandwidth. The tradeoff: more complex relay
logic and codec-specific layer parsing.

**Effort**: ~500 lines for AV1 SVC encoding (rav1e supports temporal SVC),
~300 lines for relay layer stripping, ~200 lines for catalog changes. Medium
complexity but high dependency on relay infrastructure.

## Multi-codec negotiation

A mobile client prefers hardware H.264 decoding. A desktop client prefers AV1
for better quality at the same bitrate. The publisher should encode both and
let each subscriber choose.

The catalog already supports multiple video renditions with different codecs.
The subscriber's `select_video_rendition()` could factor in decoder
availability: prefer renditions whose codec the local device can decode in
hardware. This is a policy change in `AdaptiveVideoTrack`, not a protocol
change.

**Effort**: ~100 lines to add codec preference to rendition selection.
