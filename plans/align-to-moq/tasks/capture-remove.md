# capture-remove

Branch: align/capture-remove          Wave: 3
Depends on: pin bump to the moq release carrying the merged capture stack (Wave 0);
`codec-remove` on the same platform (moq capture emits moq's frame model into moq's
encoders, so codecs adopt first); and the platform verification gate R-g for macOS
and Windows.
Kind: upstream-gated

## Goal

Shrink `rusty-capture` from 5,507 LOC to about 4,250 by deleting the macOS and
Windows capture arms as moq's own backends replace them, and retiring the
cross-platform `nokhwa` and `xcap` fallbacks once those arms no longer need them.
The net removal is roughly 1,250 LOC, matching the cut-plan's `rusty-capture`
figure for both scenarios; the discrepancy with the 5,507-LOC crate size is
deliberate, because the entire Linux column and the Android stub stay. The macOS
and Windows removals are adopt-theirs: their replacements already ship on moq main
and land with the release bump, so this task waits on a release and a platform
verification gate rather than on any upstream contribution of ours. The Linux
backends we keep are the subjects of separate upstream offers (`pipewire-dmabuf`,
`v4l2-camera-enum`, and `libcamera-preencoded`), but those offers add moq
capability and never gate a deletion here, because the Linux column remains the
working path regardless of whether moq accepts them.

## Evidence

- `../cut-plan.md` section 2, the `rusty-capture` ledger, gives every verdict, LOC,
  and prerequisite, and section 4 lists the deliberate keeps.
- `../../upstream/comparisons/capture.md` sections 2 and 5 carry the per-backend
  comparison and verdicts: theirs wins on macOS camera, macOS screen, and both
  Windows backends; ours wins across the whole Linux column.
- `../../upstream/comparisons/zerocopy.md` section 2a establishes that the adopted
  macOS and Windows paths are self-contained within moq (surface or texture into
  moq's own encoder), so no iroh-live zero-copy path is regressed by their
  adoption.

## moq primitive adopted

- `moq-video` `screencapture` for macOS display, window, and whole-application
  capture with NV12 IOSurface output and fail-fast TCC handling
  (`moq:capture/screencapture.rs:26,36-45`).
- `moq-video` `avfoundation` for macOS camera, a working zero-copy backend
  producing `Frame::Surface` with a TCC authorization flow
  (`moq:capture/avfoundation.rs:1-56`).
- `moq-video` `mediafoundation` (Windows camera, D3D11 NV12 `Frame::Texture`) and
  `desktopduplication` (Windows screen, DXGI to CPU I420)
  (`moq:capture/mediafoundation.rs:1-14`, `moq:capture/desktopduplication.rs:1-15`).
- moq's demand-driven, drop-to-release `FrameStream` lifecycle
  (`moq:capture/mod.rs:230-292`), which removes the local
  cannot-stop-before-start wart (`moq-media/src/publish.rs:1109-1113`).

## iroh-live code changed

Per-backend ledger, keyed to the replacement and the gate.

| Module | file:line | LOC | Removal kind | Waits on |
|---|---|---:|---|---|
| macOS screen | `rusty-capture/src/platform/apple/screen.rs` | 394 | adopt-theirs | release bump plus R-g macOS gate; replaced by moq-video screencapture, already on main (capture.md sec 2, verdict "adopt theirs") |
| macOS camera | `rusty-capture/src/platform/apple/camera.rs` | 81 (stub) | adopt-theirs | release bump plus R-g macOS gate; replaced by moq-video avfoundation, a working backend where ours is a stub (capture.md sec 2, "adopt theirs, outright") |
| Windows camera+screen stubs | `rusty-capture/src/platform/windows/mod.rs` and the `capture_fallback` cfg in `build.rs` | ~100 | adopt-theirs | release bump plus R-g Windows gate; replaced by moq-video mediafoundation and desktopduplication (capture.md sec 2, "adopt theirs, outright") |
| nokhwa fallback | `rusty-capture/src/platform/nokhwa_impl.rs` | 246 | cut after macOS+Windows adopt | release bump; deletable only once the macOS camera and Windows backends are proven, since nokhwa is today the only working macOS-camera and Windows path (capture.md sec 2, 5) |
| xcap fallback | `rusty-capture/src/platform/xcap_impl.rs` | 175 | cut after macOS+Windows adopt | same as nokhwa |
| selection cascade | `rusty-capture/src/lib.rs`, `.../types.rs` | ~250 of 1,107 | merge | the `CameraCapturer`/`ScreenCapturer` cascades and the `CaptureBackend` enum shrink as the Apple and Windows arms leave (capture.md sec 1, 5) |

Explicitly kept, never deleted by this task (cut-plan section 4 and capture.md
section 5):

- `platform/linux/pipewire.rs` (1,655): moq's PipeWire is CPU-only; ours delivers
  DMA-BUF into VAAPI. Port their restore-token replay, static-screen re-pacing,
  and open-per-demand lifecycle. The DMA-BUF path is upstream candidate
  `../../upstream/capture/pipewire-dmabuf.md` (U3), which does not gate this keep.
- `platform/linux/v4l2.rs` (552): keep for enumeration, format negotiation, and
  NV12 passthrough; adopt their zune-jpeg MJPEG shortcut and either implement or
  delete the dead EXPBUF claim. Upstream offer:
  `../../upstream/capture/v4l2-camera-enum.md`.
- `platform/linux/libcamera_h264.rs` (522) and `platform/linux/libcamera.rs`
  (268): the Raspberry Pi story and the only pre-encoded source on either side;
  the strongest capture upstream candidate,
  `../../upstream/capture/libcamera-preencoded.md`, gated on the pre-encoded-source
  concept buy-in (coordination point 5 of the upstream overview), which again does
  not gate this keep.
- `platform/linux/x11.rs` (373): portal-less Linux coverage moq answers only with
  `Error::Unsupported`.
- `platform/android/mod.rs` (40, stub): our MediaProjection plus Camera2 plan with
  AHardwareBuffer zero-copy; moq has no Android capture at all.

## Steps

1. Land the pin bump and `codec-remove` for macOS before touching macOS capture,
   because moq's capture backends emit moq's frame model straight into moq's
   encoders; adopting capture before codecs on a platform would need a throwaway
   conversion layer (cut-plan stage 3 entry condition).
2. Establish the R-g platform verification gate: macOS and Windows CI, or
   checked-in scripted on-hardware smoke tests with recorded results, doing
   camera and screen open, capture, and close per adopted backend.
3. Adopt moq-video avfoundation and screencapture for macOS behind the capture
   feature flag, prove them on hardware, then delete `apple/camera.rs` and
   `apple/screen.rs` in a deletion-only commit.
4. Adopt moq-video mediafoundation and desktopduplication for Windows, prove them
   on hardware, then delete the Windows stubs and the `capture_fallback` cfg.
5. Once macOS camera and all of Windows run on moq's backends, delete
   `nokhwa_impl.rs` and `xcap_impl.rs`, whose only remaining role was covering
   exactly those gaps.
6. Shrink the `CameraCapturer` and `ScreenCapturer` selection cascades and the
   `CaptureBackend` enum in `lib.rs` and `types.rs` to drop the retired Apple and
   Windows arms, keeping the full Linux cascade intact.
7. Separately from any deletion, port moq's three PipeWire behaviors and the
   `FrameStream` demand lifecycle into the kept Linux backends so the local
   cannot-stop-before-start wart is retired.

## Proof before deletion

Mandatory, per coordination point 1.

- No macOS or Windows module is deleted until the adopted moq backend passes the
  R-g open, capture, and close smoke test on real hardware, with recorded results
  checked in, since there is no CI coverage on those platforms today and P1 is
  otherwise unenforceable (cut-plan R-g).
- `nokhwa_impl.rs` and `xcap_impl.rs` are deleted only after both the macOS camera
  and the Windows backends have passed their smoke tests, because they are the
  sole working path for those platforms until then.
- The kept Linux backends continue to pass `rusty-capture`'s existing gates
  (`camera.rs`, `pipewire_reopen.rs`) and the capture-to-encode leg of
  `moq-media/tests/pipeline_integration.rs` throughout.

## Coordination

- Coordination point 1 (proof before deletion): adopt behind a flag, prove on
  hardware, delete.
- Coordination point 3 (upstream gating): although the macOS and Windows removals
  are adopt-theirs and wait only on the release, they are still release-gated and
  R-g-gated; do not delete before the replacement is in a pinned release and
  proven.
- Coordination point 4 (atomic per platform, in concert with `codec-remove`):
  macOS capture flips only after macOS codecs flip, and likewise for Windows, so a
  platform never mixes frame models.
- The keeps here interlock with `codec-remove`: the kept PipeWire DMA-BUF capture
  feeds the kept-then-upstreamed VAAPI encoder, so neither is cut while the other
  stands (coordination point 2).

## Acceptance checklist

- `apple/screen.rs`, `apple/camera.rs`, the Windows stubs, `nokhwa_impl.rs`, and
  `xcap_impl.rs` are deleted, each against a pinned release and behind a passed
  R-g gate, for a net removal of about 1,250 LOC.
- The selection cascade and backend enum no longer reference Apple or Windows
  backends, and the Linux cascade is unchanged.
- The entire Linux column and the Android stub remain in-tree and continue to pass
  their capture tests.
- moq's PipeWire behaviors and the demand-driven `FrameStream` lifecycle are ported
  into the kept backends, retiring the cannot-stop-before-start wart.
- `cargo make check-all` is green at every commit, with deletion-only commits.
</content>
