# X11 screen capture (portal-less fallback)

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path.

## What it is

Portal-less X11 screen capture over MIT-SHM with RANDR multihead enumeration, the
fallback for Linux systems without an xdg portal or PipeWire. moq lacks this
entirely: its Linux screen story without the `pipewire` feature is
`Error::Unsupported`. The verdict is move-ours because the capability is
capture-land and missing upstream, so it belongs in moq. It is upstream-ours, but
the leaf is not yet written: this is a leaf pending, needing a small follow-up
plan after the Linux capture series.

## iroh-live vs moq

Ours (`rusty-capture/src/platform/linux/x11.rs`, 373 lines) is a CPU-only MIT-SHM
grab. `X11ScreenCapturer` (`x11.rs:134`) uses RANDR for multihead enumeration and
falls back to root screens (`x11.rs:32-61`); `pop_frame` (`x11.rs:311`) calls
`shm::get_image` and builds an RGBA frame (`x11.rs:353`). It is explicitly
documented as having no zero-copy path (`x11.rs:7`). It is not in default
features.

moq has no X11 backend at all. Its `Source::Display` arm on Linux returns
`Error::Unsupported` when the `pipewire` feature is off, with the stated rationale
that on Linux the xdg portal picker owns display selection
(`rs/moq-video/src/capture/mod.rs:365-412`). So ours covers X11-only and
portal-less systems that moq cannot. It sits alongside the adopted fallbacks:
once moq's macOS and Windows capture is adopted, X11 remains the portal-less
Linux screen fallback and our nokhwa/xcap fallbacks are dropped.

## What to do

MOVE the portal-less X11 screen capture into moq. Target `rs/moq-video/src/capture/`
with a new X11 backend feeding moq's `FrameStream`, wired into the
`Source::Display` arm as a portal-less fallback alongside the existing PipeWire
path.

Source to port from `rusty-capture/src/platform/linux/x11.rs` (373 LOC):

- The MIT-SHM grab. `pop_frame` (`x11.rs:311`) calls `shm::get_image` and builds
  an RGBA frame (`x11.rs:353`); adapt it to push a CPU `Frame::I420` (or a packed
  CPU frame) into moq's channel through the pump-thread bridge.
- The RANDR multihead enumeration. `monitors()` (`x11.rs:32-61`) uses RANDR and
  falls back to root screens; reshape the output to moq's `Display { id, name,
  width, height }` enumeration type, giving Linux a portal-less display listing
  moq does not have today.

What is dropped in the port: our `VideoSource` trait and `start`/`stop`
lifecycle, replaced by moq's demand-gated `FrameStream` and pump-thread open; our
`Duration` timestamps at the boundary.

This module is a leaf pending. The comparison and disposition mark it
upstream-ours, but no leaf plan exists yet; the deletion ledger records it as
needing "a small follow-up leaf after the Linux capture series"
(the capture removal ledger). Sequence the follow-up after the PipeWire,
V4L2, and libcamera leaves land, since it reuses the same `FrameStream` and
pump-thread conventions those establish, and it is the lowest-priority Linux
capture item (a portal-less CPU fallback, not a zero-copy path).

The iroh-live removal side: `rusty-capture/src/platform/linux/x11.rs` (373 LOC,
disposition upstream-ours, leaf pending) is deleted only after the upstream
contribution merges and releases. Until then X11 stays as the portal-less Linux
screen fallback; the nokhwa and xcap fallbacks are dropped once the adopted macOS
and Windows backends are proven, and X11 remains the one portal-less path.

## Tests

- An enumeration test that runs on any Linux host with an X server: `monitors()`
  must not panic and must return a `Vec<Display>` (adapting the RANDR walk). On a
  headless CI runner without an X server the test is skipped or asserts the clean
  no-display result.
- A capture round-trip test marked `#[ignore]` with a stated reason (needs a live
  X11 session): open the backend, grab a frame, assert geometry and a valid CPU
  frame. CI without an X server cannot run it; confirm on named hardware in the
  PR.

## Evidence

- Verdict: ../comparison/capture.md (Linux screen X11 row, "373 lines, MIT-SHM
  CPU" against "absent, no portal, no capture", "ours only"; the "backends only
  we have" X11 detail at `capture.md:340-344`, "moq's Linux screen story without
  the pipewire feature is Error::Unsupported ... so we cover X11-only and
  portal-less systems they cannot"; the section 5 verdict at `capture.md:495-497`,
  X11 kept as the portal-less Linux screen fallback).
- Code map: ../comparison/maps/rusty-capture.md (X11 backend, 373 lines, CPU-only
  MIT-SHM, RANDR multihead, "No zero-copy path" at `x11.rs:7`, not in default
  features).
- Disposition: the X11 row is upstream-ours with a pending leaf
  (the capture removal ledger, "needs a small follow-up leaf after the
  Linux capture series").

## Coordination

- Base plan: none. This is a CPU path with no GPU frame vocabulary, so
  ../base/B1-frame-vocabulary.md does not apply, and there is no candidate-table
  edit.
- Leaf pending: this module has no leaf plan written yet. Author a small
  follow-up plan after the Linux capture series (PipeWire, V4L2, libcamera) lands,
  reusing their `FrameStream` and pump-thread conventions.
- Adaptation constraints: no ffmpeg; use moq's pump-thread bridge for the
  blocking `shm::get_image` read; no `Duration` at a boundary; adopt moq's
  demand-gated lifecycle, not our `start`/`stop`.
- Release gate: the local module is cut only when the release carrying the (yet
  to be written) leaf is pinned.
