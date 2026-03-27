# A/V sync — status and research notes

## Status: enabled (shared playout clock, ported from moq/js)

A shared playout clock (`moq_media::sync::Sync`) was ported from the
moq/js player (`js/watch/src/sync.ts`, commit `53fe78d8`). It is the
default sync mode (`SyncMode::Synced`). See
[docs/av-sync/README.md](../docs/av-sync/README.md) for the full
description and receive pipeline diagram.

## Earlier attempts (removed)

Three sync architectures were implemented and removed (`3e2c0de`,
`9c72f6a`) because they all performed worse than unsynchronized PTS
pacing under congestion. Code preserved on `sync-redesign-backup`
branch.

1. **Shared PlayoutClock** — audio and video fought over the reference
   point on re-anchor after stalls, causing oscillation.
2. **Audio-gated video** — audio gaps propagated as video freezes even
   when video packets were available.
3. **Audio-master with VideoSyncController** — most complete attempt;
   under congestion the hold/drop decisions created micro-freezes and
   skips that raw PTS pacing avoided. Tested with patchbay at
   0/50/200/500 ms latency and 0–10–30% loss.

## What the current approach does differently

The JS-ported `Sync` does not try to synchronize video *to* audio.
Instead, it establishes a single reference offset (the earliest
wall-minus-PTS ever observed) and buffers both streams by the same
latency target. Audio paces itself through its ring buffer; video paces
itself through `Sync::wait`. The two converge because they share the
reference and the latency, without cross-path signaling or gating.

## Open questions

- How does the shared clock behave under the patchbay congestion
  scenarios that tripped up the earlier attempts? No patchbay tests
  currently exercise `SyncMode::Synced`.
- Should the jitter buffer be adaptive rather than fixed at 100 ms?
  The JS player uses a fixed default too, but a network-aware jitter
  estimate could tighten latency on good paths.
- The catalog `jitter` field (per-codec latency) is populated by
  moq-mux's fMP4 importer but not by live encoders. If live encoders
  start populating it, the `Sync::set_audio_latency` /
  `set_video_latency` setters are ready.
