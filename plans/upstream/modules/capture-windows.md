# Windows Capture (Media Foundation + Desktop Duplication)

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: on a zero-copy path. moq's Media Foundation camera backend keeps
> frames GPU-resident as D3D11 NV12 textures consumed by the hardware encoder
> MFT on the same device; see ../zerocopy.md, the capture-to-encode section.

## What it is

Windows camera and screen capture: the camera via Media Foundation
(`IMFSourceReader` with a D3D11 device manager) and the screen via DXGI Desktop
Duplication. iroh-live has documentation-only stubs and leans on the
cross-platform xcap and nokhwa fallbacks; moq has working backends, with the
camera path GPU-resident into the encoder. This is pure gain, with nothing to
bridge: the verdict is to adopt moq's backends and delete our stubs.

## iroh-live vs moq

iroh-live has documentation-only stubs describing an intended WGC/DXGI plus
Media Foundation design (`rusty-capture/src/platform/windows/mod.rs`, about 100
LOC including the `capture_fallback` cfg in `build.rs`). `build.rs` sets that cfg
so xcap and nokhwa serve as CPU fallbacks; there is no real Windows backend code
on our side.

moq has real backends. `mediafoundation.rs` (403 LOC) drives an
`IMFSourceReader` with a D3D11 DXGI device manager and the advanced video
processor so each sample arrives as a GPU-resident NV12 texture
(`Frame::Texture`) that the hardware encoder MFT consumes zero-copy on the same
device, with a software-processor CPU I420 fallback for GPU-less hosts
(`mediafoundation.rs:1-10`); device selection uses `MFEnumDeviceSources` friendly
names or index. `desktopduplication.rs` (351 LOC) duplicates a monitor via DXGI
Desktop Duplication, copies the BGRA desktop texture through a staging texture to
CPU I420, whole-monitor only, with paced re-emission for static screens and the
pump thread hosting the `!Send` duplication handle
(`desktopduplication.rs:1-15`). moq wins outright on both camera and screen. Note
that the screen path is not zero-copy (BGRA to CPU I420); only the camera path
keeps frames on the GPU.

## Zero-copy

The Windows camera path is moq's capture-to-encode zero-copy path: the
`IMFSourceReader` produces a `Frame::Texture` on its D3D11 device, and the
encoder MFT binds the same device through the DXGI manager and consumes the NV12
texture in place (`encode/backend/mediafoundation.rs:1-14`). iroh-live has no
Windows GPU path at all, only the CPU xcap and nokhwa fallbacks, so adopting
moq's camera backend adds a zero-copy path rather than regressing one. The
Windows screen path is deliberately CPU (BGRA staging download to I420) on both
sides in effect, so it is not a zero-copy path. See ../zerocopy.md, the
capture-to-encode section, and the paired encoder in the Media Foundation codec
module.

## What to do

Adopt moq's `mediafoundation` and `desktopduplication` backends and delete our
Windows stubs, gated on the platform release and the R-g Windows verification
gate.

1. Land the pin bump and the Windows codec adoption first, so moq's capture
   backends emit moq's frame model into moq's encoders without a conversion
   layer.
2. Establish the R-g Windows verification gate: Windows CI or a checked-in
   scripted on-hardware smoke test with recorded results, doing camera and
   screen open, capture, and close per adopted backend.
3. Adopt moq's `mediafoundation` and `desktopduplication` behind the capture
   feature flag, prove them on hardware, then delete
   `rusty-capture/src/platform/windows/mod.rs` and the `capture_fallback` cfg in
   `build.rs` (about 100 LOC) in a deletion-only commit.
4. Shrink the `CameraCapturer` and `ScreenCapturer` selection cascades and the
   `CaptureBackend` enum in `lib.rs` and `types.rs` to drop the retired Windows
   arms, keeping the Linux cascade intact.
5. Retire the xcap and nokhwa fallbacks only after these backends are proven,
   since they are today the only working Windows path; those cuts are owned by
   `capture-nokhwa.md` and `capture-xcap.md`.

Since our side is documentation-only stubs, there is nothing to port upstream.

## Tests

The adopted backends must pass the R-g open, capture, and close smoke test on
real Windows hardware with recorded results checked in, since there is no
Windows CI coverage today and P1 is otherwise unenforceable. CI on Linux hosts
cannot exercise these backends. Because our side is stubs, there is no existing
iroh-live Windows test to preserve; the gate is entirely the new on-hardware
smoke test.

## Evidence

- ../comparison/capture.md, section 2 "Windows" and section 5 (verdict "adopt
  theirs, outright, on both camera and screen").
- ../comparison/capture.md, section 3 "Zero-copy capture delivery": Windows
  camera delivers `Frame::Texture` consumed in place by the encoder MFT
  (`encode/backend/mediafoundation.rs:1-14`); Windows screen is CPU (BGRA
  staging download to I420), not zero-copy.
- ../comparison/maps/rusty-capture.md, section 2, the Windows stub row
  (documentation only, `build.rs:1-11` sets `capture_fallback`).
- The removal ledger and gate are in `capture-remove.md` (the Windows camera and
  screen stubs row) and the DISPOSITION register (adopt-theirs, Windows stubs).

## Coordination

- Base plans: B1 (frame vocabulary) for the D3D11 NV12 texture reaching the
  encoder through the shared `Native` handle vocabulary, and B4 (backend trait
  registration) for how the adopted backends are selected.
- The release gate: adopt-theirs, lands with the release bump carrying moq's
  merged capture stack, behind the R-g Windows gate. Do not delete before the
  replacement is in a pinned release and proven.
- Adopt-first ordering with `capture-nokhwa.md` and `capture-xcap.md`: the
  fallbacks are the only working Windows path today, so they are retired only
  after these backends pass their smoke tests.
- Atomic per platform, in concert with the Windows codec adoption: Windows
  capture flips only after Windows codecs flip.
- CI hardware gating: Windows is not covered by current CI; the R-g gate is a
  checked-in on-hardware smoke test.
