# Adaptive track refactor

## Problem

`AdaptiveVideoTrack` is a separate type from `VideoTrack`, which forces
consumers to choose between the two upfront. This creates three problems:

1. **No shared abstraction.** Code that renders video must accept either
   `VideoTrack` or `AdaptiveVideoTrack`, duplicating rendering logic or
   requiring a trait that neither type currently implements.

2. **Audio has no adaptation at all.** The existing `AdaptiveVideoTrack`
   is video-only. Audio rendition switching or redundancy requires a
   separate mechanism that does not exist.

3. **Adaptation is all-or-nothing.** You either get a plain `VideoTrack`
   with zero adaptation or a fully autonomous `AdaptiveVideoTrack`. There
   is no middle ground where a track carries an optional adaptation
   handle that can be attached or detached dynamically.

## Design

### Core idea: adaptation as an optional attachment

Rather than creating a generic `AdaptiveTrack<T>` that wraps track types,
we make `VideoTrack` and `AudioTrack` carry an optional `AdaptationHandle`
internally. When present, the handle runs a background task that monitors
network signals and performs rendition switching by swapping the internal
frame source. The consumer's `current_frame()` / `next_frame()` API does
not change. When absent, the track behaves exactly as it does today.

This avoids a `Track` trait that would force dynamic dispatch on every
frame read. The two track types are structurally different enough (video
uses `FrameReceiver<VideoFrame>`, audio uses an `AudioSinkHandle` with
no frame channel) that a shared trait would be more boilerplate than
benefit.

### Channel-swap mechanism for video

When the adaptation task decides to switch renditions, it:

1. Subscribes to the new rendition and creates a new `VideoDecoderPipeline`.
2. Waits for the first decoded frame from the new pipeline (staging).
3. Replaces the `FrameSender` in the shared slot with the new pipeline's
   sender, so subsequent frames go to the same `FrameReceiver` the
   consumer already holds.

The current implementation already does something similar by sending a
new `VideoTrack` over an mpsc channel and having the consumer swap it.
The refactored version internalizes this: the `VideoTrack` itself holds
an `Arc<SlotInner<VideoFrame>>` that outlives any individual pipeline,
and the adaptation task replaces the sender side.

The key change from the current design: instead of `AdaptiveVideoTrack`
having its own `current_frame()` / `next_frame()` that wraps
`VideoTrack`, the `VideoTrack`'s frame channel receiver is stable and the
sender side is what gets swapped. This is simpler and keeps the consumer
API identical whether adaptation is enabled or not.

### Audio resilience: preemptive co-subscribe

For audio, the problem is different. Audio rendition switching does not
require a new decoder (Opus decoders are stateless enough to handle
parameter changes). The real concern is continuity: audio silence is far
more noticeable than a video glitch.

The approach:

1. When `enable_audio_resilience()` is called on an `AudioTrack`, a
   background task subscribes to the lowest-quality audio rendition as a
   backup.
2. The backup decoder runs in parallel, writing to a secondary audio
   buffer.
3. The audio output reads from the primary buffer. If the primary
   produces no samples for a configurable timeout (default 100 ms), the
   output switches to the backup buffer.
4. When the primary recovers, the output switches back.

This is conceptually similar to WebRTC's "comfort noise" fallback, but
instead of generating synthetic audio we play actual low-quality audio
from the backup stream.

### What we do NOT do

- **No generic `Track` trait.** Video and audio are too structurally
  different for a useful shared abstraction. Video tracks produce frames
  via a single-slot channel; audio tracks route decoded audio to a sink
  backend with no consumer-facing frame channel.

- **No `AdaptiveTrack<T>`.** A generic wrapper adds a type parameter
  throughout the call chain for marginal benefit. The adaptation logic
  for video (rendition switching with decoder staging) and audio
  (co-subscribe with failover) is different enough that sharing an
  implementation would be forced.

- **No removal of `AdaptiveVideoTrack`.** The existing type stays as a
  deprecated alias / thin wrapper during migration. Removing it would
  break existing consumers immediately.

## API

```rust
// Video: adaptation is opt-in after creation
let video = broadcast.video()?;                       // VideoTrack
video.enable_adaptation(signals, config)?;            // attaches handle
video.selected_rendition();                           // "video/h264-720p"
video.set_rendition_mode(RenditionMode::Fixed("video/h264-360p".into()));
video.disable_adaptation();                           // detaches handle

// Audio: resilience is opt-in after creation
let audio = broadcast.audio(backend).await?;          // AudioTrack
audio.enable_resilience(&broadcast)?;                 // co-subscribes backup

// The frame API is unchanged:
let frame = video.current_frame();                    // Option<VideoFrame>
let frame = video.next_frame().await;                 // Option<VideoFrame>

// AdaptiveVideoTrack stays as a convenience constructor (deprecated):
#[deprecated(note = "use VideoTrack::enable_adaptation() instead")]
let adaptive = broadcast.adaptive_video(signals)?;    // still works
```

## Implementation phases

### Phase 1: AdaptationHandle on VideoTrack (this PR)

- Add `Option<AdaptationState>` field to `VideoTrack`.
- `AdaptationState` holds: the background task handle, rendition
  watchable, mode sender, and a way to swap the frame sender.
- `enable_adaptation()` spawns the adaptation task (reusing the existing
  `evaluate()` / `adaptation_task()` logic) and stores the state.
- `disable_adaptation()` drops the state, stopping the task.
- `current_frame()` / `next_frame()` are unchanged because the
  `FrameReceiver` is stable.
- Deprecate `AdaptiveVideoTrack` and `RemoteBroadcast::adaptive_video()`.

### Phase 2: Audio resilience (this PR, partial)

- Add `Option<AudioResilienceState>` to `AudioTrack`.
- `enable_resilience()` subscribes to the lowest audio rendition and
  spawns a monitoring task.
- The monitoring task detects primary stall (no decoded samples for
  N ms) and signals the audio output to read from the backup sink.
- Full implementation depends on the audio backend supporting source
  switching, which is a separate concern. This PR adds the monitoring
  and signaling; the actual switchover is a TODO.

### Phase 3: Migrate consumers (follow-up)

- Update examples, CLI, tests to use the new API.
- Remove `AdaptiveVideoTrack` once nothing references it.

## Risks and tradeoffs

**Frame sender swap is a lock.** Replacing the `FrameSender` while the
decoder thread is writing requires either an atomic swap (if we use
`Arc<AtomicPtr>`) or a brief mutex hold. The existing `SlotInner` already
uses a `Mutex<Option<T>>` for the value, so the cost is one additional
atomic load per frame to check whether the sender has been replaced.
Given that frames arrive at 30-60 fps, this is negligible.

**Audio co-subscribe doubles bandwidth for audio.** A low-quality Opus
stream at 24 kbps is small enough that the overhead is acceptable for
the reliability gain. The backup can be paused (unsubscribed) when
network conditions are stable for an extended period, but that
optimization is not in scope for this PR.

**Deprecation period.** Keeping `AdaptiveVideoTrack` as a thin wrapper
means two code paths temporarily. The wrapper delegates to a
`VideoTrack` with adaptation enabled, so there is no logic duplication.
