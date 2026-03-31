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
# A/V sync tuning

## The two knobs

`PlaybackPolicy` has two fields that control playout behavior:

- **`sync: SyncMode`** — `Synced` (default) enables the shared playout
  clock; `Unmanaged` falls back to PTS-cadence pacing with no
  cross-track alignment.
- **`max_latency: Duration`** — maximum span of buffered media before
  Hang's ordered consumer skips forward. Default 150 ms.

```rust
PlaybackPolicy::default()              // Synced, 150 ms
    .with_max_latency(Duration::from_millis(500))
```

### When to increase `max_latency`

- Continuity matters more than fast recovery.
- You are on a stable but high-latency path.
- Short stalls should not immediately skip over content.

### When to decrease `max_latency`

- You care more about returning to the live edge quickly.
- Congestion or loss produces long stale runs that are no longer useful.
- You are debugging recovery after latency spikes.

## Changing the policy at runtime

`RemoteBroadcast::set_playback_policy` updates the policy for future
track subscriptions. Already-running pipelines keep whatever policy
they were created with. Typical pattern in a UI resubscribe flow:

```rust
broadcast.set_playback_policy(
    broadcast.playback_policy().with_sync(SyncMode::Unmanaged),
);
// now resubscribe to tracks — new pipelines use Unmanaged
```

## Reading the overlay

The Time section shows:

- **AudioBuf**: audio output ring buffer fill level in ms. Below ~20 ms
  risks underruns. Above ~200 ms means audio is queuing up faster than
  it plays.
- **VideoLag**: wall-clock drift from video PTS cadence since the first
  rendered frame. Positive means video is running behind real-time.
- **AudioLag**: same for the audio path.
- **A/V Δ**: `VideoLag - AudioLag`. Positive means video is behind
  audio. Under normal conditions this stays within ±30 ms.
- **VideoBuf**: decoded frames waiting in the video decode buffer.
  Typically 0–3 during normal playback, higher during DPB bursts.

What the numbers tell you:

- **AudioBuf dropping to zero**: network stall or decode cannot keep up.
  Audio will underrun and silence gets inserted. Check `max_latency` and
  network conditions.
- **VideoLag climbing steadily**: video decode or rendering cannot keep
  up with real-time. Check decoder FPS and decode_ms in the Render
  section.
- **A/V Δ large but both lags small**: the streams are near real-time
  but drifting apart. The shared playout clock should keep this tight;
  if it is not, check whether `SyncMode::Synced` is active.
- **Decoder FPS unstable**: the problem may be decode throughput or
  renderer cadence, not playout policy.
