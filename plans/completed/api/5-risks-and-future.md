# Risks and Future Work

This document covers the main risks in the proposed API redesign and the future
work that would make `iroh-live` more than a cleaner wrapper over the current
stack. The goal is not only to fix the API. It is to make the toolkit unusually
good for building real-time media systems.

See `0-overview.md` for the chosen direction, `1-review.md` for the critique,
and `4-impl.md` for the implementation sequence.

## North Star

We should aim for a toolkit that is excellent in three modes at once:

- **app mode**: a product developer can build a call or room quickly
- **media mode**: a systems developer can compose capture, encode, decode, and
  routing pipelines precisely
- **infrastructure mode**: a backend or tooling developer can build relays,
  dashboards, recorders, test harnesses, and analysis tools on the same model

Most real-time stacks are only good at one of these modes.

LiveKit is strong in app mode.
GStreamer is strong in media mode.
Raw WebRTC is a painful baseline that many products inherit accidentally.

If `iroh-live` gets the product layer, the broadcast layer, and the raw layer
all right, it can be unusually strong across all three.

## The Five Biggest Risks

## 1. We add wrappers without changing the center of gravity

This is the most likely failure mode.

If we add:

- `Call`
- `Room`
- `Participant`

but those types still expose or depend on:

- raw `MoqSession`
- stringly broadcast names
- ad hoc session lifetimes
- manual actor wiring

then the redesign will look better but feel the same.

### What would cause this

- jumping straight to top-layer wrappers without improving the broadcast layer
- keeping transport-first examples as the main reference
- allowing room and call types to leak too many implementation details

### Mitigation

- treat `LocalBroadcast` / `RemoteBroadcast` as a real product of the redesign,
  not a temporary wrapper
- require that new examples become materially shorter and more legible
- review every new public method by asking: "does this force the caller to think
  in transport terms?"

## 2. The three layers blur together

The proposed architecture is sound only if the layers stay distinct.

If the product layer starts exposing catalog track names, if the broadcast layer
starts owning room membership semantics, or if the raw layer is re-exported from
everywhere, the surface will become confused again.

### What would cause this

- convenience re-exports added casually over time
- examples bypassing the intended layer boundaries
- product APIs taking raw transport/media types as their normal arguments

### Mitigation

- keep crate root exports small and curated
- move raw transport and pipeline escape hatches under explicit modules
- document boundary rules and enforce them in review

## 3. We optimize the API for RTC rooms and make manual media worse

The repo is not only a room/call library.
The user explicitly asked for:

- one-to-one
- manual processing and setup
- simple RTC rooms

If the redesign imitates LiveKit too closely, manual media and streaming use
cases will become second-class.

### What would cause this

- forcing all publish/subscribe flows through participants and rooms
- hiding broadcast concepts too aggressively
- making custom pipelines awkward compared to canned capture flows

### Mitigation

- keep the broadcast layer first-class
- ensure advanced use cases have direct, documented paths
- keep media pipelines and custom sources/sinks easy to reach

## 4. We optimize the API for flexibility and make the happy path too abstract

The reverse risk also exists.

If we over-preserve the low-level power of the current system, we may produce a
technically elegant but still tiring API where every caller needs to know:

- what a broadcast is
- how audio/video grouping works
- when to subscribe vs when to announce
- how incoming calls are represented internally

### What would cause this

- trying to make one type serve both product and advanced users equally
- pushing configuration objects and builders too far into the common path
- avoiding strong opinions to preserve optionality

### Mitigation

- keep the product layer opinionated
- optimize the first example for clarity, not maximum generality
- evaluate the redesign by rewriting current examples and app integration code

## 5. We stop at API cleanup and miss the bigger opportunity

A better API alone will help, but it will not automatically make the toolkit
great. The stronger opportunity is to make the system unusually good for:

- observability
- testing
- adaptation
- composition
- cross-platform media work

If we stop at type renaming and wrapper layering, we will improve ergonomics
without materially changing what the toolkit enables.

### Mitigation

- treat the API redesign as the foundation for deeper product and systems work
- plan future work in diagnostics, testing, and adaptive media deliberately

## Future Work That Would Matter Most

## 1. First-class observability

Great live media systems are observable.
Today, this repo has useful transport stats and some smoothing helpers, but that
capability is not yet a coherent product feature.

We should add:

- structured call/room/broadcast state snapshots
- per-publication and per-subscription stats
- clear transport path visibility
- codec and rendition selection visibility
- buffer and latency measurements
- dropped-frame and underrun visibility

### What this unlocks

- debuggable apps
- adaptive policy decisions
- meaningful support tooling
- confidence when things go wrong in the field

### Suggested API direction

```rust
pub struct BroadcastStats {
    pub transport: TransportStats,
    pub video: Option<VideoStats>,
    pub audio: Option<AudioStats>,
    pub buffer: BufferStats,
}

impl RemoteBroadcast {
    pub fn stats(&self) -> impl Watcher<Value = BroadcastStats>;
}
```

## 2. Better adaptive subscription and publishing policy

The current API mostly thinks in fixed quality presets.
That is fine for now, but not enough long-term.

We should eventually support:

- target display size
- bitrate budget
- decoder capability filtering
- network-aware subscription policy
- publication-side adaptation
- source switching without semantic republish when possible

This is one of the areas where the toolkit could become much better than the
usual RTC library story.

## 3. Seamless source replacement

WebRTC's `replaceTrack()` is one of its best ideas.

The Rust API should eventually support:

- camera to camera switching
- camera to screen-share switching
- mute/unmute without full publication teardown
- encoder swaps where feasible

without forcing the rest of the object model to reset.

### Suggested principle

Publication identity should stay stable when the *semantic source* stays stable.

For example:

- switching from one camera device to another should usually keep the same
  camera publication
- replacing the source of a screen-share publication should usually keep that
  screen-share publication

That is much easier for UIs and downstream consumers.

### Suggested concrete rules

At the broadcast layer:

- `video.replace_source(new_camera)` keeps the same video slot
- `audio.replace_source(new_microphone)` keeps the same audio slot
- `video.clear()` removes video entirely
- `audio.clear()` removes audio entirely
- `set_enabled(false)` or `set_muted(true)` should not destroy the slot

At the product layer:

- camera device A → camera device B keeps the same camera publication
- microphone device A → microphone device B keeps the same microphone publication
- camera → screen share should usually become a different publication, because
  the semantic source changed
- screen share window A → screen share window B should usually keep the same
  screen-share publication

This split is important. The broadcast layer preserves transport/media continuity.
The product layer preserves semantic continuity.

## 4. Recording and replay as first-class workflows

If this toolkit wants to be the best live streaming toolkit rather than only a
good calling library, recording needs a better story.

We should support:

- recording a room
- recording a single remote broadcast
- recording decoded frames
- recording encoded data directly
- replaying recordings through the same subscription API shape where possible

### Why this matters

Recording is not a side feature.
It is one of the main reasons people adopt a media toolkit instead of writing a
narrow application stack.

## 5. Better testing infrastructure

Real-time media code fails under:

- timing variation
- packet loss
- source churn
- device churn
- slow decoders
- weird shutdown ordering

API quality is not enough if the behavior is fragile.

We should invest in:

- deterministic fake sources and sinks
- golden transport/media fixtures
- broadcast-level integration tests
- room/call scenario tests
- fault injection for network and device behavior
- snapshot tests for catalog and state transitions

The best toolkit is one whose maintainers can change internals aggressively
because the tests are strong enough to catch behavior regressions.

## 6. Multi-environment output model

The current ecosystem spans:

- terminal examples
- native Rust apps
- browser and JS references

The API should stay portable across:

- headless servers
- native UI apps
- browser-adjacent workflows
- recorders and relays

That argues against tight coupling to any one rendering or UI model in the core
API. Attach/render helpers are fine, but they should stay at the edges.

## 7. Metadata and control channels

Hang already models:

- chat
- location
- preview
- user metadata

The Rust redesign should be careful here.
These are valuable, but they should not be forced into the top product layer too
early.

Suggested progression:

1. keep them broadcast-level first
2. let top-level room/call APIs expose them later once the media model is solid

The risk otherwise is that the room API grows a lot of surface before its core
track/publication semantics are stable.

## 8. Graceful degradation for unavailable resources

Every toolkit surveyed in `2-research.md` handles partial availability poorly.
Camera unavailable? Fatal error. Encoder missing? Fatal error. No audio device?
Fatal error.

We should do better:

- missing camera → audio-only mode, not crash
- missing encoder → fallback to software, not error
- device hot-unplug → mute, not teardown
- `has_camera()`, `available_codecs()`, `available_audio_devices()` as capability
  queries
- missing sources as a runtime state (video slot is empty), not a construction error

### Suggested API direction

```rust
impl LocalVideoSlot {
    /// Returns whether a video source is currently active.
    pub fn is_active(&self) -> bool;
}

impl Live {
    /// Returns available video capture devices.
    pub fn available_cameras(&self) -> Vec<CameraInfo>;

    /// Returns available audio input devices.
    pub fn available_microphones(&self) -> Vec<MicrophoneInfo>;

    /// Returns available video codecs for encoding.
    pub fn available_video_codecs(&self) -> Vec<VideoCodecInfo>;
}
```

The broadcast layer should accept empty video/audio slots without error. The product
layer should surface partial capability clearly so apps can adapt their UI.

## Non-Goals For The First Redesign Pass

To keep the redesign tractable, I would explicitly avoid trying to solve these
in the first wave:

- moderation and permissions systems
- active-speaker ranking
- rich chat product semantics
- full browser-style signal graph APIs
- generalized relay/server product APIs
- every possible metadata/control abstraction

These matter, but they should land on top of a solid participant/publication
model, not alongside its first definition.

## Product Opportunities Beyond API Shape

If we want this to become the best live streaming toolkit, we should think about
where it can be better than existing stacks, not only cleaner.

## Opportunity 1: unusually strong state introspection

Most RTC APIs are bad at telling you what is happening.

This toolkit could become known for:

- excellent state snapshots
- clear transport path visibility
- straightforward metrics
- debuggable adaptation decisions

## Opportunity 2: one toolkit for interactive and broadcast workflows

Many stacks split:

- RTC app SDK
- streaming SDK
- recording pipeline
- relay infrastructure

into separate worlds.

The three-layer model gives us a chance to unify them.

## Opportunity 3: composition without callback hell

This is a real differentiator in Rust.

If the top-level API is:

- cloneable
- stateful
- stream-based where it should be
- watcher-based where that fits better
- ownership-driven for cleanup

then it can avoid both:

- JavaScript event soup
- GStreamer-style graph ceremony

That is worth aiming for explicitly.

## Suggested Success Criteria

We should know the redesign is working if all of these become true:

- a "join room and render remote camera" example becomes obviously simple
- a "accept incoming call and publish microphone" example becomes obviously
  simple
- a "subscribe to remote broadcast with manual decode policy" example still
  feels direct
- application code no longer needs to reconstruct participant state from raw
  sessions
- advanced users still have access to raw transport and pipelines without
  private hacks
- the examples become shorter, clearer, and less actor-shaped

## Concrete Recommendation

The redesign should ship with more than a new API.
It should also establish the habits that make the toolkit durable:

- explicit layer boundaries
- strong state and stats surfaces
- ownership-driven cleanup
- testable scenario coverage
- a first-class story for both product and systems users

That is how `iroh-live` can become not only more ergonomic, but genuinely hard
to outgrow.
