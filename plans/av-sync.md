# A/V sync — research plan

## Status: disabled, research topic

Active A/V synchronization was removed from moq-media after three
implementation attempts that all performed strictly worse than unsynchronized
PTS-based pacing under congestion. The code, tests, and documentation from
those attempts are preserved on the `sync-redesign-backup` branch. This
document consolidates the research findings and outlines a path forward.

## What works today

Audio and video each pace output based on PTS cadence from the encoder, with
no cross-path coordination. This produces smooth playout under normal
conditions and degrades gracefully under congestion (video freezes briefly on
stalls, audio inserts silence, both resume promptly when packets arrive).

The `LagTracker` in each decode loop records wall-vs-PTS drift, and the video
loop computes `av_delta_ms = video_lag - audio_lag`. This is visible in the
debug overlay's Time section. Under typical conditions the delta stays within
±30ms. On congestion it drifts but self-corrects once both streams resume
receiving packets.

Transport-level freshness (`FreshnessPolicy::max_stale_duration`) controls
how far behind live we fall before Hang's ordered consumer drops stale groups.
This is the primary congestion-response knob and works well independently of
sync.

## What was tried and why it failed

### Attempt 1: Shared PlayoutClock / SyncClock

A single reference clock that both audio and video anchored to. On stalls,
the clock re-anchored to the next arriving packet.

**Problem:** Audio and video observe different transport delays (video has
larger DPB-induced jitter, audio has tighter real-time constraints). When one
stream re-anchored the clock, the other stream's timing was invalidated. This
produced oscillating catch-up behavior where both streams fought over the
reference point.

**Commits:** `4347438..536adf6` (PlayoutClock deletion, SyncClock
introduction, repeated fixes for re-anchoring).

### Attempt 2: Audio-gated video

Audio decode loop sleeps served as the timing reference for video release.
Video would not render until audio had pushed enough samples to match the
frame's PTS.

**Problem:** Audio continuity is more valuable than using it as a gate signal.
Any gap in audio packets propagated directly as a video freeze, even when
video packets were available. Recovery from audio stalls was slow because
video had to wait for the audio queue to refill before resuming.

**Commits:** `abd8df7..2a0cf5f` (audio-master playout redesign and
stabilization attempts).

### Attempt 3: Audio-master with VideoSyncController

The most complete attempt. Audio plays freely, `AudioPosition` estimates the
current speaker PTS from queue depth, and `VideoSyncController` decides per
frame whether to hold, render, or drop based on the video frame's PTS
relative to the audio position.

The controller had two states (catchup and locked) with bounded hold budgets,
late-frame detection, skip-generation coordination between audio and video,
and decode backpressure when the playout buffer filled up.

**Problem:** Under congestion, the controller's hold and drop decisions
created visible artifacts (micro-freezes during hold, frame skips during
catch-up) that raw PTS pacing avoided entirely. Recovery from latency spikes
was slower because the controller tried to re-converge to a specific A/V
offset instead of just playing what was available. The skip-generation
coordination between audio and video added complexity that was hard to
reason about and occasionally caused both streams to flush simultaneously.

Tested with patchbay at 0/50/200/500ms latency and 0-10-30% loss. In every
scenario, the sync machinery either matched or (more commonly) degraded
the experience compared to unsynchronized PTS pacing.

**Commits:** `06b262f` (full redesign), `a64e38e..e749ba4` (VAAPI recovery,
complexity reduction), ultimately removed in `3e2c0de`.

### Common failure pattern

All three approaches shared a root cause: any mechanism that delays, holds, or
skips video frames during congestion recovery makes the experience worse. The
user perceives a freeze or stutter. Unsynchronized PTS pacing avoids this
because both streams simply play whatever they have as fast as it arrives, and
the PTS cadence ensures smooth output when data is flowing.

The A/V delta under unsynchronized pacing is typically 10-50ms and
self-correcting, which is well within lip-sync tolerance for interactive
communication.

## Research directions for re-adding sync

If sync is revisited, the following principles should guide the design:

### 1. Never make it worse under congestion

The sync mechanism must be a strict improvement over unsynchronized PTS pacing
in all tested congestion scenarios, not just normal conditions. This is the bar
that previous attempts failed to clear.

### 2. Nudge, don't gate or skip

Instead of holding frames or dropping them, adjust the PTS pacing rate. If
video is 30ms behind audio, make the next few frame sleeps 1-2ms shorter.
This is invisible to the user and converges gradually. Never block video
render on audio position.

Concretely: the `FramePacer` already has `pace(pts) -> Duration`. A sync
layer could subtract a small correction from that sleep:

```rust
let correction = (av_delta_ms * 0.05).clamp(-2.0, 2.0);  // ≤2ms per frame
let sleep = pacer.pace(pts) - Duration::from_secs_f64(correction / 1000.0);
```

This is proportional control with hard limits. It cannot cause freezes or
skips. At 30fps with 2ms/frame correction, convergence from 60ms delta takes
about 1 second.

### 3. Audio should never be gated or delayed

Audio continuity is paramount. No mechanism should insert silence, add delays,
or gate audio output for sync purposes. The audio path should remain exactly
as it is: decode, push to sink, let the ring buffer smooth jitter.

### 4. Measure before and after with patchbay

Any proposed change must be tested with the patchbay network simulation at:
- 0ms latency, 0% loss (baseline)
- 50ms latency, 0% loss (typical)
- 200ms latency, 5% loss (degraded)
- 500ms latency, 10% loss (severe)

The metric is user experience: video smoothness (no added freezes or skips),
audio continuity (no gaps from sync), and A/V delta (should converge faster
than unsynchronized pacing without introducing artifacts).

### 5. Keep it simple

The entire sync mechanism should be expressible in under 30 lines of code.
If it requires state machines, coordination signals between threads, or
playout buffers, it is too complex and will likely fail the congestion test.

## Decision

Not implementing sync until the delta-nudge approach described above can be
prototyped and tested. The current unsynchronized pacing is good enough for
the project's current stage. The A/V delta metric in the overlay provides
continuous visibility into whether this assessment changes.
