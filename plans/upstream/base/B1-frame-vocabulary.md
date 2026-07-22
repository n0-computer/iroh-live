# B1. Public GPU-frame vocabulary (Native + Frame variants + moq-frame crate)

> Campaign: upstream | Kind: base plan | Branch: up/base (B1 lands first in the
> base series) | PR target: base branch, then moq main | Read ../0-overview.md first.

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
moq-video-render crate depend on, and it is designed to be the first thing landed
and the first thing agreed upstream in the base RFC.

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
       // VAAPI decode and PipeWire DMA-BUF capture, consumed by the VAAPI encoder.
       // Gated on a new shared `dmabuf` feature (see below), not on `vaapi`,
       // because PipeWire produces DMA-BUF without VAAPI. V4L2 EXPBUF would be a
       // third producer, but only once the V4L2 zero-copy follow-up is pursued
       // (see the feature-design note below); today V4L2 capture is always
       // compiled and downloads to I420, so it is not yet a `dmabuf` producer.
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

   `download_i420` is real work, not a trivial mmap-and-copy, and B1 must say so.
   The `DmaBuf` carries a DRM `modifier` precisely because the buffer may be tiled
   or compressed (a CCS modifier), and a PRIME fd under a tiled or compressed
   modifier cannot be plain-`mmap`'d into linear I420. The download path branches on
   `modifier()`: a linear modifier (`DRM_FORMAT_MOD_LINEAR`) can map and NV12- or
   I420-pack directly, while any tiled or compressed modifier needs a VA readback,
   binding the surface and calling `vaDeriveImage` / `vaGetImage` (or a VPP detile
   blit to a linear surface) before packing. The VA-surface-backed implementation,
   including this readback, is the vaapi-decode leaf's job (it owns the moq-vaapi
   VA display); B1 declares the method and documents the linear-versus-tiled branch
   keyed on `modifier()` so the leaf and the PipeWire producer agree on the
   contract. On Android, `android::HwBuffer::download_i420` is likewise real work:
   `AHardwareBuffer_lock` maps the buffer, and the NV12 result is deinterleaved into
   I420 before returning; the android-mediacodec leaf supplies that body.

4. **The public `Native` vocabulary.** Two homes are on the table; the decision is
   an RFC item (see Adaptation notes). The recommendation is the in-moq-video
   module, because the standalone crate as first sketched is a dependency cycle:

   - Recommended: a public module `pub mod native;` in moq-video, re-exported at
     `lib.rs` beside `pub use error::Error;` and `pub use size::Size;`
     (`rs/moq-video/src/lib.rs:57-58`). It holds `Native`, `DmaBuf`, `Plane`, and a
     re-export of moq's `Size`. This home has no cycle: `DmaBuf` can hold a
     moq-vaapi surface exporter and back its download with moq-video's `I420`
     (`frame.rs:80`) and `Error` (`error.rs`), because the module lives inside
     moq-video and sits above moq-vaapi, exactly where those types already are. It
     is also the smaller ask upstream. The one thing it cannot do is let an
     out-of-tree render crate share the vocabulary without depending on all of
     moq-video; that is the tradeoff, and it is acceptable because the renderer can
     consume `Native` through moq-video's public API.
   - Alternative (a standalone `rs/moq-frame` crate): only viable if the exporter is
     abstracted behind a trait and the crate carries its own error type. A leaf
     crate that other crates depend on cannot itself depend on them, so a `DmaBuf`
     that stored a concrete moq-vaapi exporter or returned moq-video's `Error` /
     `I420` (all of which sit *above* a would-be `moq-frame`) is a cycle and does not
     compile. To make the crate work, `DmaBuf` would hold the exporter behind a
     trait or boxed closure (for example `Arc<dyn Fn() -> Result<OwnedFd,
     ExportError>>`), define its own `ExportError` (not moq-video's `Error`), and
     keep all I420 download on the moq-video-side `crate::frame::dmabuf::Frame`
     (Target 3) rather than on the public type. moq-video would then depend on
     `moq-frame` and re-export `Native`, and the root `Cargo.toml` would need the
     workspace wiring in step 7. Take this route only if a shared render crate is a
     firm requirement; otherwise the module home is simpler and cycle-free.

   The public surface, identical either way, and the frozen contract leaves code
   against verbatim:

   ```rust
   /// A GPU-resident frame's platform handle. Names a kernel or OS object, never a
   /// backend type, so it respects moq's "no backend type in the public API" rule
   /// (rs/moq-video/src/lib.rs:37-44).
   #[non_exhaustive]
   pub enum Native {
       #[cfg(all(target_os = "linux", feature = "dmabuf"))]
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

   The Linux arm is gated `all(target_os = "linux", feature = "dmabuf")`, matching
   the cfg of the `DmaBuf` payload's exporter. An earlier sketch gated the arm on
   bare `target_os = "linux"`, but that does not compile on a
   `--no-default-features` Linux build: the `DmaBuf` type and its exporter only
   exist under `dmabuf` (pulled in by `vaapi` or `pipewire`), so a Linux build with
   `dmabuf` off would name an arm whose field type is absent. Gating the arm on
   `feature = "dmabuf"` keeps the enum total on every build. A caller that wants to
   name `Native::DmaBuf` therefore compiles with a producer feature on, which is the
   only configuration where a `DmaBuf` can exist anyway. The alternative (keep the
   bare `target_os` gate and give the field an uninhabited placeholder plus an
   `unreachable!` on a `dmabuf`-less build) is more machinery for no gain, so the
   feature gate is the frozen-contract choice.

   `CvPixelBuffer(Surface)`, `D3d11(Texture)`, and `Cuda(Cuda)` wrap moq's existing
   `macos::Surface`, `d3d11::Texture`, and `cuda::Frame` by making a thin public
   newtype, not by making the internal types public, so moq keeps those internals
   free to change. Because `native(&self)` (step 5) builds an owned `Native` from a
   borrow, each wrapped handle must be cheaply clonable. `cuda::Frame` already
   derives `Clone` (`frame.rs:508`, a refcount bump), but `macos::Surface`
   (`frame.rs:352`) and `d3d11::Texture` (`frame.rs:721`) do NOT, despite their doc
   comments describing clone as a cheap retain / `AddRef`. B1 must add `Clone` to
   both (a `CFRetained` clone and a COM `AddRef` respectively, no pixel copy), or
   give each a `retain()` helper the newtype calls. Without this, `native()` cannot
   return an owned `Native` and will not compile. The public newtypes then clone the
   inner handle through.

   Send/Sync of the new handles is not a free compile check. The
   `frame_and_consumer_are_thread_safe` test (`decode/mod.rs:104-118`) requires
   `Frame: Send + Sync`, and the same must hold for every `Native` variant. A VA
   surface (a raw `VADisplay` pointer plus a `VASurfaceID`) and an `AHardwareBuffer`
   are `!Send`/`!Sync` by default, exactly as `objc2`'s CoreVideo types are, so
   `dmabuf::Frame`, `android::HwBuffer`, and their public wrappers each need an
   explicit `unsafe impl Send`/`Sync` carrying a written safety argument, modeled on
   the one `macos::Surface` already states (`frame.rs:359-369`: refcounted handle,
   thread-safe retain/release, `&self` access is a plain read, no shared write). B1
   spells out that argument per variant rather than assuming the bound is inherited.

5. **The re-export** at `rs/moq-video/src/lib.rs` beside `pub use error::Error;` and
   `pub use size::Size;` (`lib.rs:57-58`): `pub use native::Native;` (module home)
   or `pub use moq_frame::Native;` (crate home).

## Implementation steps

1. Land the public vocabulary types first, standalone and testable, in whichever
   home the RFC picks: `Native`, `DmaBuf`, `Plane`, and a `Size` alias or re-export
   of moq's `Size`. On non-Linux, non-Android hosts the enum still has the macOS /
   Windows / CUDA arms, so the type compiles everywhere. This is the piece
   upstream review weighs as the public-API commitment; keep it minimal.
2. Add the `dmabuf::Frame` backing type behind
   `cfg(all(target_os = "linux", feature = "dmabuf"))` with `download_i420`,
   `export`, and the descriptor accessors, so `Frame::DmaBuf` has a payload. Where
   the real VA-surface exporter is not yet available (moq-vaapi export is the
   vaapi-decode leaf's job), stub `export` to the moq-vaapi entry point the leaf
   fills, and gate the module so a build without `dmabuf` never sees it.

Feature design: add a new `dmabuf` Cargo feature to moq-video. It is the base
gate for the `Frame::DmaBuf` variant and its `dmabuf` module. The DMA-BUF
producers and consumers that exist today are the `vaapi` and `pipewire` features,
so those two are the enablers of `dmabuf`. moq-video declares exactly four
features today (`nvenc`, `nvdec`, `vaapi`, `pipewire`, at
`rs/moq-video/Cargo.toml:15-39`); there is no `v4l2` feature, because V4L2 capture
is unconditional through `v4l = "0.14"` (`Cargo.toml:101`) and downloads to I420.
A `v4l2` feature that also enabled `dmabuf` would be introduced only later, if and
when V4L2 EXPBUF zero-copy is pursued, at which point it joins the enabler set.
The concrete `[features]` edits, spelled out so a PR agent applies them verbatim:

```toml
# NEW: base gate for the DMA-BUF frame variant and its `frame/dmabuf.rs` module.
# Enabled by every feature that produces or consumes a PRIME-exportable buffer.
dmabuf = []
# was `vaapi = ["dep:moq-vaapi"]` (Cargo.toml:34)
vaapi = ["dmabuf", "dep:moq-vaapi"]
# was `pipewire = ["dep:pipewire", "dep:ashpd"]` (Cargo.toml:39)
pipewire = ["dmabuf", "dep:pipewire", "dep:ashpd"]
```

`dmabuf = []` adds no dependency of its own: the variant is pure descriptor
metadata plus an fd dup, and `export` needs only `use std::os::fd::OwnedFd;` from
`std`, so no new crate is pulled in by the feature itself. This resolves the open
question the pipewire-dmabuf leaf raises: it is a DMA-BUF producer that does not
otherwise need VAAPI, and it gates on `dmabuf` (pulled in by `pipewire`).
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
7. Wire the Cargo and workspace pieces. For the recommended module home this is
   just the `[features]` stanzas above (`dmabuf = []`, `vaapi = ["dmabuf", ...]`,
   `pipewire = ["dmabuf", ...]`) plus the `use std::os::fd::OwnedFd;` import in the
   `dmabuf` module and the public `native` module for `DmaBuf::export`; no root
   `Cargo.toml` change is needed, since no new crate is added. For the alternative
   `moq-frame` crate home this step also edits the root `Cargo.toml`: add
   `"rs/moq-frame"` to both `members` (`Cargo.toml:2-31`) and `default-members`
   (`Cargo.toml:32-60`), neither of which lists it today, add a
   `[workspace.dependencies] moq-frame = { version = "...", path = "rs/moq-frame" }`
   entry, and change moq-video's dependency block to `moq-frame = { workspace = true
   }`. The crate's own `Cargo.toml` declares the same `dmabuf` feature so the gated
   types line up across the two crates.

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
- The in-moq-video module versus `moq-frame` crate choice is the RFC decision, and
  the module is now the recommendation (Target 4). The crate as first sketched is a
  dependency cycle: a public `DmaBuf` that holds a moq-vaapi exporter and returns
  moq-video's `Error` and `I420` sits above both crates, so a leaf crate they depend
  on cannot hold those types. The module home avoids the cycle entirely because it
  lives inside moq-video. The crate stays a noted alternative, viable only if the
  exporter is abstracted behind a trait or boxed closure and the crate carries its
  own `ExportError`, and it is worth that extra machinery only if a render crate must
  share the vocabulary without depending on all of moq-video. Whichever lands, the
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
