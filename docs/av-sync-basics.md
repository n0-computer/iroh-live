# A/V sync basics

A/V synchronization is **currently disabled**. Audio and video play
independently with PTS-based frame pacing. Both streams track their
wall-clock drift from PTS cadence, and the debug overlay shows the resulting
A/V delta, but no active correction is applied.

The codec-level background is still true: video decode is burstier than audio
decode, keyframes are the recovery boundary, and live playback should avoid
routine audio gaps. What changed is that the active sync machinery was removed
because it consistently performed worse under congestion than unsynchronized
PTS pacing (see [plans/av-sync.md](../plans/av-sync.md) for the full history).

For the freshness/staleness controls (which are still active), see
[A/V sync tuning](av-sync/tuning.md).
