# vtb-mf-decode-surface. VideoToolbox and Media Foundation decode retain their GPU surface

> Campaign: upstream | Kind: leaf plan | Branch: up/vtb-mf-decode-surface |
> PR target: base branch, then moq main | Read ../0-overview.md first.

Depends on: B1 (frame vocabulary), B3 (`decode::Frame::native()`)
Path: A (in-tree)
Size: S-M

## Goal

Make moq's VideoToolbox (macOS) and Media Foundation (Windows) decode backends keep
their decoded frame on the GPU instead of downloading it to CPU I420, so that
`decode::Frame::native()` has something to return on Apple and Windows and the
moq-video-render crate can import a surface with no CPU round trip (requirement U2).
VideoToolbox keeps its NV12 `CVPixelBuffer` as `Frame::Surface`; Media Foundation
keeps its DXVA NV12 texture as `Frame::Texture`. This is moq-internal work: it improves
moq's own decoders and ports nothing from iroh-live's tree. The VideoToolbox half is
the smallest change on the whole program because the `Surface` type, its `Send + Sync`
justification, and retain-on-clone already exist for the capture path; the decode
backend simply stops calling the I420 download and hands back the surface it already
holds.

## Evidence

- moq decode downloads to CPU everywhere except NVDEC:
  `comparisons/zerocopy.md:244-251`, `comparisons/maps/moq-video.md:717-721`
  (the "not zero-copy" note: "macOS and Windows decode output (downloaded to I420)").
- VideoToolbox decode downloads to packed I420, verified at
  `rs/moq-video/src/decode/backend/videotoolbox.rs:12-13` ("request NV12 output and
  download it to packed I420") and the CPU sink `Sink { frames: Vec<I420> }` at
  `videotoolbox.rs:56-60`.
- Media Foundation decode downloads the DXVA texture to I420, verified at
  `rs/moq-video/src/decode/backend/mediafoundation.rs:16-17` ("a GPU texture ... that
  we download and deinterleave to packed I420"), the download at
  `mediafoundation.rs:321-357` (`sample_to_i420`, calling `Texture::new(...)
  .download_i420()` at `:339`), and the CPU output `frame: Frame::I420(i420)` at
  `mediafoundation.rs:393`.
- The moq-side change 6 and requirement U2 that drive this plan:
  `comparisons/moq-changes.md:237-243`, `comparisons/moq-changes.md:658` (change 6,
  S-M ~120, additive backend-internal), `comparisons/zerocopy.md:445-450`.
- The surface types already exist on the capture side: macOS `Surface` at
  `comparisons/maps/moq-video.md:629-645` (`frame.rs:353-369`, cloning is a cheap
  retain, `unsafe impl Send/Sync` with the safety comment and the `Sync` requirement
  from moq-transcode's `Arc<decode::Frame>` fanout), Windows `Texture` at
  `comparisons/maps/moq-video.md:679-705` (`frame.rs:716-729`, clone is a COM AddRef,
  and the DXVA NV12 allocated-height offset fix #2034).

## moq API consumed

- The B1 `crate::frame::Frame` variants that already exist: `Surface(macos::Surface)`
  under `cfg(target_os = "macos")` and `Texture(d3d11::Texture)` under
  `cfg(target_os = "windows")` (`rs/moq-video/src/frame.rs:23-36`). No new frame
  variant is needed on Apple or Windows; only the public `Native` accessor that B1/B3
  add.
- The B3 `decode::Frame::native() -> Option<Native>` accessor
  (`comparisons/moq-changes.md:208-217`), which matches the private `inner` union: it
  returns `Some(Native::CvPixelBuffer(...))` for a `Frame::Surface` and
  `Some(Native::D3d11(...))` for a `Frame::Texture`, and `None` for a CPU `Frame::I420`.
- The decode `Backend` trait as it stands (`decode/backend/mod.rs:67-75`): `Decoded {
  timestamp, frame }` already carries the surface; this plan changes only which
  `Frame` variant the backend puts there.

## Source to port

None. This is moq-internal: it edits moq's own VideoToolbox and Media Foundation
decode backends. iroh-live's `vtb/decoder.rs` and its Metal-import consumer are the
existence proof that a retained `CVPixelBuffer` decode output works
(`comparisons/zerocopy.md:237-239`, our `vtb/decoder.rs` producing CVPixelBuffer-backed
GPU frames for the Metal path), but no code is copied from our tree here. The change is
subtractive on the download side and additive on the surface-retention side, both
inside moq's existing backends.

## Target in moq

- `rs/moq-video/src/decode/backend/videotoolbox.rs`. Change the output callback and its
  `Sink` (`videotoolbox.rs:56-60`, `Sink { frames: Vec<I420> }`) to retain the NV12
  `CVPixelBuffer` and wrap it as `macos::Surface` (the capture-side type at
  `frame.rs:353-369`) rather than downloading via the shared `CVPixelBuffer` I420
  readback (`videotoolbox.rs:12-13`). Emit `Decoded { frame: Frame::Surface(surface),
  .. }`. The decode remains synchronous, so the callback still fires inside
  `decode_frame` before it returns and the `!Send` CoreFoundation handles stay
  thread-confined (`videotoolbox.rs:16-18`).
- `rs/moq-video/src/decode/backend/mediafoundation.rs`. Change `process_output`/
  `drain_output` (`mediafoundation.rs:270-320`) and `sample_to_i420`
  (`mediafoundation.rs:321-357`) to retain the DXVA `ID3D11Texture2D` as
  `d3d11::Texture` (the capture-side type at `frame.rs:716-729`, holding the device,
  texture, and subresource index) instead of calling `download_i420()`. Emit `Decoded
  { frame: Frame::Texture(texture), .. }` in place of `Frame::I420(i420)` at
  `mediafoundation.rs:393`.
- No candidate-table edit: both backends are already registered
  (`decode/backend/mod.rs:89-108`), and this plan changes only what variant they
  produce, so coordination point 2 does not apply.

## Implementation steps

1. Confirm B1's public `Native` vocabulary and B3's `decode::Frame::native()` accessor
   are in the base branch, with `Native::CvPixelBuffer` and `Native::D3d11` arms
   wrapping the existing `macos::Surface` and `d3d11::Texture` types. If either is
   missing an arm the decode side needs, stop and file against B1/B3 (coordination
   point 1).
2. VideoToolbox: replace the I420-download output callback with surface retention. The
   `Surface` clone is a CoreFoundation retain, so the callback retains the
   `CVPixelBuffer` and stores a `macos::Surface`; `Sink` becomes a `Vec<Surface>` (or a
   single pending surface). Keep the synchronous decode contract intact.
3. Media Foundation: replace `sample_to_i420` with a `sample_to_texture` that pulls the
   `ID3D11Texture2D` and its subresource index out of the sample and wraps them as
   `d3d11::Texture` on the DXVA device, retaining rather than downloading. Preserve the
   allocated-height metadata the download fix relied on (#2034,
   `frame.rs:791-796`) so any later CPU fallback still reads chroma correctly.
4. Keep the CPU path available: `decode::Frame::into_i420()` (B1's universal fallback)
   must still work by downloading the retained surface or texture on demand, so a
   consumer that wants CPU pixels is unaffected and only pays the download when it
   actually asks. This preserves the software-path guarantee.
5. Confirm the platform gating is unchanged (VideoToolbox `cfg(target_os = "macos")`,
   Media Foundation `cfg(target_os = "windows")`) and that each still compiles.

## Tests

- macOS: extend or add a decode test asserting the VideoToolbox backend's output frame
  reports `decode::Frame::native()` as `Some(Native::CvPixelBuffer(_))`, and that
  `into_i420()` still returns correct pixels on demand. Runs on macOS CI (VideoToolbox
  is always present on macOS, so no `#[ignore]` is needed there).
- Windows: the analogous assertion for `Some(Native::D3d11(_))`, gated on a machine
  with a working D3D11 device; mark `#[ignore]` with a reason where CI has no GPU, and
  assert the `into_i420()` fallback still deinterleaves correctly (the #2034
  allocated-height case).
- A round-trip that decodes, reads `native()`, and separately reads `into_i420()`,
  confirming both the zero-copy handle and the CPU fallback coexist.

## Adaptation notes

- Timestamps are unchanged: `Decoded.timestamp` already carries `moq_net::Timestamp`.
- No new `Error` variants are required; retaining a surface cannot fail the way an
  export can. Any download-on-demand from the retained handle reuses the existing
  I420 readback error path.
- `decode::Frame` must stay `Send + Sync` on every platform (the compile-time pin
  at `rs/moq-video/src/decode/mod.rs:111-118`, whose comment states callers hold
  frames across `.await`s and share them via `Arc` in the transcode fanout). The
  compile pin will still pass, because `macos::Surface` carries an
  `unsafe impl Send + Sync` and `d3d11::Texture` clones via COM AddRef. But the
  pin only asserts the bounds; it does not prove them. Today the decode backends
  download to I420 before returning, so the retained `CVPixelBuffer` and DXVA
  texture never cross into the `Arc<decode::Frame>` fanout, and the capture-side
  `unsafe impl` safety comment was written for the capture lifecycle, not for a
  surface shared across the transcode fanout threads. Retaining the surface past
  decode is exactly what changes that: the safety argument behind the
  `unsafe impl Send + Sync` must be revisited for the retained-surface decode
  path (concurrent access to a `CVPixelBuffer` or `ID3D11Texture2D` shared by
  reference across fanout threads), not assumed to carry over from capture. If the
  argument does not hold for shared read access, the fix is a documented
  synchronization contract or a fallback to the download, not a silent reliance on
  the existing comment.
- No ffmpeg, no new dependencies: both surface types and their download fallbacks
  already exist in moq for the capture path.

## Coordination

- Coordination point 1 (base API freeze): the only dependency is B1/B3. If the public
  `Native` enum lacks a `CvPixelBuffer` or `D3d11` arm, or `decode::Frame::native()` is
  not shaped to match on the private union, stop and file against the base plan rather
  than adding a public API locally.
- Coordination point 2 does not apply: no candidate-table edit.
- No moq-vaapi involvement.

## Acceptance checklist

- VideoToolbox decode emits `Frame::Surface`; `decode::Frame::native()` returns
  `Some(Native::CvPixelBuffer(_))` and `into_i420()` still works on demand.
- Media Foundation decode emits `Frame::Texture`; `native()` returns
  `Some(Native::D3d11(_))` and the `into_i420()` fallback preserves the #2034
  allocated-height correctness.
- The `decode::Frame: Send + Sync` compile-time pin still holds on all platforms,
  and the `unsafe impl Send + Sync` safety argument is re-examined for the
  retained surface crossing the transcode `Arc` fanout, not assumed from capture.
- No candidate-table edit, no new dependency, no ffmpeg.
- macOS test passes; Windows test passes or is `#[ignore]`d with a stated reason.
- Conventional commit with `moq-video` scope.
