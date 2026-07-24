# macOS Screen Capture (ScreenCaptureKit)

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: on a zero-copy path. moq's ScreenCaptureKit backend delivers
> NV12 IOSurface `CVPixelBuffer` frames straight into VideoToolbox; see
> ../zerocopy.md, the capture-to-encode section.

## What it is

macOS screen capture over Apple's ScreenCaptureKit: enumerating displays and
windows, opening a capture stream, and delivering IOSurface-backed frames to the
encoder. Both sides implement it, and both are genuinely zero-copy deliveries,
but moq's backend is the superset: it adds whole-application capture, requests
NV12 output that enters VideoToolbox in the encoder's native layout, and fails
fast on a missing permission instead of silently producing nothing. The verdict
is therefore to adopt moq's backend and delete ours.

## iroh-live vs moq

iroh-live's `MacScreenCapturer` (`rusty-capture/src/platform/apple/screen.rs`,
394 LOC) captures displays (`screen.rs:276`) and single windows (`new_window`,
`screen.rs:238`), enumerating both via `SCShareableContent`
(`screen.rs:95-133`). An `SCStreamOutputTrait` callback wraps each
IOSurface-backed `CVPixelBuffer` as an `AppleGpuFrame` in BGRA
(`screen.rs:207-209`) and pushes it into `sync_channel(2)` (`screen.rs:331`).
Missing Screen Recording permission is detected with
`CGPreflightScreenCaptureAccess` but only warned about (`screen.rs:41-54`);
capture then silently produces nothing.

moq's `screencapture.rs` (434 LOC) covers display, window, and whole-application
capture, differing only in the `SCContentFilter` each builds (#2293). It
requests NV12 output
(`kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`, `screencapture.rs:26`),
filters window listings to layer 0 so the dock and menu bar do not pollute the
list (`screencapture.rs:41-45`), and fails `open` if no first frame arrives
within 5 s, which converts a missing TCC grant into an error instead of a hang
(`screencapture.rs:36-38`). The `CMSampleBuffer` to surface extraction is shared
with the AVFoundation camera backend in a 20-line `surface.rs`.

moq wins on two axes: features (whole-application capture and fail-fast
permission handling) and format. NV12 matters because their surface enters
VideoToolbox in its native layout, while our BGRA surface makes VideoToolbox do
the color conversion internally. Ours is functional but strictly a subset plus
BGRA, with nothing to port upstream.

## Zero-copy

Both sides keep the pixel data on the GPU: the captured frame is a retained
IOSurface `CVPixelBuffer` that the VideoToolbox encoder feeds directly into
`VTCompressionSession`. iroh-live delivers it in BGRA and VideoToolbox performs
the BGRA-to-4:2:0 conversion during encode; moq delivers it in NV12 so the
surface is already in the encoder's native layout and no conversion is needed.
The macOS capture-to-encode zero-copy path is therefore moq's after adoption,
paired with moq's VideoToolbox encoder; see `codec-videotoolbox-encode.md` and
../zerocopy.md, the capture-to-encode section. Adopting capture before codecs on
macOS would need a throwaway conversion layer, so codecs adopt first.

## What to do

Adopt moq's `screencapture` backend and delete iroh-live's, gated on the
platform release and the R-g macOS verification gate.

1. Land the pin bump and the macOS codec adoption first, because moq's capture
   backend emits moq's frame model straight into moq's encoders; adopting
   capture before codecs would need a throwaway conversion layer.
2. Establish the R-g macOS verification gate: macOS CI, or a checked-in scripted
   on-hardware smoke test with recorded results, doing display and window open,
   capture, and close.
3. Adopt moq's `screencapture` behind the capture feature flag, prove it on
   hardware, then delete `rusty-capture/src/platform/apple/screen.rs` (394 LOC)
   in a deletion-only commit.
4. Shrink the `ScreenCapturer` selection cascade and the `CaptureBackend` enum
   in `lib.rs` and `types.rs` to drop the retired macOS arm, keeping the Linux
   cascade intact.

Nothing is ported upstream from our version; the absence of app capture and the
BGRA-versus-NV12 gap are recorded in `../notes-unstructured/parity-ports.md`.

## Tests

The adopted backend must pass the R-g open, capture, and close smoke test on
real macOS hardware with recorded results checked in, since there is no macOS CI
coverage today and the proof-before-deletion rule (P1) is otherwise
unenforceable. CI on Linux hosts cannot exercise this backend at all. The
capture-to-encode leg of `moq-media/tests/pipeline_integration.rs` continues to
gate the pipeline on covered platforms.

## Evidence

- ../comparison/capture.md, section 2 "macOS screen: ScreenCaptureKit" and
  section 5 (verdict "adopt theirs").
- ../comparison/capture.md, section 3 "Zero-copy capture delivery": macOS screen
  delivers a retained `CVPixelBuffer` fed straight into `VTCompressionSession`
  (`rusty-codecs/src/codec/vtb/encoder.rs:245-253`).
- ../comparison/maps/rusty-capture.md, section 2, the ScreenCaptureKit row and
  detail (`platform/apple/screen.rs:182-209, 331, 387`).
- The removal ledger and gate are in `capture-remove.md` (the macOS screen row)
  and the DISPOSITION register (adopt-theirs, `platform/apple/screen.rs`).

## Coordination

- Base plans: B1 (frame vocabulary), so the NV12 surface reaches the encoder
  through the shared `Native` handle vocabulary; B4 (backend trait
  registration) for how the adopted backend is selected.
- The release gate: the macOS removal is adopt-theirs and lands with the release
  bump carrying moq's merged capture stack, behind the R-g macOS gate. Do not
  delete before the replacement is in a pinned release and proven.
- Atomic per platform, in concert with `codec-videotoolbox-encode.md`: macOS
  capture flips only after macOS codecs flip, so the platform never mixes frame
  models.
- CI hardware gating: macOS is not covered by current CI; the R-g gate is a
  checked-in on-hardware smoke test.
