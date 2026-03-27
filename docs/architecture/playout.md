# Playout and sync

| Field | Value |
|-------|-------|
| Status | **sync enabled** — shared playout clock (ported from moq/js) |
| Applies to | moq-media |

## Overview

Audio and video share a playout clock (`moq_media::sync::Sync`) that
gates video frame output to a wall-clock target derived from network
arrival times. Audio paces itself through its ring buffer. The shared
reference and latency target keep the two streams aligned without
cross-path gating or signaling.

`SyncMode::Unmanaged` disables the clock and falls back to PTS-cadence
pacing (the `FramePacer`) for tests and file playback.

## Building blocks

- **`Sync`** (video path): records arrival offsets via `received(pts)`,
  blocks on `wait(pts)` until `reference + pts + latency` arrives.
  Ported from `moq/js` commit `53fe78d8`, `js/watch/src/sync.ts`.
- **`FramePacer`** (fallback, `SyncMode::Unmanaged`): sleeps between
  frames based on PTS deltas, clamped to 2× frame period to prevent
  long stalls after network gaps.
- **Audio decode loop**: pushes decoded samples to the cpal output sink
  immediately. The sink's ring buffer (~80 ms) smooths jitter. Silence
  is inserted when the buffer runs low.
- **`PlaybackPolicy::max_latency`**: drives Hang's ordered consumer,
  controlling how aggressively stale groups are dropped before decode.
- **`LagTracker`**: records wall-vs-PTS drift in both paths. The video
  loop computes `av_delta_ms = video_lag - audio_lag` for the overlay.

## Where to read next

- [A/V sync and playout](../av-sync/README.md) documents the algorithm,
  the history of earlier sync attempts, and the receive pipeline diagram.
- [A/V sync tuning](../av-sync/tuning.md) explains the playback policy
  knobs and the debug overlay metrics.
