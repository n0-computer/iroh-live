# v4l2-camera-enum. V4L2 camera capture and Linux device enumeration

> Campaign: upstream | Kind: leaf plan | Branch: up/v4l2-camera-enum |
> PR target: base branch, then moq main | Read ../0-overview.md first.

Depends on: B1 (only if zero-copy EXPBUF is pursued); otherwise independent
Path: A (in-tree)
Size: M

## Goal

Fill out moq's Linux camera story. moq already has a working V4L2 capture
backend, but device enumeration is macOS only: `cameras()` returns
`Error::Unsupported` on every non-macOS platform
(`rs/moq-video/src/capture/mod.rs:365-374`), so a Linux caller cannot discover or
select a webcam by anything but a raw index or path string. This plan contributes
a Linux `cameras()` implementation that scans `/dev/video*`, filters to
capture-capable devices, and reports per-device formats, so moq's public
enumeration works on Linux the way it does on macOS. It also folds in the format
negotiation and NV12 handling from our richer V4L2 backend where they add value
over moq's, while keeping moq's economical `zune-jpeg` MJPEG-to-I420 shortcut. It
explicitly scopes the zero-copy EXPBUF path (DMA-BUF export from V4L2 into
`Frame::DmaBuf`) as a follow-up rather than part of this leaf, for the reason
stated below.

## Evidence

- Verdict: keep our enumeration and format negotiation, adopt moq's
  `zune-jpeg` MJPEG-to-I420 decode, and implement or delete our EXPBUF claim
  (`comparisons/capture.md:235-258`, and the per-backend verdict at
  `comparisons/capture.md:483-486`). Upstream candidate: "Linux device
  enumeration (V4L2 FourCC scanning) to fill out their macOS-only `cameras()`"
  (`comparisons/capture.md:523-525`).
- moq is enumeration-poor: "Device selection is by index or path string; there is
  no format enumeration API" (`comparisons/capture.md:250-251`), and the public
  `cameras()`/`displays()`/`windows()`/`apps()` are macOS-only
  (`comparisons/capture.md:107-109`).
- moq's source, verified against HEAD 3a3e0ea8:
  - `rs/moq-video/src/capture/mod.rs:365-374` `cameras()` returns
    `Error::Unsupported` off macOS.
  - `rs/moq-video/src/capture/mod.rs:117-190` the public `Camera { id, name }`
    enumeration type and its `.source()` builder (per the map,
    `comparisons/maps/moq-video.md:496-501`).
  - `rs/moq-video/src/capture/v4l2.rs:1-7` the backend's own doc: MMAP CPU path,
    YUYV resampled and MJPEG decoded with `zune-jpeg`, "no GPU surface here".
  - `rs/moq-video/src/capture/v4l2.rs:24-53` `open()` selecting the device by
    index or path through `Camera::open` on the pump thread; the `Camera::open`
    impl and `negotiate` at `v4l2.rs:88-96`.
  - `rs/moq-video/src/capture/v4l2.rs:141-159` the conversion arms inside `read()`:
    YUYV via `I420::from_yuyv` (`v4l2.rs:146`) and MJPEG via
    `zune_jpeg::JpegDecoder` (`v4l2.rs:151`).
  - `rs/moq-video/src/capture/pump.rs` the blocking-device bridge this backend
    already uses.
- Our reference: `comparisons/maps/rusty-capture.md:213-222` (V4L2 backend, 552
  LOC, MMAP, enumeration, dead EXPBUF field).

## moq API consumed

- The public `Camera { id, name }` enumeration type and `Source::Camera(Option<String>)`
  (`capture/mod.rs`), which this plan populates on Linux. No change to their
  shape; this is an additive platform arm.
- Independent of the base contract for the CPU enumeration and capture work. B1
  (`Frame::DmaBuf`) is consumed only if the optional EXPBUF zero-copy follow-up is
  taken in this leaf; the default scope leaves EXPBUF out and needs no base plan.

## Source to port

`rusty-capture/src/platform/linux/v4l2.rs` (552 LOC). What carries over:

- Enumeration. `cameras()` (`v4l2.rs:47-77`) scans `/dev/video0..63`, queries
  `VIDIOC_QUERYCAP`, and skips devices without the `VIDEO_CAPTURE` capability so
  M2M encoder/decoder nodes and ISP nodes are excluded. `enumerate_formats`
  (`v4l2.rs:78-95+`) walks the FourCC format list and, per format, the frame-size
  list, mapping each FourCC to a `CapturePixelFormat`. This is the core
  contribution; port the scan-and-filter logic and reshape the output to moq's
  `Camera { id, name }` (moq's `Camera` does not carry a format list, so the
  format detail either drops or motivates a small additive field, see step 4).
- NV12 passthrough. Our backend produces NV12 directly when the device offers it
  (`v4l2.rs:455-476` conversion arms), which is useful because a VAAPI encoder
  wants NV12. moq's backend converges everything on `I420`; adding an NV12
  source-format arm avoids a needless I420 round trip when the encoder wants NV12.

What is dropped:
- Our `VideoSource` trait, `start`/`stop`, and the `CameraCapturer` facade cascade
  (`comparisons/maps/rusty-capture.md:14-46, 154-174`). moq's `FrameStream` plus
  pump-thread model and demand-gated open own the lifecycle; we adopt it
  (`comparisons/moq-changes.md:499-519`, section 3 item 7).
- Our MJPEG-to-RGBA-via-`image` path (`v4l2.rs:461-463, 540-542`). moq already
  decodes MJPEG straight to I420 with pure-Rust `zune-jpeg`
  (`capture/v4l2.rs:146-151`); keep moq's, drop ours, honoring the no-ffmpeg and
  minimal-dependency rules (the `image` crate and its jpeg backend do not enter
  moq).
- Our `Duration` timestamps and `CapturePixelFormat` vocabulary at boundaries.
- The dead EXPBUF field (`v4l2.rs:150-161`, `#[allow(dead_code)]`). It is either
  implemented as the follow-up below or left out; it is not ported as dead code.

## Target in moq

- `rs/moq-video/src/capture/mod.rs:365-374`: replace the non-macOS `cameras()`
  stub with a `#[cfg(target_os = "linux")]` arm delegating to a new
  `v4l2::cameras()`, keeping `Error::Unsupported` for the remaining platforms.
- `rs/moq-video/src/capture/v4l2.rs`: add a `cameras()` function (the ported
  scan) and, if NV12 passthrough is taken, an NV12 source-format arm alongside
  the existing YUYV and MJPEG arms. The existing `open()`/`Camera::open` pump
  wiring is reused unchanged for the capture path.
- No candidate-table edit (capture is not a codec candidate), so coordination
  point 2 does not apply.

## Implementation steps

1. Add `v4l2::cameras()` to moq's backend, ported from `v4l2.rs:47-95`: scan
   `/dev/video0..63`, `QUERYCAP`, filter to `VIDEO_CAPTURE`, read the card name,
   and build `Camera { id: path, name }`. Use the `v4l` crate moq already depends
   on rather than reintroducing `v4l2r`.
2. Wire the Linux arm of `capture/mod.rs::cameras()` to it.
3. Add the NV12 source-format arm to the capture converter so a device offering
   NV12 delivers `Frame::I420` from NV12 without the intermediate step, or, if the
   encoder-facing NV12 optimization is deferred, note it and keep I420 only. This
   step is optional and independent of enumeration.
4. Decide the format-detail question: moq's `Camera` is `{ id, name }` and does
   not carry supported formats. Either (a) drop the per-format detail, which keeps
   the public type unchanged and is the smaller ask, or (b) propose a small
   additive `#[non_exhaustive]` field carrying available formats. Prefer (a) for
   this leaf and mention (b) as a possible follow-up; do not expand the public
   type unilaterally (coordination point 1 applies to any public-shape change).
5. Keep moq's `zune-jpeg` MJPEG path; do not port ours.

## Zero-copy EXPBUF scope

Out of scope for this leaf, stated deliberately. Our own EXPBUF claim was never
implemented (the field is dead code, `v4l2.rs:150-161`), and neither side has
Linux camera zero-copy today (`comparisons/capture.md:382, 414-420`). A real
`VIDIOC_EXPBUF` path would export the MMAP buffer as a DMA-BUF and deliver
`Frame::DmaBuf`, which depends on B1 and on the encoder importing a camera-origin
(often YUYV or MJPEG, not NV12) DMA-BUF, a materially different and unvalidated
path from the PipeWire screen case. Treat it as a follow-up leaf gated on B1 and
on hardware validation, not as part of enumeration. If pursued later, it consumes
B1's `Frame::DmaBuf` exactly as `pipewire-dmabuf` does and adds `Depends on: B1`.

## Tests

- A `cameras()` test that runs on any Linux host: it must not panic, must return
  a `Vec<Camera>` (empty is a valid result on a host with no camera), and must
  skip non-capture nodes. This runs in CI on a camera-less runner and asserts the
  no-panic and filtering behavior.
- A capture round-trip test marked `#[ignore]` with a stated reason (needs a real
  UVC webcam): open a camera, read a frame, assert geometry and that MJPEG and
  YUYV devices both yield a valid `Frame::I420`. Model it on moq's existing
  hardware-gated capture tests.
- If the NV12 arm is added, a unit test on a synthetic NV12 buffer asserting the
  converted I420 matches, needing no hardware.

## Adaptation notes

- No ffmpeg, no `image` crate: MJPEG goes through moq's `zune-jpeg`; our
  `image`-based path is dropped.
- Minimal dependencies: use moq's existing `v4l` crate, not our `v4l2r`; add no
  new dependency.
- Timestamps: none travel with the frame; the encode loop stamps.
- Errors: enumeration failures on a single device are skipped (continue the
  scan), matching our `v4l2.rs:52-58` behavior; a whole-scan failure returns
  moq's `Error`. No new public variant needed.
- Lifecycle: adopt moq's demand-gated `FrameStream` and pump-thread open; do not
  port our `start`/`stop` trait.

## Coordination

- Coordination point 1 (base API freeze). Any change to the public `Camera` type
  (the optional format-detail field) is a base-shape question; do not expand it
  unilaterally. The default scope changes no public type.
- If the EXPBUF follow-up is later folded in, it consumes B1 and inherits
  coordination point 1 against the `Native`/`DmaBuf` contract.
- No candidate-table edit; coordination point 2 does not apply.

## Acceptance checklist

- `capture::cameras()` returns real devices on Linux and still
  `Error::Unsupported` on unsupported platforms.
- Non-capture V4L2 nodes (M2M, ISP) are excluded from the listing.
- MJPEG capture uses moq's `zune-jpeg` path; the `image` crate does not enter
  moq.
- The camera-less CI test passes; the hardware capture test exists and is
  `#[ignore]`d with a stated reason and confirmed on named hardware in the PR.
- No public type is expanded without a base-plan decision.
- `cargo clippy` clean; no new dependency; no ffmpeg; no `Duration` at a
  boundary.
