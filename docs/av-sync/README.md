# A/V sync and playout

| Field | Value |
|-------|-------|
| Status | stable |
| Applies to | moq-media, iroh-live |

`moq-media` now uses an audio-master playout model. Audio establishes the
effective playback clock, video follows it, and transport freshness stays a
separate concern. That is the current design to understand, tune, and extend.

The receiver stack is intentionally split into four layers:

1. `hang::OrderedConsumer` gives us in-order delivery and skips stale groups
   once they exceed a configured age.
2. The decoder pipelines turn ordered packets into decoded audio samples and
   video frames.
3. `AudioPosition` estimates which audio PTS is currently reaching the speaker.
4. `VideoSyncController` decides whether the front decoded video frame should
   be held, rendered, or dropped relative to that audio position.

This is simpler than the earlier shared-clock designs because each layer owns
one job. Transport decides freshness. Audio decides time. Video decides visual
catch-up.

## Current receive-side model

### Public policy

`RemoteBroadcast` exposes one receive-side policy type:

```rust
PlaybackPolicy {
    sync: SyncMode,
    freshness: FreshnessPolicy,
}
```

`SyncMode` answers one question: how should decoded video relate to decoded
audio at playout time?

- `SyncMode::AudioMaster { video_hold_budget }` is the default. Audio keeps
  continuity. Video may wait up to `video_hold_budget`, or drop late frames, to
  stay close to the estimated speaker position.
- `SyncMode::Unmanaged` renders video as soon as it decodes and disables active
  A/V alignment. This is mainly for diagnostics and local experiments.

`FreshnessPolicy` answers a different question: how much stale media are we
willing to keep before we skip forward?

- `FreshnessPolicy::max_stale_duration` feeds Hang's ordered-consumer latency
  ceiling.
- The same value also gates decode-side stale-packet recovery. When video is
  clearly behind the current audio playout position, we skip toward the next
  keyframe instead of trying to preserve obsolete packets.

The important part is the split itself. Users tune sync and freshness for
different reasons, so the API keeps them separate.

### Audio path

The audio decode loop is continuity-first.

```text
OrderedConsumer
  -> forward_packets()
  -> audio decode thread
  -> decoder.push_packet() / pop_samples()
  -> AudioSink::push_samples()
  -> output queue / resampler
  -> cpal callback
  -> speaker
```

Two details matter for sync:

- The output queue is the real audio playout buffer. We do not build a second
  sync-only buffer in front of it.
- `AudioPosition` records the last pushed chunk span and the current queue
  depth, then estimates speaker position as:

```text
playback_pts = last_chunk_start + last_chunk_duration - buffered_output
```

That estimate is good enough for lip sync and has a direct relationship to the
actual audio backend.

The audio loop also contains the small recovery behavior we intentionally keep:

- a bounded startup wait in audio-master mode so audio does not race far ahead
  of the first video frame;
- silence insertion after short gaps so brief stalls do not become hard audio
  dropouts;
- skip-generation handling so audio flushes stale decoded state after video has
  skipped forward.

### Video path

The video decode loop is timestamp-driven and bounded.

```text
OrderedConsumer
  -> forward_packets()
  -> video decode thread
  -> decoder.push_packet() / pop_frame()
  -> PlayoutBuffer
  -> VideoSyncController
  -> frame channel
  -> renderer
```

`PlayoutBuffer` is only a small decoded-frame queue. It smooths decoder burst
output and lets the release logic reason about one front frame at a time.

`VideoSyncController` is intentionally narrow. Given the front frame PTS and
the current `AudioPosition`, it returns one of three decisions:

- hold the frame for a bounded duration;
- render it now;
- drop it because it is already too late.

The controller carries only two diagnostic states:

- `catchup`: video is not yet stably close to audio, or transport has gone
  quiet;
- `locked`: video is close enough to audio that steady playout should look
  smooth.

The decode loop adds one more piece on top of that: when the front frame is
already being held and the decoded queue is large enough, it pauses further
decode intake. That prevents the old failure mode where held future frames
accumulated until VAAPI ran out of output surfaces or the playout queue started
thrashing.

### Recovery behavior

The current recovery story is deliberately plain.

When latency rises or jitter increases:

- Hang may skip very old groups.
- Audio resumes quickly through silence insertion and ordinary decode.
- Video drops late frames, holds early ones, and uses the decoded queue to
  smooth bursty output once the stream is stable again.

When latency drops again:

- there is no separate "return to live edge" subsystem;
- instead, transport already starts delivering newer media promptly, audio keeps
  advancing, and video catch-up happens through the same hold-or-drop policy;
- decode backpressure keeps future backlog from fighting that recovery.

When the decoder encounters a real failure:

- video clears stale decoded frames;
- the sync controller falls back to `catchup`;
- the decoder waits for a fresh keyframe and may reset the backend if the codec
  implementation supports that.

This design is intentionally less adaptive than the earlier plans. Ordered
delivery already solves most of the transport-side problem, so the playout code
should not try to re-create a full adaptive transport controller.

## Module layout

The receive pipeline is split so that shared coordination stays in
`pipeline.rs`, while codec-specific loops live in submodules:

- `pipeline.rs`: shared decode context, `AudioPosition`, packet forwarding, and
  tests;
- `pipeline/audio_decode.rs`: audio decode loop and audio-only recovery logic;
- `pipeline/video_decode.rs`: video decode loop, keyframe skip recovery, and
  playout gating;
- `playout.rs`: public playback policy, `PlayoutBuffer`, and
  `VideoSyncController`.

That split reflects the design boundary. `playout.rs` contains policy and
timing decisions. The pipeline modules contain thread orchestration and codec
integration.

## Stats and debug overlay

The subscribe-side overlay is aligned to the current design. The timing section
shows the values that matter when diagnosing sync and recovery:

- `sync`: `catchup` or `locked`;
- `audio_buffer_ms`: estimated queued audio between decode and speaker output;
- `audio_live_lag_ms`: newest received audio PTS minus estimated speaker PTS;
- `video_live_lag_ms`: newest received video PTS minus last rendered video PTS;
- `av_delta_ms`: rendered video PTS minus estimated speaker PTS;
- `late_frames_dropped`: video frames dropped because they were already too
  late.

The render section still shows decoder FPS and decode cost. That remains useful
because not all visible jank is a sync problem; slow decode or bursty decoder
output can produce the same symptom.

## What we removed

The current design replaced several approaches that looked attractive but made
recovery harder to reason about.

- We removed the shared re-anchoring `SyncClock`. Audio and video should not
  both mutate one reference point when they observe different delays.
- We stopped treating audio decode-loop sleeps as the primary sync mechanism.
  Audio continuity is more important than forcing it through packet-arrival
  timing.
- We dropped the larger multi-stage "return to live edge" heuristics that kept
  re-trimming queues and re-anchoring clocks. Ordered delivery plus a bounded
  front-frame policy worked better.

Those paths are worth remembering because they explain the shape of the current
code. They are not the direction we want to return to.

## Known limits

The design is intentionally "good enough" rather than fully adaptive.

- `AudioPosition` estimates the speaker position from queue depth, not from a
  hardware clock readback. Device callback latency still folds into the
  estimate.
- `SyncMode::Unmanaged` is useful for diagnostics, but it intentionally gives up
  lip-sync guarantees.
- `video_hold_budget` and `max_stale_duration` still need practical tuning for
  new products or unusual networks. We have fewer knobs than before, not zero.

For the public tuning surface and guidance on when to change these values, see
[A/V sync tuning](tuning.md).
