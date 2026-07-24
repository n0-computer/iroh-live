# V4L2 camera capture and Linux device enumeration

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: not on a zero-copy path today. Both sides are CPU MMAP; our EXPBUF
> DMA-BUF export is a documented claim that was never implemented, scoped here as
> a follow-up on B1. See ../zerocopy.md, section 2a.

## What it is

Linux camera capture over V4L2, plus Linux device enumeration. moq already has a
working V4L2 capture backend, but its device enumeration is macOS only:
`cameras()` returns `Error::Unsupported` on every non-macOS platform, so a Linux
caller cannot discover or select a webcam by anything but a raw index or path
string. Ours enumerates `/dev/video*` with per-device FourCC format lists and
negotiates formats richly. The verdict is move-ours because the enumeration and
format negotiation fill a real gap in moq's macOS-only `cameras()`; the capture
loop itself is a wash (both CPU) and we adopt moq's leaner MJPEG shortcut in the
port.

## iroh-live vs moq

Ours (`rusty-capture/src/platform/linux/v4l2.rs`, 552 lines) streams MMAP buffers
through `v4l2r` with four buffers (`v4l2.rs:146, 278`), blocks on `dqbuf` in
`pop_frame` (`v4l2.rs:357`), and converts by negotiated format to NV12, I420,
RGBA, or passthrough packed frames, decoding MJPEG through the `image` crate to
RGBA (`v4l2.rs:462-463, 540-542`). Enumeration scans `/dev/video0..63`, queries
`VIDIOC_QUERYCAP`, filters to capture-capable devices, and reports per-device
FourCC format lists (`v4l2.rs:47-95`), feeding the `CameraSelector` negotiation.
The module doc advertises `VIDIOC_EXPBUF` DMA-BUF export, but the field is
`dead_code` and the delivered frames are CPU (`v4l2.rs:14, 150-161`).

Theirs (`rs/moq-video/src/capture/v4l2.rs`, 204 lines) is a quarter the size: the
`v4l` crate on the pump thread, YUYV resampled directly to I420 via
`I420::from_yuyv` (`v4l2.rs:146`) and MJPEG decoded with pure-Rust `zune-jpeg`
then converted (`v4l2.rs:151`), with the header stating plainly that this is the
CPU path feeding NVENC, VAAPI, and openh264 and there is no GPU surface
(`v4l2.rs:1-7`). Device selection is by index or path string; there is no format
enumeration API. The public enumeration types `Camera { id, name }` and the
`Source::Camera(Option<String>)` selector exist but return `Error::Unsupported`
off macOS (`capture/mod.rs:365-374`).

Both are CPU paths in practice, so the honest comparison is breadth against
economy. Ours keeps richer enumeration, format negotiation, and NV12 passthrough
(useful because our VAAPI encoder wants NV12); theirs decodes MJPEG to I420 in
one hop where ours goes MJPEG to RGBA and reconverts downstream. Keep ours; steal
the zune-jpeg-to-I420 shortcut.

## Zero-copy

Not on a zero-copy path today. Neither side has Linux camera zero-copy; ours
downloads GPU-less CPU MMAP frames and moq converges everything on CPU I420
(../zerocopy.md, section 2a). Our own EXPBUF claim was never implemented: the
`VIDIOC_EXPBUF` field is dead code (`v4l2.rs:150-161`, `#[allow(dead_code)]`,
documented "future DMABUF format selection"). The comparison's instruction is to
implement or delete that claim.

The EXPBUF DMA-BUF export is scoped here as a follow-up on B1, not part of this
leaf. A real EXPBUF path would export the MMAP buffer as a DMA-BUF and deliver
`Frame::DmaBuf`, which depends on ../base/B1-frame-vocabulary.md and on the
encoder importing a camera-origin DMA-BUF. Camera buffers are typically YUYV or
MJPEG rather than NV12, so this is a materially different and unvalidated path
from the PipeWire screen case (see [capture-pipewire.md](capture-pipewire.md)).
Treat it as a follow-up leaf gated on B1 and on hardware validation. If pursued
later it consumes B1's `Frame::DmaBuf` exactly as the PipeWire leaf does and adds
`Depends on: B1`. The default scope of this leaf leaves EXPBUF out and needs no
base plan.

## What to do

MOVE the enumeration and format negotiation into moq, as an additive Linux
platform arm. Target `rs/moq-video/src/capture/mod.rs` and
`rs/moq-video/src/capture/v4l2.rs`.

Source to port from `rusty-capture/src/platform/linux/v4l2.rs`:

- Enumeration. `cameras()` (`v4l2.rs:47-77`) scans `/dev/video0..63`, queries
  `VIDIOC_QUERYCAP`, and skips devices without the `VIDEO_CAPTURE` capability so
  M2M encoder/decoder nodes and ISP nodes are excluded. `enumerate_formats`
  (`v4l2.rs:78-95+`) walks the FourCC format list and, per format, the
  frame-size list, mapping each FourCC to a pixel format. This is the core
  contribution; reshape the output to moq's `Camera { id, name }`.
- NV12 passthrough. Our backend produces NV12 directly when the device offers it
  (`v4l2.rs:455-476` conversion arms), useful because a VAAPI encoder wants NV12
  and this avoids a needless I420 round trip.

What is dropped in the port: our `VideoSource` trait, `start`/`stop`, and the
`CameraCapturer` facade cascade (moq's `FrameStream` plus pump-thread model and
demand-gated open own the lifecycle); our MJPEG-to-RGBA-via-`image` path
(`v4l2.rs:461-463, 540-542`), because moq already decodes MJPEG straight to I420
with pure-Rust `zune-jpeg` (`capture/v4l2.rs:146-151`), honoring the no-ffmpeg
and minimal-dependency rules; our `Duration` timestamps and `CapturePixelFormat`
vocabulary at boundaries; and the dead EXPBUF field, which is either implemented
as the follow-up above or left out, not ported as dead code.

Concrete changes:

1. Add `v4l2::cameras()` to moq's backend, ported from `v4l2.rs:47-95`: scan
   `/dev/video0..63`, `QUERYCAP`, filter to `VIDEO_CAPTURE`, read the card name,
   and build `Camera { id: path, name }`. Use the `v4l` crate moq already depends
   on rather than reintroducing `v4l2r`.
2. Wire the Linux arm of `capture/mod.rs::cameras()` to it
   (`capture/mod.rs:365-374`), keeping `Error::Unsupported` for the remaining
   platforms.
3. Add an NV12 source-format arm to the capture converter so a device offering
   NV12 delivers `Frame::I420` from NV12 without the intermediate step, or, if
   the encoder-facing NV12 optimization is deferred, note it and keep I420 only.
   This step is optional and independent of enumeration.
4. Decide the format-detail question. moq's `Camera` is `{ id, name }` and does
   not carry supported formats. Either (a) drop the per-format detail, which
   keeps the public type unchanged and is the smaller ask, or (b) propose a small
   additive `#[non_exhaustive]` field carrying available formats. Prefer (a) for
   this leaf and mention (b) as a possible follow-up; do not expand the public
   type unilaterally, since any public-shape change is a base-freeze question.
5. Keep moq's `zune-jpeg` MJPEG path; do not port ours.

The iroh-live removal side: `rusty-capture/src/platform/linux/v4l2.rs` (552 LOC,
disposition upstream-ours) is deleted only after the upstream contribution merges
and releases, on the paired `up/v4l2-camera-enum` branch
(`the capture removal sequencing`).

## Tests

- A `cameras()` test that runs on any Linux host: it must not panic, must return
  a `Vec<Camera>` (empty is valid on a camera-less host), and must skip
  non-capture nodes. Runs in CI on a camera-less runner and asserts the no-panic
  and filtering behavior.
- A capture round-trip test marked `#[ignore]` with a stated reason (needs a real
  UVC webcam): open a camera, read a frame, assert geometry and that MJPEG and
  YUYV devices both yield a valid `Frame::I420`. Model it on moq's existing
  hardware-gated capture tests; confirm on named hardware in the PR.
- If the NV12 arm is added, a unit test on a synthetic NV12 buffer asserting the
  converted I420 matches, needing no hardware.

## Evidence

- Verdict: ../comparison/capture.md (Linux camera V4L2 row, "552 lines, v4l2r
  MMAP, NV12/I420/YUYV/MJPEG/RGBA, enumeration" against "204 lines, v4l MMAP,
  YUYV + MJPEG to I420"; section 2 verdict at `capture.md:235-260`, "keep ours;
  steal the zune-jpeg-to-I420 shortcut" and "implement or delete our EXPBUF
  claim"; the section 5 verdict at `capture.md:483-489`). The macOS-only
  `cameras()` at `capture.md:107-109`; "no format enumeration API" at
  `capture.md:250-251`. Upstream candidate value at `capture.md:523-525`.
- Code map: ../comparison/maps/rusty-capture.md (V4L2 backend, 552 LOC, MMAP,
  enumeration, dead EXPBUF field at `v4l2.rs:150-161`).
- Zero-copy: ../zerocopy.md, section 2a (neither side has Linux camera
  zero-copy; V4L2 EXPBUF documented, not implemented, `v4l2.rs:161`).

## Coordination

- Base plan: independent of the base contract for the CPU enumeration and capture
  work. ../base/B1-frame-vocabulary.md (`Frame::DmaBuf`) is consumed only if the
  optional EXPBUF zero-copy follow-up is taken; the default scope needs no base
  plan.
- No candidate-table edit (capture is not a codec candidate), so the shared-table
  coordination point does not apply.
- Any change to the public `Camera` type (the optional format-detail field) is a
  base-shape question; do not expand it unilaterally.
- Adaptation constraints: no ffmpeg and no `image` crate (MJPEG goes through
  moq's `zune-jpeg`); use moq's existing `v4l` crate, not our `v4l2r`, adding no
  new dependency; no `Duration` at a boundary; enumeration failures on a single
  device are skipped (continue the scan), a whole-scan failure returns moq's
  `Error`, no new public variant needed; adopt moq's demand-gated `FrameStream`
  and pump-thread open, do not port our `start`/`stop` trait.
- Release gate: the local module is cut only when the release carrying the leaf
  is pinned, on the paired `up/v4l2-camera-enum` branch.
