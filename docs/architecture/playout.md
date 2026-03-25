# Playout and sync

| Field | Value |
|-------|-------|
| Status | **sync disabled** — PTS pacing only |
| Applies to | moq-media |

Audio and video currently play independently with PTS-based frame pacing.
Active A/V synchronization was implemented, tested, and removed because it
performed strictly worse under congestion than unsynchronized playout. See
[A/V sync and playout](../av-sync/README.md) for the detailed rationale and
[plans/av-sync.md](../../plans/av-sync.md) for the research plan.

## Current building blocks

- **`FramePacer`** (video): sleeps between frames based on PTS deltas,
  clamped to 2× frame period to prevent long stalls after network gaps.
- **Audio decode loop**: pushes decoded samples to the cpal output sink
  immediately. The sink's ring buffer (~80ms) provides jitter smoothing.
  Silence is inserted when the buffer runs low.
- **`FreshnessPolicy`**: drives Hang's ordered consumer `max_latency`,
  controlling how aggressively stale groups are dropped before decode.
- **`LagTracker`**: records wall-vs-PTS drift in both paths. The video loop
  also computes `av_delta_ms = video_lag - audio_lag` for the overlay.

## Where to read next

- [A/V sync and playout](../av-sync/README.md) documents the current state
  and the history of why sync was removed.
- [A/V sync tuning](../av-sync/tuning.md) explains the freshness knobs.
