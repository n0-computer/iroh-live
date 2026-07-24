# Android screen and camera capture (HardwareBuffer, future work)

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: the intended Android capture path is HardwareBuffer-based
> (zero-copy), matching the `Native::HardwareBuffer` variant. This is future
> work, not a shipped capability. See ../zerocopy.md, section 2a.

## What it is

Android screen capture (MediaProjection) and camera capture (Camera2), delivering
frames as AHardwareBuffer for zero-copy GPU import. Today iroh-live's Android
capture is a stub plus a plan, and moq has nothing on Android at all. The verdict
is move-ours because Android capture is capture-land and entirely absent
upstream, so it belongs in moq; but be honest that this is future work. What
exists on our side is a documentation stub of roughly 34 lines, not a shipped
backend. It travels beside the Android codec work and cannot ship before the
frame vocabulary lands.

## iroh-live vs moq

Ours is a documentation-only stub (`rusty-capture/src/platform/android/`, about
34 lines) describing a MediaProjection plus Camera2 plan with AHardwareBuffer
zero-copy. There is no capture code yet. What backs the plan is the codec side:
`rusty-codecs` already carries the `NativeFrameHandle::HardwareBuffer` variant
(`rusty-codecs/src/format.rs:89-109`) and MediaCodec encoder and decoder
backends, and `AndroidHwDecoder` uses MediaCodec's Surface output through an
ImageReader and wraps the AHardwareBuffer as a `GpuFrameInner` with a
`HardwareBuffer` native handle. So the frame vocabulary and the decode side of
zero-copy exist; the capture side is a plan of record.

moq has no Android capture surface at all, and no Android plan. There is nothing
to adopt and nothing to contrast against on the capture axis.

## Zero-copy

The intended Android capture path is HardwareBuffer-based and therefore
zero-copy, matching the `Native::HardwareBuffer` variant that B1 declares
(../base/B1-frame-vocabulary.md). MediaProjection and Camera2 can deliver frames
backed by an AHardwareBuffer, which the encoder imports without a CPU download,
the Android analog of the Linux DMA-BUF path. This is the design intent, not a
measured capability: no capture code exists yet, so no zero-copy path is wired
today. The comparison lists Android as "planned AHardwareBuffer / n/a"
(../zerocopy.md, section 2a and the vendor-API matrix), meaning the handle type
exists in the frame model but no in-tree capture consumer produces it. The
sibling [codec-android-mediacodec.md](codec-android-mediacodec.md) carries the
codec-side HardwareBuffer work, and this capture module produces the frames that
work consumes.

## What to do

MOVE the Android capture plan into moq, as future work sequenced beside the
Android codec leaf. There is no shipped implementation to port; the deliverable
here is a real capture backend built on the frame vocabulary, not a lift-and-shift
of existing code.

Target `rs/moq-video/src/capture/` with a new Android module producing frames for
moq's `FrameStream`:

- A MediaProjection screen backend and a Camera2 camera backend, each delivering
  an AHardwareBuffer-backed frame as the private `Frame::HardwareBuffer` variant
  (B1), with the CPU `to_i420` download as the fallback.
- Wire the Android arms of moq's public `Source::Display` and `Source::Camera`
  selectors, which return `Error::Unsupported` off macOS today.

This depends on the frame vocabulary. The HardwareBuffer path needs B1's
`Native::HardwareBuffer` public arm and the private `Frame::HardwareBuffer`
variant, which B1 lands as a compile-only declaration so the enum is total on
Android; the real MediaCodec/ImageReader producer is the android-mediacodec
leaf's job, and this capture backend is a second producer of the same variant.
Because moq has no Android support today, this also requires moq's build and CI to
grow an `aarch64-linux-android` target, which B1's totality test already
exercises for the codec side.

Placement is gated on the B4 backend-trait-registration decision (coordination
point 6): whether the Android backends live in-tree in moq or register through a
public hook. That decision governs the Android codec leaf and this capture leaf
together, since they share the HardwareBuffer vocabulary and the NDK dependency.

The iroh-live side: the Android capture stub
(`rusty-capture/src/platform/android/`, about 34 LOC, disposition keep) stays as
the plan of record until the Android codec placement (B4) settles and a capture
leaf is worth writing. It is not deleted on the ordinary schedule because there
is nothing shipped to replace; it is the plan itself that migrates upstream once
the placement is decided. Note that iroh-live's separate `moq-media-android`
integration (JNI camera, EGL render, 955 LOC) is app glue, not a capture backend,
and stays local; its codec dependency travels via the Android codec leaf and its
render half via the GLES backend of the render crate.

## Tests

- CI cannot exercise Android capture without a device or emulator, so the
  hardware round-trip tests are `#[ignore]` with a stated reason (needs an
  Android device with camera and screen-capture permission): open the backend,
  receive a frame, assert it is `Frame::HardwareBuffer` with a plausible NV12
  plane layout, and assert `to_i420()` produces a frame of the expected geometry.
- A compile-only target check: moq-video builds for `aarch64-linux-android` with
  the Android arms compiled and the frame enum total, sharing B1's cross-target
  CI job. This verifies the vocabulary and the stub declarations, not runtime
  capture.
- Confirm the round-trip on a named device in the PR body; moq's runners cannot
  run it.

## Evidence

- Verdict: ../comparison/capture.md (Android row, "stub (MediaProjection +
  Camera2 plan), absent no plan", "ours only (planning)", "planned AHardwareBuffer
  / n/a"; the "backends only we have" Android detail at `capture.md:349-352`, "our
  stub documents a MediaProjection plus Camera2 plan with AHardwareBuffer
  zero-copy, and rusty-codecs already carries the NativeFrameHandle::HardwareBuffer
  variant and MediaCodec backends. moq has no Android capture surface at all"; the
  section 5 verdict at `capture.md:505-506`, "keep our plan and frame vocabulary
  ... they have nothing here").
- Zero-copy: ../zerocopy.md, section 2a and the vendor-API matrix (AHardwareBuffer
  handle exposed for consumers, no in-tree capture producer yet; "planned
  AHardwareBuffer").
- Sibling: [codec-android-mediacodec.md](codec-android-mediacodec.md) carries the
  MediaCodec encoder and decoders including the zero-copy HardwareBuffer decode
  path, gated on the same B4 placement decision.

## Coordination

- Base plan: ../base/B1-frame-vocabulary.md. This leaf produces the private
  `Frame::HardwareBuffer` variant and the AHardwareBuffer handle; B1 lands the arm
  as a compile-only declaration so the enum is total on Android without pulling in
  the NDK, and this capture backend supplies a real producer.
- The B4 placement question (coordination point 6): whether Android backends live
  in-tree or register through a public hook. This governs the Android codec leaf
  and this capture leaf together. Android is a plausible upstream decline that
  pushes it onto the B4 registration path; if declined, the plan stays iroh-live's
  and nothing is lost.
- Sibling coordination: this capture leaf and
  [codec-android-mediacodec.md](codec-android-mediacodec.md) share the
  HardwareBuffer vocabulary, the NDK dependency, and the `aarch64-linux-android`
  CI target; sequence them together.
- Honesty note: this is future work, not a shipped capability. The iroh-live side
  is a stub, so there is no zero-copy path to regress and no local deletion on the
  ordinary schedule; the deliverable is a new backend built after B1 and the B4
  decision.
- Release gate: no local cut applies until a real Android capture backend exists
  upstream and releases; until then the stub stays as the plan of record.
