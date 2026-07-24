# macOS Camera Capture (AVFoundation)

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: on a zero-copy path. moq's AVFoundation backend delivers
> IOSurface `CVPixelBuffer` frames straight into VideoToolbox with no copy and
> no color conversion; see ../zerocopy.md, the capture-to-encode section.

## What it is

macOS camera capture over AVFoundation: enumerating cameras, requesting the TCC
authorization grant, opening an `AVCaptureVideoDataOutput`, and delivering
IOSurface-backed frames to the encoder. iroh-live never finished this backend;
its file is a non-functional stub, and the working macOS camera path today is
the cross-platform nokhwa fallback (CPU RGBA, no surface). moq's backend is
complete and zero-copy with the full TCC flow, so the verdict is to adopt moq's
and delete ours. This is the single largest quality gap in moq's favor alongside
Windows.

## iroh-live vs moq

iroh-live's `apple/camera.rs` (81 LOC) is a stub whose `new()` bails with
"AVFoundation camera capture is not yet implemented; enable the `nokhwa`
feature" (`camera.rs:41-47`), `cameras()` returns empty, and the objc2
sample-buffer delegate is unfinished. Because of it, the facade cascade
deliberately orders nokhwa before AVFoundation (`rusty-capture/src/lib.rs:198-203`),
so the working macOS camera path is nokhwa: CPU RGBA, no surface, no zero-copy.

moq's `avfoundation.rs` (244 LOC) is complete. It runs an
`AVCaptureVideoDataOutput` with a delegate that wraps each IOSurface-backed
`CVPixelBuffer` as `Frame::Surface`, so "frames reach VideoToolbox with no copy
and no color conversion" (`avfoundation.rs:1-6`). It gates on TCC authorization
with a 60 s prompt timeout, applies a 5 s first-frame timeout, and enumerates
cameras by AVFoundation `uniqueID` without needing the grant
(`avfoundation.rs:29-56`). moq wins outright: ours is a stub, theirs is a working
zero-copy backend with TCC handling, and there is nothing to port.

## Zero-copy

moq's backend is zero-copy end to end on the capture-to-encode leg: each
captured frame is a retained IOSurface `CVPixelBuffer` in NV12 delivered as
`Frame::Surface`, cloned as a retain, and consumed by the VideoToolbox backend
via `surface.buffer.clone()` into `encode_frame`
(`encode/backend/videotoolbox.rs:162-166`), pinned by a hardware test. iroh-live
has no zero-copy macOS camera path at all, only the CPU nokhwa fallback, so
adopting moq's backend is a net addition of a zero-copy path rather than a
regression. The path pairs with moq's VideoToolbox encoder; see
`codec-videotoolbox-encode.md` and ../zerocopy.md, the capture-to-encode
section.

## What to do

Adopt moq's `avfoundation` backend and delete iroh-live's stub, gated on the
platform release and the R-g macOS verification gate, and additionally on nokhwa
being retired. Because nokhwa is currently the only working macOS camera path,
the order is adopt-first: prove moq's backend before removing anything.

1. Land the pin bump and the macOS codec adoption first, so moq's capture
   backend emits moq's frame model into moq's encoders without a conversion
   layer.
2. Establish the R-g macOS verification gate: macOS CI or a checked-in scripted
   on-hardware smoke test with recorded results, doing camera open, capture, and
   close, including the TCC prompt flow.
3. Adopt moq's `avfoundation` behind the capture feature flag and prove it on
   hardware.
4. Delete the stub `rusty-capture/src/platform/apple/camera.rs` (81 LOC) in a
   deletion-only commit, and shrink the `CameraCapturer` selection cascade and
   the `CaptureBackend` enum in `lib.rs` and `types.rs` to drop the retired
   macOS arm.
5. Retire nokhwa only after this backend is proven, since nokhwa is the sole
   working macOS camera path until then; the nokhwa cut is owned by
   `capture-nokhwa.md`.

## Tests

The adopted backend must pass the R-g open, capture, and close smoke test on
real macOS hardware with recorded results checked in, exercising the TCC
authorization flow, since there is no macOS CI coverage today and P1 is
otherwise unenforceable. CI on Linux hosts cannot exercise this backend. Because
ours is a stub, there is no existing iroh-live macOS camera test to preserve; the
gate is entirely the new on-hardware smoke test.

## Evidence

- ../comparison/capture.md, section 2 "macOS camera" and section 5 (verdict
  "adopt theirs, outright").
- ../comparison/capture.md, section 3 "Zero-copy capture delivery": moq delivers
  `Frame::Surface`, consumed by VideoToolbox as `surface.buffer.clone()` into
  `encode_frame` (`encode/backend/videotoolbox.rs:162-166`).
- ../comparison/maps/rusty-capture.md, section 2, the AVFoundation row
  (stub/non-functional, `platform/apple/camera.rs:40-56`), and the note that
  `list_cameras` places nokhwa before AVFoundation (`lib.rs:198-210`).
- The removal ledger and gate are in `capture-remove.md` (the macOS camera row)
  and the DISPOSITION register (adopt-theirs, `platform/apple/camera.rs`).

## Coordination

- Base plans: B1 (frame vocabulary) for the NV12 surface reaching the encoder,
  and B4 (backend trait registration) for how the adopted backend is selected.
- The release gate: adopt-theirs, lands with the release bump carrying moq's
  merged capture stack, behind the R-g macOS gate. Do not delete before the
  replacement is in a pinned release and proven.
- Adopt-first ordering with `capture-nokhwa.md`: nokhwa is the only working
  macOS camera path today, so it is retired only after this backend passes its
  smoke test.
- Atomic per platform, in concert with `codec-videotoolbox-encode.md`: macOS
  camera capture flips only after macOS codecs flip.
- CI hardware gating: macOS is not covered by current CI; the R-g gate is a
  checked-in on-hardware smoke test.
