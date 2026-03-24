# Playout and sync

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | moq-media |

`moq-media` no longer uses the older shared `PlayoutClock` design. The current
receive side is audio-master:

- transport owns stale-history skipping;
- audio owns the effective playback clock;
- video follows that clock by holding or dropping decoded frames;
- decoder reset is reserved for actual codec failures.

That split matches the code much better than the older shared-clock model.

## Current building blocks

The current implementation is built from three pieces:

- `PlaybackPolicy` exposes the public receive-side tuning surface;
- `AudioPosition` estimates which audio PTS is reaching the speaker;
- `VideoSyncController` decides whether the front decoded video frame should be
  held, rendered, or dropped.

`PlayoutBuffer` still exists, but it is only a small decoded-frame queue used
to smooth decoder burstiness and to give the video loop one front frame to
reason about. It is not a shared timeline abstraction.

## Why audio is the master

Audio continuity is harder to fake than video continuity. Short visual drops
are often acceptable. Short audio gaps are not. The current design therefore
lets audio keep moving through jitter and short outages, then asks video to
catch up to the estimated speaker position.

That does not mean audio is never delayed. There is still a bounded startup
wait so audio does not race far ahead of the first video frame. It does mean
that routine steady-state sync is handled on the video side, not by repeatedly
gating audio on packet-arrival timing.

## Where to read next

- [A/V sync and playout](../av-sync/README.md) documents the current receive-side
  design.
- [A/V sync tuning](../av-sync/tuning.md) explains the public policy surface
  and when to change it.
