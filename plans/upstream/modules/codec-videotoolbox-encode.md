# VideoToolbox H.264/H.265 encoder (macOS)

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: On the macOS capture-to-encode surface path. Both sides retain the
> IOSurface-backed CVPixelBuffer and hand it straight to `VTCompressionSession`,
> so the path is preserved when we adopt theirs; see ../zerocopy.md, section
> "Capture to encode: verdict complementary", and capture-macos-screen.md.

## What it is

The VideoToolbox encoder drives a hardware `VTCompressionSession` on macOS. This
is the closest matchup in the codec comparison and the one where moq wins: their
backend adds H.265, High profile, per-frame forced IDR, and `ExpectedFrameRate`,
none of which our encoder has, and it carries no encode-side capability we need.
We adopt theirs and delete ours.

## iroh-live vs moq

Shared ground: both create a `VTCompressionSession` over raw objc2 bindings, set
`RealTime`, disable frame reordering, set `AverageBitRate` and
`MaxKeyFrameInterval`, accept zero-copy `CVPixelBuffer` capture input (ours
`codec/vtb/encoder.rs:248-255`, theirs the `Frame::Surface` arm), upload CPU I420
into planar pixel buffers otherwise, and implement live `set_bitrate` with no IDR
(ours `codec/vtb/encoder.rs:329-340`).

Where theirs is ahead:

- H.265. Their backend encodes H.264 and HEVC from the same file
  (`kCMVideoCodecType_HEVC`, `kVTProfileLevel_HEVC_Main_AutoLevel`, VPS spliced
  with SPS/PPS on keyframes), with a hardware round-trip test. Ours is H.264
  only.
- Profile. Theirs uses `kVTProfileLevel_H264_High_AutoLevel`; ours pins
  `kVTProfileLevel_H264_Baseline_AutoLevel` (`codec/vtb/encoder.rs:151-155`),
  costing meaningful compression efficiency on hardware that handles High
  trivially. With reordering disabled VT emits no B-frames on High either.
- Per-frame forced IDR. Theirs passes the force-keyframe dictionary whenever the
  caller asks. Ours has the same dictionary (`build_force_keyframe_props`,
  `codec/vtb/encoder.rs:406-424`) but only an internal `force_next_keyframe`
  flag; the trait cannot request it.
- Output latency shape. Theirs calls `complete_frames` after every encode, making
  output synchronous per frame, and sets `ExpectedFrameRate`, which ours does
  not.

Where ours is ahead, encode-side: only the avcC output mode (description
assembled from the format description in the callback). That is parked, not lost.
The much larger macOS advantage on our side is the GPU-resident decode output,
which is a separate module (codec-decode-surface.md), not this encoder.

## Zero-copy

The macOS capture-to-encode surface path is theirs and ours in equivalent
mechanism, so adopting theirs preserves it. ScreenCaptureKit wraps each
IOSurface-backed CVPixelBuffer as a surface and the VideoToolbox encoder retains
it and hands it to `VTCompressionSession`, letting VideoToolbox do BGRA-to-YUV
and scaling on the GPU (ours `codec/vtb/encoder.rs:243-292`, theirs
`moq:encode/backend/videotoolbox.rs:162-166`). Nothing to contribute here: the
zero-copy encode input already exists on moq's side. The zero-copy work that
does need contributing is on the decode side, covered in
codec-decode-surface.md. See ../zerocopy.md, section "Capture to encode: verdict
complementary", and the capture counterpart in capture-macos-screen.md.

## What to do

Adopt moq-video's VideoToolbox encoder and delete ours.

- Removal (adopt-theirs): delete `rusty-codecs/src/codec/vtb/encoder.rs` (895
  LOC). Nothing to port; the avcC output mode is parked (recorded in the
  parity-ports register), same as the openh264 and annexb cases.
- Sequencing (the codec removal sequencing, step 4): macOS flips atomically. The
  encoder deletion is held until the surface-retaining VideoToolbox decoder
  (codec-decode-surface.md) has released and render-adopt consumes moq's
  `decode::Frame::native()`, then both the encoder adoption and the decoder swap
  land together in one platform-scoped, revertible commit on the
  `up/vtb-mf-decode-surface` pair branch. This keeps the CVPixelBuffer-to-render
  path from ever breaking (zero-copy rule, the zero-copy rule). The encoder alone
  regresses no zero-copy path, but the atomic-per-platform rule forbids holding
  two frame models within macOS at once.

## Tests

- The conformance harness and `moq-media/tests/pipeline_integration.rs` pass with
  the adopted encoder before deletion (the proof-before-deletion rule). moq's own H.264 and HEVC
  hardware round-trip tests cover the adopted backend.
- macOS carries the platform verification gate R-g: CI or checked-in scripted
  on-hardware runs with recorded results, since P1 is unenforceable on a platform
  we cannot test in ordinary CI. No macOS deletion lands without it.

## Evidence

- ../comparison/codecs.md, section 1 (H.264), subsection "VideoToolbox vs
  VideoToolbox", and section 2 (H.265).
- ../comparison/codecs.md, section 10 (verdict table, VideoToolbox encode row:
  "cut, replace (adopt moq)").
- ../comparison/zerocopy.md, section 2a (capture to encode) for the macOS surface
  equivalence.

## Coordination

- Release gate plus atomic-macOS hold: flips only when the VTB decode swap
  (codec-decode-surface.md) is also ready, on the `up/vtb-mf-decode-surface` pair
  branch.
- Platform verification gate R-g governs the macOS deletion.
- The per-encode transcode and FETCH rate-control rule applies to every encoder:
  moq's rate control makes `set_bitrate` mandatory, and their VideoToolbox
  backend already satisfies it by setting `AverageBitRate`.
- Sibling cross-references: capture-macos-screen.md (the paired capture backend
  feeding this encoder), codec-decode-surface.md (the decode half that gates the
  atomic flip).
</content>
