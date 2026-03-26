# A/V sync and playout

| Field | Value |
|-------|-------|
| Status | **sync disabled** — PTS pacing only, no active A/V alignment |
| Applies to | moq-media |

## Current state

Audio and video decode and render independently. Each stream paces its output
based on PTS cadence:

- **Video:** `FramePacer` sleeps between frames proportionally to the PTS delta
  from the previous frame, clamped to 2× frame period to avoid long stalls
  after network gaps.
- **Audio:** the audio decode loop pushes samples to the cpal output sink as
  fast as they arrive. The sink's internal ring buffer (~80ms at 48kHz)
  provides the front-buffering that smooths jitter. Silence is inserted when
  the buffer runs low.

There is no active mechanism to align video timing to audio timing. The A/V
delta metric in the debug overlay reports the drift, but nothing corrects it.

## Why sync was removed

Three successive sync architectures were implemented and tested under
congestion, latency spikes, and loss:

1. **Shared `PlayoutClock` / `SyncClock`** — a single reference clock that
   both audio and video anchored to and re-anchored on stalls. Failed because
   the two streams observe different delays, and re-anchoring one stream
   destabilized the other.

2. **Audio-gated video** — audio decode loop sleeps controlled video release
   timing. Failed because audio continuity is more important than forcing it
   to serve as a video gate, and gaps in the audio stream propagated as video
   freezes.

3. **Audio-master with `VideoSyncController`** — audio plays freely, video
   holds or drops frames based on estimated speaker position
   (`AudioPosition`). The most successful approach, but still performed
   strictly worse than unsynchronized PTS pacing under all congestion
   scenarios tested. Recovery from stalls was slower, and the hold/drop
   decisions introduced visible artifacts (micro-freezes, frame skips) that
   raw PTS pacing avoided.

Each approach was implemented, tested with patchbay network simulation at
0/50/200/500ms latency and 0-30% loss, and evaluated against the baseline of
unsynchronized PTS pacing. In all cases, the sync machinery added complexity
without measurably improving the user experience during normal operation, and
actively degraded it during congestion.

## What remains

- `PlaybackPolicy` and `FreshnessPolicy` are still in the public API.
  `FreshnessPolicy::max_stale_duration` drives Hang's ordered consumer and
  is the primary congestion-response knob.
- `SyncMode` exists in the type system with only the `Unmanaged` variant.
  The `AudioMaster` variant was removed.
- The `LagTracker` in each decode loop records wall-vs-PTS drift. The video
  loop also computes A/V delta as `video_lag - audio_lag` and records it to
  `TimingStats::av_delta_ms`.
- The debug overlay shows AudioBuf, VideoLag, AudioLag, A/V Δ, and VideoBuf
  in the Time detail panel.

## Receive pipeline (current)

```text
OrderedConsumer (hang, max_latency = freshness)
    ↓
forward_packets() async task → mpsc channel
    ↓
┌──────────────────────┐   ┌──────────────────────┐
│  Audio decode thread │   │  Video decode thread  │
│  push_packet()       │   │  push_packet()        │
│  pop_samples()       │   │  pop_frame()          │
│  sink.push_samples() │   │  VecDeque buffer      │
│  10ms tick cadence   │   │  FramePacer (PTS)     │
│  silence on underrun │   │  frame_channel send   │
└──────────────────────┘   └──────────────────────┘
         ↓                            ↓
   cpal output ring              egui/wgpu renderer
         ↓
      speakers
```

Both paths are independent. No shared timing state, no cross-path
coordination, no skip-generation signaling.

## Future direction

See [plans/av-sync.md](../../plans/av-sync.md) for the research plan.
