# A/V sync and playout

| Field | Value |
|-------|-------|
| Status | **sync enabled** — shared playout clock (ported from moq/js) |
| Applies to | moq-media |

## Current state

A shared playout clock (`moq-media::sync::Sync`) coordinates video
frame timing. The clock is ported from the moq/js player (`js/watch/src/sync.ts`,
commit `53fe78d8`) and uses the same algorithm.

- **Video:** the decode loop calls `Sync::received(pts)` on each packet
  from the ordered consumer, recording the wall-clock arrival offset.
  After decoding, `Sync::wait(pts)` blocks until `reference + pts +
  latency` arrives, then emits the frame. This replaces the earlier
  PTS-cadence `FramePacer`.
- **Audio:** the decode loop pushes samples to the cpal output sink as
  fast as they arrive. The sink's internal ring buffer (~80 ms at 48 kHz)
  smooths jitter. Silence is inserted when the buffer runs low. Audio
  does *not* call `received` or `wait` — it runs independently, matching
  the JS player's design.

Both paths share the same `Sync` instance through `PipelineContext`.
The `Sync::latency` value (default 100 ms jitter buffer) determines
how far ahead of real-time the clock buffers before releasing frames.

`SyncMode::Unmanaged` disables the shared clock and falls back to
PTS-cadence pacing for tests, file playback, and single-track
scenarios.

## History: three earlier sync attempts

Before the JS port, three successive sync architectures were implemented
and tested under congestion, latency spikes, and loss:

1. **Shared `PlayoutClock` / `SyncClock`** — a single reference clock
   that both audio and video anchored to and re-anchored on stalls.
   Failed because the two streams observe different delays, and
   re-anchoring one stream destabilized the other.

2. **Audio-gated video** — audio decode loop controlled video release
   timing. Failed because audio continuity is more important than
   forcing it to serve as a video gate, and gaps in the audio stream
   propagated as video freezes.

3. **Audio-master with `VideoSyncController`** — audio plays freely,
   video holds or drops frames based on estimated speaker position.
   The most successful approach, but still performed strictly worse
   than unsynchronized PTS pacing under all congestion scenarios
   tested.

These were removed in `3e2c0de` and `9c72f6a`. Code preserved on
`sync-redesign-backup` branch.

## What the current approach does differently

The JS-ported `Sync` takes a simpler approach than any of the three
above. It does not try to synchronize video *to* audio. Instead, it
establishes a single reference offset (the earliest wall-minus-PTS ever
observed) and buffers both streams by the same latency target. Audio
paces itself through its ring buffer; video paces itself through
`Sync::wait`. The two converge because they share the reference and
the latency, without any cross-path signaling or gating.

## Remaining infrastructure

- `PlaybackPolicy` controls sync mode and the `max_latency` threshold
  for Hang's ordered consumer.
- `LagTracker` in each decode loop records wall-vs-PTS drift. The video
  loop computes A/V delta as `video_lag - audio_lag` and records it to
  `TimingStats::av_delta_ms`.
- The debug overlay shows AudioBuf, VideoLag, AudioLag, A/V Δ, and
  VideoBuf in the Time detail panel.

## Receive pipeline

```text
OrderedConsumer (hang, max_latency from PlaybackPolicy)
    ↓
forward_packets() async task → mpsc channel
    ↓
┌──────────────────────┐   ┌──────────────────────────┐
│  Audio decode thread │   │  Video decode thread      │
│  push_packet()       │   │  sync.received(pts)       │
│  pop_samples()       │   │  push_packet() / pop()    │
│  sink.push_samples() │   │  sync.wait(pts)           │
│  10ms tick cadence   │   │  frame_channel send       │
│  silence on underrun │   │                           │
└──────────────────────┘   └──────────────────────────┘
         ↓                            ↓
   cpal output ring              egui/wgpu renderer
         ↓
      speakers
```

Audio and video share the `Sync` instance but do not coordinate
directly. The shared reference and latency target align their timing
without cross-path signaling.

## Future direction

See [plans/av-sync.md](../../plans/av-sync.md) for research notes.
