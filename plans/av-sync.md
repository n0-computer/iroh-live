# A/V sync — research plan

## Status: disabled, research topic

Active A/V synchronization was removed (`3e2c0de`, `9c72f6a`) after three
implementation attempts that all performed worse than unsynchronized PTS-based
pacing under congestion. Code preserved on `sync-redesign-backup` branch.

## What works today

Audio and video each pace output based on PTS cadence, with no cross-path
coordination. `LagTracker` in each decode loop records wall-vs-PTS drift;
`av_delta_ms = video_lag - audio_lag` is visible in the debug overlay. Under
typical conditions the delta stays within +/-30ms and self-corrects on
congestion recovery.

Transport-level freshness (`FreshnessPolicy::max_stale_duration`) controls how
far behind live we fall before Hang drops stale groups.

## What was tried

Three approaches failed for the same root cause: any mechanism that delays,
holds, or skips video frames during congestion recovery makes the experience
worse than raw PTS pacing.

1. **Shared PlayoutClock** — audio and video fought over the reference point on
   re-anchor after stalls, causing oscillation.
2. **Audio-gated video** — audio gaps propagated as video freezes even when
   video packets were available.
3. **Audio-master with VideoSyncController** — most complete attempt; under
   congestion the hold/drop decisions created micro-freezes and skips that raw
   PTS pacing avoided. Tested with patchbay at 0/50/200/500ms latency and
   0-10-30% loss.

## Design principles for future attempt

If sync is revisited:

1. **Never make it worse under congestion** — must strictly improve over
   unsynchronized PTS pacing in all tested scenarios.
2. **Nudge, don't gate or skip** — adjust PTS pacing rate (1-2ms correction per
   frame), never block video render on audio position.
3. **Audio is never gated or delayed** — audio continuity is paramount.
4. **Measure with patchbay** at 0/50/200/500ms latency, 0-10-30% loss.
5. **Keep it simple** — entire sync mechanism under 30 lines of code. No state
   machines, no cross-thread coordination, no playout buffers.

The delta-nudge approach (proportional control with hard limits, ~1s convergence
from 60ms delta at 30fps) is the most promising unexplored direction.
