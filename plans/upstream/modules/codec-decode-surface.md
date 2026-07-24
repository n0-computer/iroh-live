# VideoToolbox and Media Foundation decode surface retention

VERDICT: apply improvements from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Critical. This is the module that keeps the decode-to-render path
> alive on macOS and Windows once the local decoders are removed. Without it, the
> only decode-to-render GPU path on those platforms dies. See the full section
> below and ../zerocopy.md, section "Decode to render: verdict ours".

## What it is

moq's VideoToolbox (macOS) and Media Foundation (Windows) decode backends
download every decoded frame to CPU I420, even though the surface is already on
the GPU. Our contribution makes both backends retain the GPU surface instead:
VideoToolbox keeps its NV12 `CVPixelBuffer` as `Frame::Surface`, Media Foundation
keeps its DXVA NV12 texture as `Frame::Texture`, so `decode::Frame::native()` has
something to return on Apple and Windows and the render crate can import a surface
with no CPU round trip. This is moq-internal work: it improves moq's own
decoders and ports nothing from our tree, though our `vtb/decoder.rs` and its
Metal-import consumer are the existence proof that retained-CVPixelBuffer decode
output works. Once the surface-retention lands, the local VideoToolbox decoder
(and the Windows story, where we have no local decoder) is removed and macOS and
Windows decode through moq.

## iroh-live vs moq

moq's VideoToolbox decoder wins on H.265 and shares session handling, but
downloads every frame to packed CPU I420 ("macOS decoded frames are always CPU
I420", `moq:decode/backend/videotoolbox.rs:56-60`, `Sink { frames: Vec<I420> }`).
Its Media Foundation decoder downloads the DXVA texture to I420
(`moq:decode/backend/mediafoundation.rs:16-17`, the download at
`:321-357` via `sample_to_i420` calling `download_i420()` at `:339`, CPU output
`Frame::I420(i420)` at `:393`). Only NVDEC keeps decoded frames on the GPU on
moq's side, and that variant is `pub(crate)`, produced only for the NVENC
transcode path.

Ours keeps decoded frames on the GPU: the VideoToolbox decoder wraps each output
as a retained NV12 `CVPixelBuffer` GpuFrame (`codec/vtb/decoder.rs:47-56,
219-220`), feeding the zero-copy Metal import path in `render/metal_import.rs`. We
have no Windows decoder at all, so on Windows the retained-DXVA-texture output is
new work with no iroh-live reference, done inside moq's own backend. Our decoder
also supports deferred session creation from the first keyframe and mid-stream SPS
change detection (`current_sps`, `codec/vtb/decoder.rs:72-74`); moq's decoder
rebuilds the session on parameter-set change too and adds H.265, so we adopt
moq's decoder and contribute only the surface-retention delta.

## Zero-copy

This is the keystone of the macOS and Windows decode-to-render story. The full
chain is: hardware decode produces a GPU surface, the renderer imports that
surface directly (Metal aliasing on macOS via `CVMetalTextureCache`, D3D11 on
Windows), and pixels reach the screen with no CPU download and re-upload. moq's
decoders break that chain by downloading to CPU I420 the moment they emit a
frame, so on macOS and Windows a rendered frame would be decoded on GPU,
downloaded to CPU, and re-uploaded by the renderer.

This module removes the download. It changes only which `Frame` variant the
backend emits; the decode `Backend` trait's `Decoded { timestamp, frame }`
already carries the surface. On macOS the change is the smallest on the whole
program: the `Surface` type, its `Send + Sync` justification, and retain-on-clone
already exist for the capture path (`frame.rs:353-369`), so the decode backend
simply stops calling the I420 download and hands back a `macos::Surface` it
already holds. On Windows the DXVA `ID3D11Texture2D` is retained as
`d3d11::Texture` (`frame.rs:716-729`, clone is a COM AddRef) instead of
downloaded, preserving the #2034 allocated-height metadata for any later CPU
fallback.

Without this contribution, deleting the local VideoToolbox decoder (and adopting
moq on Windows) severs the only decode-to-render GPU path on those two platforms.
The zero-copy rule (the zero-copy rule) therefore gates the local VTB decode deletion on
this surface-retention landing and on render-adopt consuming
`decode::Frame::native()`. See ../zerocopy.md, section "Decode to render: verdict
ours" (the itemized list of what moq lacks) and section 5 requirement U2
(per-decoder surface export). The renderer that imports these surfaces is
render-video-render.md; the Metal import specifically depends on this module for
its decode-side input.

## What to do

Contribute the surface-retention improvement to moq (Path A, in-tree; branch
`up/vtb-mf-decode-surface`), then adopt moq's decoders and delete ours.

Depends on B1 (the public `Native` vocabulary, ../base/B1-frame-vocabulary.md)
and B3 (`decode::Frame::native()`, ../base/B3-decode-native-accessor.md). No new
frame variant is needed on Apple or Windows; the private `crate::frame::Frame`
already has `Surface(macos::Surface)` under `cfg(target_os = "macos")` and
`Texture(d3d11::Texture)` under `cfg(target_os = "windows")`
(`rs/moq-video/src/frame.rs:23-36`). Only B1/B3's public `Native` accessor is
new: it returns `Some(Native::CvPixelBuffer(...))` for a `Frame::Surface`,
`Some(Native::D3d11(...))` for a `Frame::Texture`, and `None` for CPU I420.

moq targets:

- `rs/moq-video/src/decode/backend/videotoolbox.rs`. Change the output callback
  and its `Sink` (`:56-60`, `Sink { frames: Vec<I420> }`) to retain the NV12
  `CVPixelBuffer` and wrap it as `macos::Surface` rather than downloading via the
  shared I420 readback (`:12-13`). Emit `Decoded { frame: Frame::Surface(surface),
  .. }`. The decode remains synchronous, so the callback fires inside
  `decode_frame` before it returns and the `!Send` CoreFoundation handles stay
  thread-confined (`:16-18`).
- `rs/moq-video/src/decode/backend/mediafoundation.rs`. Change `process_output`/
  `drain_output` (`:270-320`) and `sample_to_i420` (`:321-357`) to a
  `sample_to_texture` that pulls the `ID3D11Texture2D` and its subresource index
  out of the sample and wraps them as `d3d11::Texture` on the DXVA device,
  retaining rather than downloading. Preserve the allocated-height metadata the
  download fix relied on (#2034, `frame.rs:791-796`) so any later CPU fallback
  reads chroma correctly. Emit `Frame::Texture(texture)` in place of
  `Frame::I420(i420)` at `:393`.
- No candidate-table edit: both backends are already registered
  (`decode/backend/mod.rs:89-108`); this plan changes only what variant they
  produce, so coordination point 2 does not apply.

Ordered steps:

1. Confirm B1's public `Native` vocabulary and B3's `decode::Frame::native()`
   are in the base branch, with `Native::CvPixelBuffer` and `Native::D3d11` arms
   wrapping the existing `macos::Surface` and `d3d11::Texture`. If either lacks an
   arm the decode side needs, stop and file against B1/B3 (coordination point 1).
2. VideoToolbox: replace the I420-download callback with surface retention. The
   `Surface` clone is a CoreFoundation retain, so the callback retains the
   `CVPixelBuffer` and stores a `macos::Surface`; `Sink` becomes a `Vec<Surface>`
   (or a single pending surface). Keep the synchronous decode contract intact.
3. Media Foundation: replace `sample_to_i420` with `sample_to_texture`, retaining
   the DXVA texture and its subresource index, preserving the #2034
   allocated-height metadata.
4. Keep the CPU path available: `decode::Frame::into_i420()` (B1's universal
   fallback) must still work by downloading the retained surface or texture on
   demand, so a consumer that wants CPU pixels is unaffected and only pays the
   download when it asks.
5. Confirm the platform gating is unchanged (VideoToolbox macos, Media Foundation
   windows) and each still compiles.

iroh-live removal (adopt-theirs, after the leaf): delete
`rusty-codecs/src/codec/vtb/decoder.rs` (+ `vtb.rs`), ~599 LOC, only after the
surface-retention path releases and render-adopt consumes
`decode::Frame::native()`. macOS flips atomically together with the VideoToolbox
encoder adoption (codec-videotoolbox-encode.md) in one revertible commit on the
`up/vtb-mf-decode-surface` pair branch. Windows has no local decoder to delete; it
adopts moq's surface-retaining Media Foundation decoder as pure gain.

## Adaptation notes

- Timestamps are unchanged: `Decoded.timestamp` already carries
  `moq_net::Timestamp`.
- No new `Error` variants: retaining a surface cannot fail the way an export can,
  and any download-on-demand reuses the existing I420 readback error path.
- `decode::Frame` must stay `Send + Sync` on every platform (the compile-time pin
  at `rs/moq-video/src/decode/mod.rs:111-118`, whose comment states callers hold
  frames across `.await`s and share them via `Arc` in the transcode fanout). The
  pin will still pass because `macos::Surface` carries an `unsafe impl Send +
  Sync` and `d3d11::Texture` clones via COM AddRef, but the pin asserts the bounds
  without proving them. Today the backends download to I420 before returning, so
  the retained `CVPixelBuffer` and DXVA texture never cross into the
  `Arc<decode::Frame>` fanout, and the capture-side `unsafe impl` safety comment
  was written for the capture lifecycle, not for a surface shared across fanout
  threads. Retaining the surface past decode is exactly what changes that, so the
  safety argument must be re-examined for the retained-surface decode path
  (concurrent access to a `CVPixelBuffer` or `ID3D11Texture2D` shared by reference
  across fanout threads), not assumed to carry over from capture. If it does not
  hold for shared read access, the fix is a documented synchronization contract or
  a fallback to the download, not silent reliance on the existing comment.
- No ffmpeg, no new dependencies: both surface types and their download fallbacks
  already exist in moq for the capture path.

## Tests

- macOS: a decode test asserting the VideoToolbox output reports
  `decode::Frame::native()` as `Some(Native::CvPixelBuffer(_))`, and that
  `into_i420()` still returns correct pixels on demand. Runs on macOS CI with no
  `#[ignore]` (VideoToolbox is always present on macOS).
- Windows: the analogous assertion for `Some(Native::D3d11(_))`, gated on a
  machine with a working D3D11 device; mark `#[ignore]` with a reason where CI has
  no GPU, and assert the `into_i420()` fallback still deinterleaves the #2034
  allocated-height case correctly.
- A round-trip that decodes, reads `native()`, and separately reads `into_i420()`,
  confirming the zero-copy handle and the CPU fallback coexist.
- Before the local VTB decode deletion, the hardware-gated
  `moq-media/tests/zero_copy_pipeline.rs` must pass on macOS through render-adopt
  (the proof-before-deletion rule, P2), and the macOS platform verification gate R-g applies.

## Evidence

- ../comparison/codecs.md, section 1 (H.264), subsection "VideoToolbox vs
  VideoToolbox", verdict "cut and replace with theirs, then upstream
  GPU-resident decode output".
- ../comparison/zerocopy.md, section 2b (decode to render: verdict ours) and
  section 5, requirement U2 (per-decoder surface export).
- ../comparison/maps/moq-video.md (the "not zero-copy" note: macOS and Windows
  decode output downloaded to I420) and ../comparison/maps/rusty-codecs.md.

## Coordination

- Base plans: B1 (../base/B1-frame-vocabulary.md) and B3
  (../base/B3-decode-native-accessor.md). Coordination point 1 (base API freeze):
  if the public `Native` enum lacks a `CvPixelBuffer` or `D3d11` arm, or
  `decode::Frame::native()` is not shaped to match the private union, stop and
  file against the base plan rather than adding a public API locally.
- Coordination point 2 does not apply: no candidate-table edit. No moq-vaapi
  involvement.
- Release gate: the local VTB decode deletion waits for this leaf in a moq release
  plus render-adopt (U1, U2). macOS flips atomically with the VideoToolbox encoder
  (codec-videotoolbox-encode.md); the macOS platform verification gate R-g governs
  the deletion.
- Sibling cross-references: render-video-render.md (the renderer importing these
  surfaces; Metal import depends on this module for its decode input),
  codec-videotoolbox-encode.md (the encoder half that flips atomically with the
  decode swap).
</content>
