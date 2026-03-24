# A/V sync

The active A/V sync documentation now lives in:

- [docs/av-sync/README.md](../docs/av-sync/README.md)
- [docs/av-sync/tuning.md](../docs/av-sync/tuning.md)

This file tracks only the remaining work.

## Current state

The receive side is audio-master. `PlaybackPolicy` separates sync from
freshness, `AudioPosition` estimates speaker playout, and
`VideoSyncController` decides whether the front decoded video frame should be
held, rendered, or dropped.

That architecture is in place and covered by unit tests and patchbay tests.
The open work is now follow-up refinement, not another redesign.

## Remaining work

1. Revisit long-session drift. We still estimate audio playout from queue depth
   rather than from a hardware-clock calibration loop.
2. Keep reducing internal decode-loop complexity where it does not buy visible
   recovery behavior.
3. Add more coverage around source changes, renderer cadence, and extended
   latency ramps if manual testing finds new failure modes.

## Paths not taken

- We are not going back to a shared re-anchoring `SyncClock`.
- We are not making routine audio gating the primary sync mechanism.
- We are not adding a large adaptive "return to live edge" subsystem unless the
  current bounded front-frame policy proves insufficient.

Those alternatives were explored and made recovery harder to reason about.
