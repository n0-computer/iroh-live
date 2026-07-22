# 3u. moq Changes to Upstream Everything Capture and Codec

Status: design artifact. This document specifies, concretely and in moq's own
vocabulary, every change moq-video (and its sibling crates moq-vaapi, moq-audio,
moq-mux) would need so that iroh-live can upstream its entire capture and codec
surface: the hardware encoders and decoders, the zero-copy capture-to-encode and
decode-to-render paths, the renderer, the audio codecs, and the pre-encoded
capture concept. It is the concrete follow-on to the conclusions in
[8-upstream-plan.md](8-upstream-plan.md) (the contribution catalog C1 through
C14) and [3z-compare-zerocopy.md](3z-compare-zerocopy.md) (requirements U1
through U4), and it draws its inventory of both codebases from
[maps/rusty-codecs.md](maps/rusty-codecs.md), [maps/rusty-capture.md](maps/rusty-capture.md),
and [maps/moq-video.md](maps/moq-video.md).

moq merged its long-lived rewrite branch into main on 2026-07-21, so there is no
longer a dev line to target: everything described here is against moq main,
working tree `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`, and every `rs/...`
citation was read from that tree directly. Where a line number differs by a few
lines from the older map pin, the citation below is the value verified at
`3a3e0ea8`.

The framing is deliberately honest about a fork in the road that the maintainer
must decide, and Section 4 is written as the agenda item for that decision: moq
vendors every backend in-tree behind `pub(crate)` seams, and asking it to accept
external out-of-tree backends is a different and larger request than asking it to
accept our backends into its tree. Sections 1 through 3 specify the changes that
are needed either way (the frame vocabulary, the render home, the PTS plumbing,
the interface reconciliations); Section 4 isolates the one change (a public,
registerable `Backend` trait) that is needed only if we keep any backend in our
own tree.

---

## Section 1: Zero-copy improvements to upstream (render and capture)

The single fact that governs this whole section: moq's raw frame type
`crate::frame::Frame` is `pub(crate)` (`rs/moq-video/src/frame.rs:23`), its
variants are `Surface(macos::Surface)`, `Texture(d3d11::Texture)`,
`Cuda(cuda::Frame)`, and `I420(I420)` (`frame.rs:24-35`), and the only public
way a decoded frame leaves the crate is `decode::Frame::into_i420()`
(`rs/moq-video/src/decode/mod.rs:94-101`, a CPU download) or
`Encoder::encode(&decode::Frame, keyframe)` (the in-tree transcode input,
`encode/encoder.rs:249-251`). There is no `DmaBuf` variant, no `AHardwareBuffer`
variant, no public GPU handle, and no renderer. Every zero-copy improvement below
therefore begins with the same keystone: a public platform-handle vocabulary
(requirement U1, catalog item C2), because until a GPU handle can cross the crate
boundary, none of our capture backends can feed a moq encoder a GPU frame and
none of our importers can read a moq decoder's output.

### 1a. Capture-side zero-copy (capture -> encode)

**What we have.** Our capture layer delivers GPU handles that our hardware
encoders consume with no CPU download: PipeWire negotiates `SPA_DATA_DmaBuf` and
wraps the dup'd fd as a `PipeWireDmaBufFrame` exposing
`NativeFrameHandle::DmaBuf` (`rusty-capture/src/platform/linux/pipewire.rs:145-247,
721-766`); ScreenCaptureKit wraps each IOSurface-backed `CVPixelBuffer`
(`rusty-capture/src/platform/apple/screen.rs:182-209`); and the Android plan
carries an `AHardwareBuffer` through `HardwareBufferInfo`
(`rusty-codecs/src/format.rs:89-109`). The VAAPI encoder imports a matching NV12
DMA-BUF as a VA surface through a hand-built `VADRMPRIMESurfaceDescriptor` with
no copy (`rusty-codecs/src/codec/vaapi/encoder.rs:87-118, 1268-1297`), and routes
dimension or format mismatches through VPP on the GPU rather than downloading.

**What moq has.** On macOS the shape already exists: `capture/surface.rs:12-19`
extracts a `CVPixelBuffer` as `Frame::Surface`, and VideoToolbox consumes it
directly. On Windows the D3D11 `Frame::Texture` path is equivalent. On Linux moq
is CPU end to end: PipeWire converts to CPU I420, and the VAAPI encoder accepts
only tightly packed CPU NV12 (`maps/moq-video.md` section 1, verified against
the `frame.rs` enum having no `DmaBuf` variant). There is no `AHardwareBuffer`
type anywhere because there is no Android support.

**The moq changes.**

1. Add two cfg-gated variants to `crate::frame::Frame`
   (`rs/moq-video/src/frame.rs:23-36`). This is mechanical because the enum is
   already per-platform cfg-gated:

   ```rust
   pub(crate) enum Frame {
   	#[cfg(target_os = "macos")]
   	Surface(macos::Surface),
   	#[cfg(target_os = "windows")]
   	Texture(d3d11::Texture),
   	#[cfg(all(target_os = "linux", feature = "nvdec"))]
   	Cuda(cuda::Frame),
   	// NEW: any Linux GPU frame with a PRIME-exportable descriptor, produced by
   	// VAAPI capture/decode and PipeWire DMA-BUF capture, consumed by the VAAPI
   	// encoder. Gated on `vaapi` (or a new `dmabuf` feature) rather than `nvdec`.
   	#[cfg(all(target_os = "linux", feature = "vaapi"))]
   	DmaBuf(dmabuf::Frame),
   	// NEW: Android MediaCodec / ImageReader output and camera capture.
   	#[cfg(target_os = "android")]
   	HardwareBuffer(android::HwBuffer),
   	I420(I420),
   }
   ```

   The private enum grows two arms in `width`/`height`/`to_i420`
   (`frame.rs:39-74`); `to_i420` gains a CPU download fallback for each new
   variant so the software encode path and the container's CPU consumers keep
   working.

2. Give `Frame::DmaBuf` a backing type that mints an fd on demand rather than
   storing one per buffered frame, matching our lazy-export design
   (`format.rs:495-502`, "transient export on access"). The moq-side type mirrors
   our `DmaBufInfo` (`rusty-codecs/src/format.rs:505-525`): DRM fourcc, modifier,
   coded and display dimensions, and per-plane offset and pitch, but holds the
   exporting object (the VA surface, or the dup source) and produces an
   `OwnedFd` only when a consumer asks. This is the one place our model is
   strictly better than a store-the-fd design and it should be preserved.

3. Make the handle public, since `Frame` itself stays `pub(crate)`. The public
   vocabulary is a new closed-but-non-exhaustive enum of concrete OS handles, the
   U1 sketch from 3z restated in moq naming. Preferred home is a small new
   `moq-frame` crate so moq-video, moq-transcode, and a render crate can share
   it; the fallback is a public module in moq-video:

   ```rust
   /// A GPU-resident frame's platform handle. Names a kernel or OS object, never
   /// a backend, so it respects the "no backend types in the public API" rule
   /// (rs/moq-video/src/lib.rs:35-44).
   #[non_exhaustive]
   pub enum Native {
   	#[cfg(target_os = "linux")]
   	DmaBuf(DmaBuf),          // fd(): OwnedFd on demand, fourcc, modifier, planes
   	#[cfg(target_os = "macos")]
   	CvPixelBuffer(Surface),  // moq's existing macos::Surface, made public
   	#[cfg(target_os = "windows")]
   	D3d11(Texture),          // moq's existing d3d11::Texture, made public
   	#[cfg(all(target_os = "linux", feature = "nvdec"))]
   	Cuda(Cuda),              // moq's existing cuda::Frame, made public
   	#[cfg(target_os = "android")]
   	HardwareBuffer(HwBuffer),
   }

   pub struct DmaBuf { /* private exporter */ }
   impl DmaBuf {
   	pub fn fourcc(&self) -> u32;
   	pub fn modifier(&self) -> u64;
   	pub fn coded_size(&self) -> Size;
   	pub fn display_size(&self) -> Size;
   	pub fn planes(&self) -> &[Plane];   // { offset: u32, pitch: u32 }
   	/// Exports a fresh dup'd descriptor; no fd is held per frame.
   	pub fn export(&self) -> Result<OwnedFd, Error>;
   }
   ```

4. Define the capture-to-encode contract in terms of the private `Frame`. A
   capture backend produces `Frame::DmaBuf` or `Frame::HardwareBuffer` into the
   existing `FrameStream` (`rs/moq-video/src/capture/mod.rs:230-242`), exactly as
   `capture/surface.rs:12-19` produces `Frame::Surface` today; nothing about the
   `FrameChannel` or the `read().await -> Option<Frame>` surface changes. An
   encoder backend consumes the handle by matching the variant, precisely as
   VideoToolbox matches `Frame::Surface(surface) => surface.buffer.clone()`
   today. The VAAPI encoder backend gains:

   ```rust
   fn encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe: bool)
   	-> Result<Vec<Packet>, Error>
   {
   	let surface = match frame {
   		#[cfg(all(target_os = "linux", feature = "vaapi"))]
   		Frame::DmaBuf(db) => self.import_prime(db)?,   // VADRMPRIMESurfaceDescriptor
   		other => self.upload_i420(other.to_i420()?)?,  // existing CPU path
   	};
   	// VPP scale/csc on mismatch, then encode; our encoder.rs:1009-1118, :1268-1297.
   }
   ```

   The `import_prime` body is our `rusty-codecs/src/codec/vaapi/encoder.rs:87-118`
   descriptor builder and `:1268-1297` dispatch, carried over. (The `Packet` and
   `timestamp` argument are the PTS-through-encode change of Section 2, needed
   because pipelined backends cannot honestly stamp output otherwise.)

The public surface touched is just the `Native` vocabulary; the capture and
encode wiring is all behind moq's existing `pub(crate)` seams, so points 1, 2,
and 4 are non-breaking additions and point 3 is the only new public API. This is
catalog items C1 and C11 resting on C2.

### 1b. Render-side zero-copy (decode -> render)

This is the harder ask, and the maintainer explicitly wants it upstreamed rather
than kept forever local. Two things stand in the way, and they are separable.

**Problem one: moq decoders download to CPU.** VideoToolbox decode requests NV12
and downloads every frame to packed I420 (`maps/moq-video.md` section 2,
`decode/backend/videotoolbox.rs`); Media Foundation downloads the DXVA NV12 pool
to I420; only NVDEC keeps its output on the GPU as `Frame::Cuda`, and that
variant is `pub(crate)` and produced only to feed NVENC. So even where a moq
decoder runs on the GPU, the surface is unreachable, and on non-NVIDIA Linux
there is no hardware decode at all (the Linux decode candidates are NVDEC and
openh264, `decode/backend/mod.rs:89-114`). Our importers would have nothing to
import.

**Problem two: moq renders nothing in Rust.** Rendering is deliberately the
application's problem (`3z` section 2d), which is a defensible scope choice for a
relay and transcode stack. Our render stack is roughly 3,500 lines of wgpu, ash,
glow, and objc2 dependencies (`WgpuVideoRenderer` in `rusty-codecs/src/render.rs`,
Vulkan DMA-BUF import with the `VppRetiler` Y_TILED-to-CCS re-tile in
`render/dmabuf_import.rs`, EGLImage import in `render/gles_dmabuf.rs`, Metal
`CVMetalTextureCache` aliasing in `render/metal_import.rs`), and pushing it into
moq-video contradicts moq's minimal-dependency posture and its scope.

**The moq changes for problem one (decoders export a handle).** Change each GPU
decoder backend to hand out its surface instead of downloading, and expose that
surface publicly:

1. Add a public accessor to `decode::Frame` beside `into_i420()`
   (`rs/moq-video/src/decode/mod.rs:94-101`):

   ```rust
   impl decode::Frame {
   	/// The platform GPU handle when the frame is GPU-resident; `None` for a
   	/// CPU I420 frame. Lets a renderer import without a CPU round trip.
   	pub fn native(&self) -> Option<Native>;
   }
   ```

   The private `decode::Frame::inner` (`decode/mod.rs:36-46`) already holds the
   `crate::frame::Frame` union, so `native()` is a match producing the U1 handle;
   the existing `into_i420()` stays as the universal CPU fallback.

2. Add a VAAPI decode backend to the decode candidate table
   (`rs/moq-video/src/decode/backend/mod.rs:89-108`), producing `Frame::DmaBuf`.
   Our `rusty-codecs/src/codec/vaapi/decoder.rs` is the reference, including the
   two hard-won details our map records: `vaSyncSurface` before
   `vaExportSurfaceHandle` because the export does not synchronize
   (`vaapi/decoder.rs:247-300`), and per-frame export caching so repeated handle
   access from the render thread and the playout buffer dup the same fd rather
   than re-exporting (`vaapi/decoder.rs:85-113`). This closes moq's largest
   decode gap (any hardware H.264 decode on Intel and AMD Linux) at the same time
   as it feeds the renderer. Depends on moq-vaapi growing surface export
   (`vaExportSurfaceHandle` plus `VADRMPRIMESurfaceDescriptor`) and the decode
   half of cros-codecs, the largest single piece of the whole program (3z section
   4, ordering note).

3. Retain the surface on VideoToolbox and Media Foundation decode instead of
   downloading. On macOS this is the smallest change on the list: the `Surface`
   type, its `Send + Sync` justification, and retain-on-clone all exist for the
   capture path already, so the decode backend keeps the NV12 `CVPixelBuffer` as
   `Frame::Surface` rather than calling `download_i420`, and our
   `render/metal_import.rs` works unchanged over `native()` (this is part of
   requirement U2). The Windows analog keeps the DXVA texture as `Frame::Texture`.

**The moq changes for problem two (a home for the renderer).** The maintainer
named three placements, and all three rest on the same enabling change (the public
`native()` accessor of change 1); they differ only in who carries the graphics
dependency tree and the vendor FFI. [3z section 4](3z-compare-zerocopy.md) works
through each in full; the summary is:

- Option A, an in-tree `moq-video-render` crate. moq gains a Rust playback story,
  but it also takes on wgpu, ash, glow, and objc2-metal dependencies it must
  review and cannot test on the hardware where the zero-copy paths matter (Intel
  Y_TILED re-tiling was validated only on Meteor Lake hardware we own). This
  contradicts moq's minimal-dependency rule and its stated scope.

- Option B, an out-of-tree crate consuming moq's public `Native` handles. The
  renderer imports `decode::Frame::native()`, selects a path per frame, and falls
  back to `into_i420()` when a zero-copy path fails, exactly as
  `render.rs:267-359` does today over our own handle. moq's semver surface stays
  free of graphics dependencies, and the crate is a working proof that the U1
  vocabulary is sufficient for a third party.

- Option C, keep the renderer in iroh-live but fully aligned to moq's frame model.
  It consumes `moq_video::decode::Frame` and its `native()` accessor directly and
  deletes our parallel `FrameData`/`NativeFrameHandle`, so there is one frame
  vocabulary rather than two, but the render stack stays iroh-live-private and
  never becomes part of the moq ecosystem.

**Recommendation: Option B.** It is aligned with moq's own scope decision (they
render nothing today and say so), it keeps 3,500 lines of graphics code and its
dependency weight out of moq's tree, and its strategic value is precisely the
proof: a third-party renderer working purely over the public handles is the
strongest possible argument that U1 is complete, which is the argument for U1 in
the first place. This is catalog item C13's conclusion, and it is unchanged by
the main merge. The only thing Option B needs from moq is the public `native()`
accessor of change 1; the decoder-export changes 2 and 3 are what give that
accessor something to return on Linux and Apple. Option C is the minimal fallback
if the public-handle work stalls, since it still deletes our parallel frame model
and aligns us to moq's vocabulary while forgoing reusability. Revisit in-tree
adoption (Option A) only if moq-video is ever repositioned as a client playback SDK
rather than a relay and transcode stack.

---

## Section 2: Opening the closed Backend variants with a trait

### The current shape

moq's encode and decode `Backend` traits are `pub(crate)`
(`rs/moq-video/src/encode/backend/mod.rs:37`, `decode/backend/mod.rs:67`), and
selection runs through an internal `Candidate` table: a `struct Candidate { name:
&'static str, codecs: &'static [Codec], open: fn(&Config) -> Result<Box<dyn
Backend>, Error> }` (`encode/backend/mod.rs:60-64`) held in two `const` slices
`HARDWARE` and `SOFTWARE` (`encode/backend/mod.rs:68-102`), with
`open(config)` filtering by codec support and ordering per `Kind`
(`encode/backend/mod.rs:106-134`). The decode side is identical with a `supports:
fn(Codec) -> bool` field to express NVDEC's three codecs
(`decode/backend/mod.rs:80-114`). Nothing outside the crate can name a `Backend`,
add a `Candidate`, or reach the tables. Our side, by contrast, selects backends
through public `VideoEncoder` and `VideoEncoderFactory` traits with a `const ID`
plus `with_config` factory (`rusty-codecs/src/traits.rs:311-377`) and a
`VideoDecoder` trait (`traits.rs:379-410`), and dispatches through public runtime
enums (`rusty-codecs/src/codec.rs`, `codec/dynamic.rs`).

### The draft change

Make the trait public as a sealed-with-escape trait, add a registration API for
the tables, extend `Kind` selection to include registered candidates, and thread
the PTS through `encode`.

1. Publish the trait behind a public `Packet` output type and the U1 `Frame`
   input, and add the PTS argument that pipelined backends need:

   ```rust
   /// An opened video encoder backend. Additive-sealed: external crates implement
   /// it, but only through the public `Native`/CPU frame vocabulary and the public
   /// `Packet` output, so moq never exposes a codec-internal type.
   pub trait Backend: Send {
   	/// Encode one frame stamped with its presentation `timestamp`. `keyframe`
   	/// forces an IDR. Returns zero or more packets, each carrying the timestamp
   	/// of the frame it belongs to, so a pipelined backend (V4L2 M2M, Android
   	/// MediaCodec) can drain a frame decoded several inputs ago without
   	/// mis-stamping it.
   	fn encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe: bool)
   		-> Result<Vec<Packet>, Error>;
   	fn finish(&mut self) -> Result<Vec<Packet>, Error>;
   	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;
   	fn name(&self) -> &str;
   }

   #[non_exhaustive]
   pub struct Packet { pub payload: Bytes, pub timestamp: Timestamp }
   ```

   The PTS-through-encode change is the same one 8-upstream-plan.md calls C3 and
   D3: today `Backend::encode(&mut self, frame: &Frame, keyframe: bool) ->
   Result<Vec<Bytes>, Error>` (`encode/backend/mod.rs:40`) stamps output at the
   call site, which is correct only for the zero-frame-delay backends moq ships
   (all five drain per call). The decode side already carries per-picture
   timestamps in `Decoded { timestamp, frame }` (`decode/backend/mod.rs:53-62`),
   so this makes encode symmetric with decode and changes no existing backend's
   observable behavior (each echoes the current frame's timestamp). It is a
   prerequisite for V4L2 M2M and Android MediaCodec whether they land in-tree or
   out-of-tree.

2. Add a registration entry point so an external crate can contribute a
   candidate without editing moq's `const` tables. The tables become a
   `OnceLock<Vec<Candidate>>` seeded from the built-in `const` slices, plus:

   ```rust
   /// A registerable backend: a name, the codecs it emits, and an opener.
   #[non_exhaustive]
   pub struct Registration {
   	pub name: &'static str,
   	pub codecs: &'static [Codec],
   	pub open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
   }

   /// Register an external encoder backend, consulted by `Kind::Auto`
   /// (after built-in hardware, before software), `Kind::Hardware`, and
   /// `Kind::Named`. Call once at startup, before the first `Encoder::new`.
   pub fn register_encoder(reg: Registration);
   ```

   The encode-side `open(config)` (`encode/backend/mod.rs:106-134`) already builds
   its candidate list by chaining `HARDWARE` and `SOFTWARE`; it chains the
   registered slice too, so `Kind::Auto` sees external candidates in priority
   order, `Kind::Named(name)` can select one by name, and `Kind::Hardware`
   includes those a registrant flags as hardware. No change to the public `Kind`
   enum (`encode/encoder.rs:34-48`) is needed; `Named(String)` already routes by
   name.

   The decode side needs its own entry point rather than a mirror, because the
   decode `Candidate` is not shaped like the encode one: it carries a `supports:
   fn(Codec) -> bool` predicate instead of a `codecs` slice, and its opener takes
   the concrete codec as an argument, `open: fn(Codec, &Config) -> Result<Box<dyn
   Backend>, Error>` (`decode/backend/mod.rs:78-85`), so a single backend can
   serve several codecs the way NVDEC serves H.264, H.265, and AV1. The decode
   registration matches that shape:

   ```rust
   /// A registerable decoder backend: a name, a codec-support predicate, and an
   /// opener that receives the concrete codec to open.
   #[non_exhaustive]
   pub struct DecodeRegistration {
   	pub name: &'static str,
   	pub supports: fn(Codec) -> bool,
   	pub open: fn(Codec, &Config) -> Result<Box<dyn Backend>, Error>,
   }

   /// Register an external decoder backend, consulted alongside the built-in
   /// decode candidates in priority order. Call once at startup, before the first
   /// `Decoder::new`.
   pub fn register_decoder(reg: DecodeRegistration);
   ```

   The decode `open(codec, config)` (`decode/backend/mod.rs:119`) chains the
   registered slice onto its `HARDWARE`/`SOFTWARE` scan exactly as the encode side
   does, filtering by the `supports` predicate.

3. Keep moq's stability guarantee with a sealing escape hatch. The concern moq
   will raise is that a public `Backend` trait freezes the frame vocabulary and
   the packet shape into semver. The mitigation is that `Backend` only ever sees
   public types: the U1 `Frame`/`Native` input and the `#[non_exhaustive]`
   `Packet` output, never a codec-internal type, so moq keeps the "no backend
   types in the public API" invariant (`lib.rs:35-44`) even with a public trait,
   because the trait traffics only in vocabulary types. Additive changes stay
   possible: new `Frame` variants are cfg-gated and additive, `Packet` is
   `#[non_exhaustive]`, and a new required trait method is the only genuinely
   breaking move, avoidable by defaulting.

### The tradeoff, stated for moq

A public `Backend` trait plus registration is a real stability cost: it turns the
frame vocabulary, the `Packet` shape, and the `Registration`/`Kind` interaction
into semver surface that reshaping in review can no longer touch freely. moq's
current `pub(crate)` seam exists precisely so they can refactor backends without a
version bump, and they vendored NVENC and trimmed cros-libva rather than depend on
external crates specifically to keep that freedom. The honest read is that moq
does not need this change for any backend it is willing to carry in-tree: an
in-tree VAAPI decode backend just adds a `const` `Candidate`, no public trait
required. The registration API is worth it to moq only if it decides it wants an
ecosystem of out-of-tree backends, which is the Section 4 question. So Section 2
is best presented to the maintainer as conditional: the PTS-through-encode change
(point 1's timestamp argument) is unconditionally needed and small; the public
trait and registration (points 2 and 3) are needed only on the external-backend
path.

---

## Section 3: Other interface differences to reconcile

Everything else a full capture and codec upstreaming touches, with the
reconciliation and who changes for each.

1. **Timestamp type.** moq uses `moq_net::Timestamp` end to end at every API
   boundary (`decode/backend/mod.rs:17`, `decode/mod.rs:36-46`), and its style
   rule forbids `Duration` at boundaries. We use `Duration` on `VideoFrame` and
   `EncodedFrame`/`MediaPacket` (`rusty-codecs/src/format.rs:568-811`,
   microsecond-resolution). Reconciliation: we change. Everything we contribute
   adopts `moq_net::Timestamp`; internally our OS-thread pipelines can keep
   `Duration` behind the boundary if convenient, but no contributed signature
   carries it. This is a mechanical rename across every backend we upstream.

2. **Config versus hang catalog types.** moq's encode `Config`
   (`encode/encoder.rs:55-70`) and decode `Config` (`decode/decoder.rs:43-58`)
   are construction configs; the wire description comes from `hang::catalog`
   types (`VideoCodec`, `VideoConfig`, read in `decode/decoder.rs:14`). We keep a
   parallel catalog-config family that mirrors hang 1:1 with `From` impls
   (`rusty-codecs/src/config.rs`) plus a separate construction-config family
   (`format.rs`). Reconciliation: we change, dropping our mirror family in
   contributed code and using hang catalog types directly, which is what moq's
   producer and consumer already do. Our `From` impls document the mapping and can
   be discarded once we consume moq-video.

3. **Error types.** moq uses `#[non_exhaustive]` `thiserror` enums with
   actionable variants and no `anyhow` in public signatures (`Error` in
   `rs/moq-video/src/error.rs`, re-exported at `lib.rs:57`), for example
   `Error::BitrateUnsupported` and `Error::NoEncoder(String)`. We use `anyhow` in
   trait return types throughout (`traits.rs` methods return `anyhow::Result`).
   Reconciliation: we change. Contributed backends return moq's `Error`, adding
   variants where a failure mode is new (a `DmaBufImport` variant, a
   `SurfaceExport` variant). Who changes: us for the signatures, moq for the new
   variants (additive on a `#[non_exhaustive]` enum).

4. **One-shot versus streaming encode.** moq's public `Encoder` is one-shot per
   frame: `encode_rgba`/`encode_i420` return `Vec<Bytes>` synchronously
   (`encode/encoder.rs:219-249`), and the backend drains per call. Our
   `VideoEncoder` is push/pop streaming: `push_frame` then `pop_packet`
   (`traits.rs:311-377`), which matches pipelined hardware queues naturally.
   Reconciliation: moq's `Backend::encode` returning `Vec<Packet>` is compatible
   with a pipelined backend as long as the PTS travels with the packet (Section
   2, point 1), so the streaming nature is expressible without changing the
   public one-shot `Encoder` front end; the backend simply returns zero packets
   on a frame that is still in the device queue and several on a later drain. No
   moq public API change beyond the `Packet` timestamp; the reconciliation is
   that our streaming backends adopt the `Vec<Packet>`-per-frame return.

5. **Audio Opus knobs.** moq's Opus encoder is in moq-audio. We add three things
   worth upstreaming (catalog C7): runtime `set_bitrate` via one `OPUS_SET_BITRATE`
   ctl (`rusty-codecs/src/codec/opus/encoder.rs:206-219`), which is also the
   precondition for extending moq's video-style congestion rate control to audio;
   lookahead-derived pre-skip, where moq hardcodes OpusHead pre-skip 0 and we
   query `OPUS_GET_LOOKAHEAD` (`opus/encoder.rs:91-108`) so conforming decoders
   trim the encoder delay, a genuine correctness fix that touches moq-mux's opus
   `Config` and moq-audio; and FEC/PLC groundwork, where neither side conceals
   loss today, so the ask is only an API reservation (`decode_lost(duration)`
   shape) with the implementation deferred to phase 3c. Who changes: moq accepts
   the setter and the pre-skip field, we supply both.

6. **PCM as a codec.** We carry a `Pcm` audio codec (`rusty-codecs/src/codec/pcm`,
   20 ms LE-f32 chunks, ID `"pcm"`) that slots into moq-audio's
   `#[non_exhaustive]` codec enum in about 100 lines plus our impl. The blocker is
   that the hang catalog has no PCM codec variant, so the interop value is nil and
   a decline is the expected outcome. Reconciliation: offer it, expect it kept
   local for tests. Who changes: nobody, most likely.

7. **Capture Source and demand-gating versus our start/stop.** moq's capture is a
   `pub enum Source { Camera, Display, Window, App }`
   (`capture/mod.rs:65-86`) plus a `pub(crate) FrameStream` with `read().await`
   (`capture/mod.rs:230-242`), and crucially the encoder runs strictly on demand:
   `publish_capture` advertises the track up front, opens the device only while a
   subscriber watches, and releases it when the last viewer leaves via
   `moq_net::track::Demand`. Our `VideoSource` is a pull trait with explicit
   `start`/`stop` and no transport awareness (`traits.rs:229-259`), because
   demand-gating lives one layer up in iroh-live. Reconciliation: moq's
   demand-gating model is better and stays; our backends adopt the `FrameStream`
   producer shape (produce a `Frame` into the channel), and our explicit
   start/stop maps onto the device open/release that the `Keepalive` drop and the
   pump-thread guard already implement. Who changes: us, adapting each capture
   backend to moq's channel and keepalive plumbing. Our one genuinely additive
   concept is the pre-encoded source (catalog C12, `LibcameraH264Source`,
   `rusty-capture/src/platform/linux/libcamera_h264.rs`): a source that yields
   encoded packets and bypasses the encoder, which needs a `publish_preencoded`
   sibling of `publish_capture` feeding `Producer::publish` directly. moq's
   `Producer::publish(Vec<Bytes>, Timestamp)` already accepts external Annex-B, so
   the concept fits; the change is a turnkey entry point plus buy-in that a source
   may bypass the encoder.

---

## Section 4: Would moq accept external codecs at all?

This is the open question the maintainer flagged, and it must be decided with
them rather than assumed. The evidence, then both paths, then a per-backend
recommendation.

### The evidence from moq's code and posture

moq vendors everything in-tree and links nothing that can fail to load. NVENC is
an in-tree fork of nvidia-video-codec-sdk (`rs/moq-nvenc`), fully dlopen'd so a
GPU-less builder links and a driverless host falls back
(`rs/moq-video/Cargo.toml:20-30`, and the driver-probe note there). openh264 is
built from vendored source and always compiled (`Cargo.toml:51-55`). moq-vaapi is
their own trimmed vendor of cros-libva plus cros-codecs. ffmpeg was removed
entirely and replaced with `yuv`, `fast_image_resize`, `zune-jpeg`, and `v4l`
(`Cargo.toml:44-102`). Dependencies are crates.io only, minimal, and justified
per line in the manifest. The `Backend` traits are deliberately `pub(crate)`
(`encode/backend/mod.rs:37`, `decode/backend/mod.rs:67`), the `Candidate` tables
are private `const` slices (`encode/backend/mod.rs:68-102`), and the public API
is documented as codec-agnostic with no backend type ever exposed
(`lib.rs:35-44`). This is a coherent, deliberate posture: control the whole codec
surface, keep it refactorable, and keep every build portable.

### Is there any existing seam for out-of-tree backends?

No. There is no public trait, no registration function, no plugin hook, and no
public frame vocabulary a would-be external backend could even produce or
consume. `Kind::Named(String)` (`encode/encoder.rs:45-47`) routes by name but
only among the built-in `const` candidates. An out-of-tree backend today is
impossible without source changes to moq. So external-codec support is not a
matter of using an existing seam; it requires three new things: the public frame
vocabulary of Section 1 (U1/C2), the public registerable `Backend` trait of
Section 2, and moq's willingness to treat those as stable semver surface.

### Would it conflict with the vendor-in-tree posture?

Partly and honestly, yes. A public `Backend` trait and a registration API are in
tension with "we vendor everything so we can reshape it freely": they invite an
ecosystem moq would not control and would freeze interfaces moq currently keeps
private on purpose. But the tension is bounded, because the trait traffics only
in public vocabulary types (Section 2, point 3), so it does not expose codec
internals, and because registration is additive to the existing table mechanism
rather than a redesign. The realistic maintainer position is that they will
accept backends they can carry and test in-tree far more readily than they will
open a public plugin surface, because the former costs them nothing structurally
and the latter costs them refactoring freedom permanently.

### The two paths, presented honestly

- **Path A: upstream our backends in-tree.** This is moq's preferred model. Each
  backend becomes a `const Candidate` in moq's tables behind a feature, vendored
  and feature-gated exactly like NVENC and VAAPI. We give up out-of-tree control
  and our own release cadence on that code, and we accept that moq reshapes it in
  review. It needs U1 (the frame vocabulary) and the PTS-through-encode change,
  but it does not need a public `Backend` trait or registration at all.

- **Path B: push moq to support external registered backends.** We keep our
  backends in our own tree, implementing moq's public `Backend` trait and calling
  `register_encoder`/`register_decoder`. We keep control and cadence, but we ask
  moq for the larger stability commitment of Section 2, and we carry the risk
  that moq declines it and we are left with backends that only work against a
  fork.

### Recommendation per backend

The decision is not uniform, and this is the agenda item to settle with the
maintainer:

- **VAAPI (encode and decode): in-tree, Path A.** It is moq's largest platform
  gap (no VAAPI decode at all, an unvalidated CPU-only VAAPI encoder), it belongs
  next to their other hardware backends, and we have Meteor Lake hardware to
  validate it. The dependency growth lands in their moq-vaapi vendor, which
  matches their posture exactly.

- **V4L2 M2M (encode and decode): in-tree, Path A.** It is the whole Raspberry Pi
  and ARM SoC class, moq has no V4L2 codec backend, and the stride and alignment
  handling is expensive platform knowledge that belongs where everyone benefits.
  The only friction is CI (no Pi in their runners), answered with a compile-only
  gate plus our hardware validation, the same story as NVENC's compile-everywhere
  stub.

- **AV1 (rav1e encode, rav1d decode): in-tree, Path A, once the dependency pin is
  resolved.** It completes moq's hardware-then-software story for AV1 (they have
  NVDEC-decode-only today), and software codecs are the natural in-tree case. The
  blocker is our rav1d git-fork pin, unacceptable upstream as-is; it must resolve
  to a crates.io release or a vendored safe wrapper first.

- **Android MediaCodec: the strongest case for Path B.** It is a whole platform
  moq cannot test in CI (no Android emulator hardware codecs), and it drags in an
  `ndk`/NDK dependency surface that is genuinely foreign to moq's desktop and
  server focus. This is the backend where "keep it in our tree over a public API"
  is the coherent outcome, and it is the concrete motivating user for the Section
  2 registration API. Propose Android in-tree first (it is a complete platform
  contributed whole), and fall back to Path B with Android as the named external
  backend if moq declines to carry code it cannot test.

- **Opus (encode and decode): in-tree, Path A, no external case.** Our
  improvements (runtime `set_bitrate`, lookahead-derived pre-skip, FEC/PLC
  groundwork) merge into moq-audio's existing in-tree Opus rather than standing up
  a separate backend, so there is no out-of-tree question to answer. Detail in
  [6-compare-audio.md](6-compare-audio.md) and Section 3 item 5.

- **PCM: offer in-tree as a simple `Codec::Pcm`, no external case, low value.** It
  slots into moq-audio's `#[non_exhaustive]` codec enum as an in-tree addition, but
  the hang catalog has no PCM variant, so interop value is nil and a decline is the
  expected outcome (keep it local for tests). Detail in
  [6-compare-audio.md](6-compare-audio.md) and Section 3 item 6.

- **The renderer: out-of-tree over public handles, always (Section 1b, Option
  B).** Not a backend, but the same shape of answer: it stays ours, consuming
  moq's public `native()` accessor, and its existence proves the vocabulary.

Net recommendation: pursue Path A for VAAPI, V4L2, and AV1, which need only U1
and the PTS change and nothing from Section 2's public trait; hold Android as the
one backend that justifies opening the Section 2 registration API, and only
escalate that ask if moq declines Android in-tree. Present Path B not as a demand
but as the fallback that a single untestable platform makes reasonable.

---

## Section 5: Sequenced change list

Each moq change, what it unblocks, its estimated moq-side diff size (S under
~150 LOC, M ~150 to 600, L ~600 to 1,500, XL above), whether it is breaking or
additive, its prerequisite ordering, and whether it is a prerequisite for the
in-tree path (Path A) or only for the external-backend path (Path B). Diff sizes
are moq-side authoring estimates excluding review reshaping.

| # | moq change | Unblocks | Size | Break/additive | Prereqs | Path |
|---|---|---|---|---|---|---|
| 1 | Public frame vocabulary `Native` + `frame.rs` `DmaBuf`/`HardwareBuffer` variants (`frame.rs:23-36`, new `moq-frame` crate or module) | C1, C5, C11, C13, and the render `native()` accessor | M-L (300 minimal, 700-1000 as a crate) | additive (variants cfg-gated; new public enum) | none; keystone | Both (needed for A and B) |
| 2 | PTS through `Backend::encode` (timestamp arg + `Packet` type, `encode/backend/mod.rs:40`, producer stamping) | V4L2 M2M, Android MediaCodec | S-M (~150, 7 files) | additive (private trait today; behavior unchanged) | none | Both |
| 3 | `decode::Frame::native()` accessor (`decode/mod.rs:94-101`) | C13 renderer, out-of-tree render (Option B) | S (~40) | additive | change 1 | Both |
| 4 | VAAPI decode backend exporting `Frame::DmaBuf` (`decode/backend/mod.rs` table; moq-vaapi export + VPP FFI) | Linux hardware decode, decode-to-render | L-XL (moq-vaapi ~1000-1400 + backend ~900) | additive (new candidate) | changes 1, 3; moq-vaapi growth | A (Path A backend) |
| 5 | VAAPI encoder GPU input + honest `set_bitrate` (`encode/backend/vaapi.rs`) | Linux capture-to-encode zero-copy | L (~800) | additive (backend-internal) | changes 1, 2 | A |
| 6 | VideoToolbox / Media Foundation decode retain surface (U2) | macOS + Windows decode-to-render | S-M (~120) | additive (backend-internal) | changes 1, 3 | A |
| 7 | Public + registerable `Backend` trait, `register_encoder`/`register_decoder`, `Kind` includes registered (`encode/backend/mod.rs:37,60-134`) | out-of-tree backends (Android) | M (~250, both sides) | breaking (trait goes public; semver commitment) | changes 1, 2 | B only |
| 8 | Render crate home (out-of-tree `moq-video-render` over public handles) | the whole render stack | 0 upstream (one paragraph in the U1 RFC) | n/a | changes 1, 3, 6 | Both |
| 9 | Timestamp/config/error unification (adopt `moq_net::Timestamp`, hang catalog, moq `Error`) | every contributed backend compiles in moq shape | S per backend (mechanical) | additive (new `Error` variants) | none | Both |
| 10 | Audio Opus knobs: `set_bitrate`, pre-skip, FEC/PLC reservation (moq-audio, moq-mux) | audio rate control, decoder delay correctness | S each (~200 total) | additive | none | independent |
| 11 | PCM codec offer (moq-audio `#[non_exhaustive]` enum) | PCM interop (low value) | S (~350) | additive | hang catalog PCM variant (likely declined) | independent |
| 12 | Pre-encoded source + `publish_preencoded` (C12, `capture` + `encode`) | Pi Zero on-device H.264 | M (~500) | additive | concept buy-in | independent |

Ordering: change 1 is the keystone and goes first or in the same RFC as the VAAPI
decode backend that motivates it. Changes 2, 3, and 9 are small, additive, and
have no cross-dependencies, so they can land alongside change 1. The VAAPI series
(4, 5) gates on moq-vaapi growing surface export and VPP, the single largest and
most resistance-prone piece. Change 7 is the only breaking change and the only
one exclusive to Path B, so it is deferred until the Android decision forces it.
Changes 10, 11, and 12 are independent of the frame-model work and can proceed in
any order as relationship-building or opportunistic PRs.

The top prerequisites, in one sentence: the public frame vocabulary (change 1) is
the keystone every GPU contribution rests on, the PTS-through-encode change
(change 2) is the small unconditional unblock for pipelined backends, and the
public registerable `Backend` trait (change 7) is the one larger, breaking ask
that is needed only if Android or any other backend stays in our tree.
