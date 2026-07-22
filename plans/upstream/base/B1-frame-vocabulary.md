# B1. Public GPU-frame vocabulary (Native + Frame variants + moq-frame crate)

Branch: moq-upstream/base (B1 lands first in the base series)          PR target: base branch, then moq main
Depends on: none (this is the keystone every GPU leaf rests on)
Path: Both (needed for Path A and Path B)
Size: M-L (roughly 300 lines as a public module in moq-video, 700 to 1000 as a standalone `moq-frame` crate)

## Goal

Give moq a public, closed-but-non-exhaustive vocabulary of concrete OS frame
handles, so a GPU-resident frame can cross the moq-video crate boundary without a
CPU download. Today moq's raw frame type `crate::frame::Frame` is `pub(crate)`
(`rs/moq-video/src/frame.rs:23`) and the only public exit is a CPU copy through
`decode::Frame::into_i420()` (`decode/mod.rs:94-101`). This plan adds the public
`Native` enum of platform handles, adds a `DmaBuf` accessor type that mints a file
descriptor on demand rather than storing one per buffered frame, and adds the two
cfg-gated private `crate::frame::Frame` variants (`DmaBuf` on Linux+vaapi,
`HardwareBuffer` on Android) that feed the public vocabulary. It is the single
change every zero-copy leaf, the decode-side `native()` accessor (B3), and the
out-of-tree renderer depend on, and it is designed to be the first thing landed
and the first thing agreed with the maintainer in the base RFC.

## Evidence

- Our capture and codec layers already deliver GPU handles with no download:
  PipeWire wraps a dup'd fd as `NativeFrameHandle::DmaBuf`
  (`rusty-capture/src/platform/linux/pipewire.rs:145-247, 721-766`); the VAAPI
  encoder imports a matching NV12 DMA-BUF as a VA surface with a hand-built
  `VADRMPRIMESurfaceDescriptor` (`rusty-codecs/src/codec/vaapi/encoder.rs:87-118,
  1268-1297`); the Android plan carries an `AHardwareBuffer`
  (`rusty-codecs/src/format.rs:89-109`).
- Our native handle is `NativeFrameHandle` (`rusty-codecs/src/format.rs:73-87`),
  `#[non_exhaustive]`, with `DmaBuf(DmaBufInfo)` on Linux
  (`format.rs:505-517`), `HardwareBuffer` on Android, `CvPixelBuffer` on macOS.
- The lazy fd export is our reference design: `GpuFrameInner::native_handle`
  documents "transient export on access, rather than storing one FD per buffered
  frame" (`rusty-codecs/src/format.rs:495-502`), and `DmaBufInfo` is the field
  layout to mirror (`format.rs:505-525`: fourcc/`drm_format`, `modifier`, coded
  and display dimensions, per-plane `{ offset, pitch }`).
- moq's private enum, its cfg-gated shape, and its download-fallback method are in
  `comparisons/moq-changes.md` section 1a (change 1) and `comparisons/traits-api.md`
  sections 3.2 to 3.4. Both were read against `/home/bit/Code/rust/moq` at HEAD
  `3a3e0ea8`.

## moq API consumed

None. B1 defines the frozen-contract types the rest of the campaign consumes:
`Native`, `DmaBuf`, and the private `crate::frame::Frame` variants. It is the
producer of the vocabulary, not a consumer.

## Source to port

- `rusty-codecs/src/format.rs:505-525` (`DmaBufInfo`, `DmaBufPlaneInfo`): the field
  layout for the public `DmaBuf` accessor. Carried over: fourcc, modifier, coded
  and display size, per-plane offset and pitch. Dropped: the eagerly stored
  `fd: OwnedFd` field. The moq `DmaBuf` holds the exporting object (a VA surface or
  a dup source) and produces an `OwnedFd` only in `export()`.
- `rusty-codecs/src/format.rs:495-502` (`native_handle` doc, "transient export on
  access"): the design rationale for mint-on-access, carried over verbatim into the
  `DmaBuf::export` doc.
- `rusty-codecs/src/format.rs:89-109` (`HardwareBufferInfo`): the Android handle's
  NV12 plane layout, the reference for the `HwBuffer` accessor. The Android leaf
  (android-mediacodec) supplies the backing type; B1 only declares the public arm
  and the private `Frame::HardwareBuffer` variant so it compiles under
  `cfg(target_os = "android")`.

## Target in moq

1. **The private frame enum** `crate::frame::Frame`
   (`rs/moq-video/src/frame.rs:23-36`). It is already per-platform cfg-gated
   (`Surface` under macos, `Texture` under windows, `Cuda` under linux+nvdec,
   `I420` always). Add two cfg-gated variants:

   ```rust
   pub(crate) enum Frame {
       #[cfg(target_os = "macos")]
       Surface(macos::Surface),
       #[cfg(target_os = "windows")]
       Texture(d3d11::Texture),
       #[cfg(all(target_os = "linux", feature = "nvdec"))]
       Cuda(cuda::Frame),
       // NEW: any Linux GPU frame with a PRIME-exportable descriptor, produced by
       // VAAPI decode, PipeWire DMA-BUF capture, and V4L2 EXPBUF, consumed by the
       // VAAPI encoder. Gated on a new shared `dmabuf` feature (see below), not on
       // `vaapi`, because PipeWire and V4L2 produce DMA-BUF without VAAPI.
       #[cfg(all(target_os = "linux", feature = "dmabuf"))]
       DmaBuf(dmabuf::Frame),
       // NEW: Android MediaCodec / ImageReader output and camera capture.
       #[cfg(target_os = "android")]
       HardwareBuffer(android::HwBuffer),
       I420(I420),
   }
   ```

2. **The `width`/`height`/`to_i420` arms** (`rs/moq-video/src/frame.rs:39-74`). Each
   of the three methods is a match over the variants; add an arm for `DmaBuf` and
   for `HardwareBuffer` to each, matching the existing per-variant cfg gates.
   `to_i420` (`frame.rs:63-74`) gains a CPU download fallback for each new variant,
   so the software encode path (`Frame::to_i420` is called by every CPU backend and
   by `into_i420`) keeps working:

   ```rust
   pub(crate) fn to_i420(&self) -> Result<Cow<'_, I420>, Error> {
       match self {
           // ... existing arms ...
           #[cfg(all(target_os = "linux", feature = "dmabuf"))]
           Frame::DmaBuf(db) => Ok(Cow::Owned(db.download_i420()?)),
           #[cfg(target_os = "android")]
           Frame::HardwareBuffer(hb) => Ok(Cow::Owned(hb.download_i420()?)),
           Frame::I420(i) => Ok(Cow::Borrowed(i)),
       }
   }
   ```

3. **The backing type `dmabuf::Frame`** (new module `rs/moq-video/src/frame/dmabuf.rs`
   under `cfg(all(target_os = "linux", feature = "dmabuf"))`). It holds the exporting
   object plus the DRM descriptor metadata (fourcc, modifier, coded and display
   size, per-plane offset and pitch), exposes `download_i420(&self) -> Result<I420,
   Error>` for the fallback, and `export(&self) -> Result<OwnedFd, Error>` for the
   fresh dup. It does not store an `OwnedFd`. The concrete VA-surface-backed
   implementation is supplied by the vaapi-decode leaf (which owns moq-vaapi surface
   export); B1 defines the type, its fields, and its two methods so PipeWire capture
   (`pipewire-dmabuf`) and the VAAPI backends have a shared shape to produce and
   consume. Where B1 cannot supply a real exporter without moq-vaapi, it lands the
   type behind the `vaapi` feature with the export body as the moq-vaapi call the
   leaf fills in, and gates the whole module so a `vaapi`-less build never compiles
   it.

4. **The public `Native` vocabulary.** Two homes are on the table; the decision is
   an RFC item (see Adaptation notes):

   - Preferred: a new small crate `rs/moq-frame` with `Cargo.toml`, `src/lib.rs`,
     and the `Native`, `DmaBuf`, `Plane`, and `Size` public types. moq-video,
     moq-transcode, and an out-of-tree render crate then share one vocabulary.
     moq-video depends on `moq-frame` and re-exports `Native` at `lib.rs` beside
     `Error` and `Size` (`rs/moq-video/src/lib.rs:57-58`).
   - Fallback: a public module `pub mod native;` in moq-video, re-exported at
     `lib.rs`. Same types, no new crate, but not shareable by a render crate without
     depending on all of moq-video.

   The public surface, identical either way, and the frozen contract leaves code
   against verbatim:

   ```rust
   /// A GPU-resident frame's platform handle. Names a kernel or OS object, never a
   /// backend type, so it respects moq's "no backend type in the public API" rule
   /// (rs/moq-video/src/lib.rs:37-44).
   #[non_exhaustive]
   pub enum Native {
       #[cfg(target_os = "linux")]
       DmaBuf(DmaBuf),          // fd on demand, fourcc, modifier, planes
       #[cfg(target_os = "macos")]
       CvPixelBuffer(Surface),  // moq's existing macos::Surface, made public
       #[cfg(target_os = "windows")]
       D3d11(Texture),          // moq's existing d3d11::Texture, made public
       #[cfg(all(target_os = "linux", feature = "nvdec"))]
       Cuda(Cuda),              // moq's existing cuda::Frame, made public
       #[cfg(target_os = "android")]
       HardwareBuffer(HwBuffer),
   }

   /// A DMA-BUF-backed frame. Holds the exporting object and the DRM descriptor;
   /// mints a descriptor only when a consumer asks, so no fd is held per frame.
   pub struct DmaBuf { /* private exporter, no fd stored */ }

   impl DmaBuf {
       /// The DRM fourcc of the buffer's pixel layout.
       pub fn fourcc(&self) -> u32;
       /// The DRM format modifier (tiling / compression).
       pub fn modifier(&self) -> u64;
       /// The allocated buffer size, which may exceed the visible picture.
       pub fn coded_size(&self) -> Size;
       /// The visible picture size.
       pub fn display_size(&self) -> Size;
       /// The per-plane layout, in plane order.
       pub fn planes(&self) -> &[Plane];   // Plane { offset: u32, pitch: u32 }
       /// Exports a fresh dup'd descriptor. No fd is held per frame, so a caller
       /// owns the returned fd and closes it when done.
       pub fn export(&self) -> Result<OwnedFd, Error>;
   }

   #[non_exhaustive]
   pub struct Plane {
       pub offset: u32,
       pub pitch: u32,
   }
   ```

   The Linux arm is gated `target_os = "linux"` (not the `vaapi` feature) so a
   caller can name `Native::DmaBuf` on any Linux build; the arm carries a `DmaBuf`
   whose exporter is only constructible where a producer feature (`vaapi`,
   `pipewire`) is on. `CvPixelBuffer(Surface)`, `D3d11(Texture)`, and `Cuda(Cuda)`
   wrap moq's existing `macos::Surface`, `d3d11::Texture`, and `cuda::Frame` by
   making a thin public newtype, not by making the internal types public, so moq
   keeps those internals free to change.

5. **The re-export** at `rs/moq-video/src/lib.rs` beside `pub use error::Error;` and
   `pub use size::Size;` (`lib.rs:57-58`): `pub use native::Native;` (module home)
   or `pub use moq_frame::Native;` (crate home).

## Implementation steps

1. Land the public vocabulary types first, standalone and testable, in whichever
   home the RFC picks: `Native`, `DmaBuf`, `Plane`, and a `Size` alias or re-export
   of moq's `Size`. On non-Linux, non-Android hosts the enum still has the macOS /
   Windows / CUDA arms, so the type compiles everywhere. This is the piece the
   maintainer reviews for the public-API commitment; keep it minimal.
2. Add the `dmabuf::Frame` backing type behind
   `cfg(all(target_os = "linux", feature = "dmabuf"))` with `download_i420`,
   `export`, and the descriptor accessors, so `Frame::DmaBuf` has a payload. Where
   the real VA-surface exporter is not yet available (moq-vaapi export is the
   vaapi-decode leaf's job), stub `export` to the moq-vaapi entry point the leaf
   fills, and gate the module so a build without `dmabuf` never sees it.

Feature design: add a new `dmabuf` Cargo feature to moq-video. It is the base
gate for the `Frame::DmaBuf` variant and its `dmabuf` module. The `vaapi`,
`pipewire`, and `v4l2` features each enable `dmabuf` (`dmabuf = []`, `vaapi =
["dmabuf", ...]`, and so on), so any producer or consumer of DMA-BUF pulls the
variant in without depending on `vaapi`. This resolves the open question the
pipewire-dmabuf and v4l2-decode leaves raise: they are DMA-BUF producers that do
not otherwise need VAAPI, and they gate on `dmabuf`.
3. Add the two variants to the private `crate::frame::Frame`
   (`frame.rs:23-36`) with their cfg gates.
4. Add the matching arms to `width`, `height`, and `to_i420`
   (`frame.rs:39-74`), with `to_i420` downloading for the new variants so every CPU
   consumer keeps a total match.
5. Wire the public `Native` to the private `Frame`: a `pub(crate)` conversion
   `impl crate::frame::Frame { fn native(&self) -> Option<Native> }` that B3 exposes
   on `decode::Frame`. Keep it `pub(crate)` here; B3 publishes the accessor. This
   split lets B1 land the vocabulary and the private plumbing without B3, and lets
   B3 be a tiny follow-on.
6. Add the Android arm and `HwBuffer` type behind `cfg(target_os = "android")` as a
   compile-only declaration (fields plus `download_i420`), so the enum is total on
   Android; the real MediaCodec / ImageReader producer is the android-mediacodec
   leaf. moq builds on non-Android hosts do not compile it.

## Tests

- A compile-time totality check per platform: an exhaustive `match` over
  `crate::frame::Frame` in `to_i420`, `width`, and `height` fails to build if a
  variant is added without an arm. This already exists structurally; the test is
  that CI builds moq-video with `--features vaapi` on Linux and (in a separate job)
  for the `aarch64-linux-android` target, so both new arms are exercised.
- A `Native`/`DmaBuf` API-shape test: construct a `DmaBuf` from a synthetic
  descriptor (no real GPU), assert `fourcc`, `modifier`, `coded_size`,
  `display_size`, and `planes` round-trip, and that two `export()` calls yield two
  distinct owned fds (proving mint-on-access, not a shared stored fd). This is
  ffmpeg-free and hardware-free, so it runs in CI everywhere.
- The `frame_and_consumer_are_thread_safe` compile check (`decode/mod.rs:104-118`)
  must still pass with the new variants, so `dmabuf::Frame` and `android::HwBuffer`
  are `Send + Sync`; add the `assert_send`/`assert_sync` bound if a variant regresses.
- A real DMA-BUF export round trip is the vaapi-decode leaf's hardware-gated test,
  not B1's; B1 ships only the hardware-free shape tests.

## Adaptation notes

- The public arm carries a kernel or OS object name (`DmaBuf`, `CvPixelBuffer`,
  `D3d11`, `Cuda`, `HardwareBuffer`), never a backend name, so the "no backend type
  in the public API" rule (`lib.rs:37-44`) holds even with a public enum.
- The `moq-frame` crate versus in-moq-video module choice is the RFC decision. The
  crate is preferred because the out-of-tree renderer (render leaf, Option B) and
  moq-transcode can share the vocabulary without depending on all of moq-video, and
  because it is the cleanest proof that the vocabulary is self-contained. The module
  is the smaller ask if the maintainer resists a new crate. Whichever lands, the
  public type shape above is identical, so leaves are unaffected by the choice.
- Mint-on-access is the one place our model is strictly better than a store-the-fd
  design (`comparisons/moq-changes.md` section 1a, change 2): a buffered playout
  queue holds many frames, and holding one fd per frame exhausts the descriptor
  table. `DmaBuf::export` dups fresh on each call and the caller owns the result.
- Errors use moq's `Error` (`rs/moq-video/src/error.rs`, re-exported at `lib.rs:57`).
  `export` failing needs a variant; propose `Error::SurfaceExport(String)` here or
  cite B5, which collects the moq-side `Error` additions a full upstreaming needs.
- No ffmpeg, no dlopen in B1 itself: the vocabulary is pure metadata plus an fd dup.
  The producers that need libva or the NDK are the leaves, which dlopen per B5.

## Coordination

- This is coordination point 1 (base API freeze). B1 is the frozen contract for the
  `Native` and `DmaBuf` shape; no GPU leaf finalizes against a different shape. If a
  leaf finds the vocabulary cannot express its module (a missing `Native` variant, a
  missing accessor), it stops and files the gap against B1 rather than improvising.
- The concrete `dmabuf::Frame` exporter body depends on moq-vaapi surface export,
  which the vaapi-decode leaf owns (coordination point 3). B1 declares the type and
  its methods; the leaf fills the VA-surface export. B1 must not grow moq-vaapi
  itself.
- The Android arm's backing type is the android-mediacodec leaf's, gated on the B4 /
  Path B decision (coordination point 6). B1 lands the arm as a compile-only
  declaration so the enum is total on Android without pulling in the NDK.

## Acceptance checklist

- [ ] `Native`, `DmaBuf`, and `Plane` are public, `#[non_exhaustive]` where the
      contract says, and match the frozen-contract signatures verbatim.
- [ ] The home (crate or module) is the one the RFC agreed, and `Native` is
      re-exported from moq-video's crate root.
- [ ] `crate::frame::Frame` has the `DmaBuf` and `HardwareBuffer` variants under the
      right cfg gates, and `width`/`height`/`to_i420` are total on every platform.
- [ ] `to_i420` downloads for both new variants, so every CPU consumer still works.
- [ ] No public API names a backend type; only OS/kernel object names appear.
- [ ] `DmaBuf::export` dups a fresh fd per call and holds none per frame; the shape
      test proves two exports yield two distinct fds.
- [ ] moq-video builds with `--features vaapi` on Linux and for the Android target
      with the new arms compiled; the thread-safety compile check passes.
- [ ] No ffmpeg and no dlopen introduced by B1 itself.
