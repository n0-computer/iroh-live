# V4L2 M2M H.264 decode

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: partial. The shipped CPU path packs the decoder's strided NV12 output
> into I420 on the CPU; the EXPBUF DMA-BUF export that makes decode-to-render
> zero-copy is a scoped follow-up on the B1 frame vocabulary. See the short section
> below and ../zerocopy.md, section 2b "Decode to render".

## What it is

A hardware H.264 decoder for the Raspberry Pi and embedded ARM SoC class, driving
the kernel V4L2 M2M stateful decoder through the `v4l2r` crate on a dedicated OS
thread. moq's Linux decode story today is NVDEC plus the openh264 software
fallback (../comparison/maps/moq-video.md:348-352), so every non-NVIDIA ARM board
decodes in software. The verdict is move-to-moq because moq has no V4L2 decode of
any kind. The first PR delivers the simple and already-validated CPU path (MMAP
output downloaded to I420); a documented follow-up adds the EXPBUF zero-copy path
that exports the decoded surface as `Frame::DmaBuf`, which is why the plan is filed
against B1 and B3 even though the CPU path needs neither.

## iroh-live vs moq

moq has no counterpart: the decode-backends grid shows V4L2 M2M as ours-only
(../comparison/codecs.md, section 1 decode-backends table, and the "decoder uses
the `v4l2r` crate" note at line 197). Ours is
`rusty-codecs/src/codec/v4l2/decoder.rs` (521 LOC), which uses `v4l2r`'s high-level
stateful decoder API on a dedicated thread and produces CPU NV12 honoring the
per-plane stride. moq's decode `Backend` is stronger at the selection and lifecycle
level (its `Vec<Decoded>` return already exposes DPB flush bursts as a longer Vec,
and it handles resize through `Config::resize`), so the port sheds our
`set_viewport`/`burst_size` decoder-trait extensions. The port keeps our
dedicated-thread model that contains the v4l2r generics and our strided-plane
extraction, and adds moq's per-picture `Decoded { timestamp, frame }` output.

## Zero-copy

The shipped CPU path is not zero-copy: it downloads MMAP CAPTURE buffers and packs
the strided NV12 into a tightly packed `Frame::I420`. There is no EXPBUF or DMA-BUF
export anywhere in the V4L2 tree today: a grep for `EXPBUF`, `DmaBuf`, `DMABUF`, and
`dma_buf` across `src/codec/v4l2/` returns nothing. The encoder's `raw_v4l2`
submodule (`encoder.rs:316`) is `QBUF`/`DQBUF`/`S_FMT`/`STREAMON` encode FFI with no
`VIDIOC_EXPBUF` and no export machinery, and the decoder is CPU MMAP end to end. The
zero-copy decode path is therefore new work, not a port; it starts from nothing.

The follow-up switches the CAPTURE queue to `MemoryType::DmaBuf` or exports MMAP
buffers with `VIDIOC_EXPBUF`, wraps each exported descriptor as the B1
`dmabuf::Frame` (fourcc, modifier, coded and display size, per-plane offset and
pitch, and a lazy fd exporter that mints an `OwnedFd` on demand rather than holding
one per buffered frame, mirroring the B1 design), and returns
`Decoded { timestamp, frame: Frame::DmaBuf(...) }`. The B3 `native()` accessor then
hands the descriptor to a renderer with no CPU round trip. This is the same layering
the VAAPI decode leaf follows (see codec-vaapi-decode.md), keeping the two Linux
decode backends aligned. The EXPBUF follow-up adds `v4l2` to the set of features
that enable `dmabuf`; B1 documents `v4l2` as a future `dmabuf` producer for exactly
this path (../base/B1-frame-vocabulary.md, feature-design note). Real integration
risk exists: bcm2835-codec's exported buffers carry a DRM format modifier whose
import into a wgpu or Vulkan renderer is unvalidated on Pi hardware, so the
zero-copy path must not block the CPU decoder from landing. See ../zerocopy.md,
section 2b "Decode to render".

## What to do

MOVE. Target crate: moq-video.

New file `rs/moq-video/src/decode/backend/v4l2.rs` holds the backend struct, its
`Backend` impl, `pub const NAME: &str = "v4l2"`, and a `supports` predicate
returning true only for `Codec::H264`, cfg-gated `#[cfg(all(target_os = "linux",
feature = "v4l2"))]`. One additive `Candidate` in the decode `HARDWARE` slice at
`rs/moq-video/src/decode/backend/mod.rs:89-107`, alongside the NVDEC entry (the
single `const SOFTWARE` sits at `:110-114`):

```rust
#[cfg(all(target_os = "linux", feature = "v4l2"))]
Candidate { name: v4l2::NAME, supports: v4l2::supports, open: v4l2::open },
```

The `v4l2` feature in `rs/moq-video/Cargo.toml` is shared with the v4l2-encode leaf
and pulls in the `v4l2r` crate for the decode half. Confirm `v4l2r` 0.0.7 is on
crates.io with no git pin, per the minimal-dependencies convention.

moq API consumed:

- The decode `Backend` trait (`rs/moq-video/src/decode/backend/mod.rs:36-75`):
  `decode(&mut self, access_unit: Bytes, timestamp: Timestamp, keyframe: bool) ->
  Result<Vec<Decoded>, Error>` and `name`. Output is `Decoded { timestamp:
  Timestamp, frame: crate::frame::Frame }`.
- The decode registration shape is a `Candidate` carrying a `supports: fn(Codec) ->
  bool` predicate and `open: fn(Codec, &Config) -> Result<Box<dyn Backend>, Error>`
  (`rs/moq-video/src/decode/backend/mod.rs:80-85`); this backend supports only
  `Codec::H264`.
- CPU path: constructs `Frame::I420` (`rs/moq-video/src/frame.rs:79-85`) from the
  decoder's strided NV12 output. moq's `I420::from_nv12` (`frame.rs:208-225`) cannot
  be reused as-is: it is gated `#[cfg(target_os = "windows")]` (`frame.rs:210`) so it
  does not compile on the Linux target this backend runs on, and its signature
  `from_nv12(nv12: &[u8], width, height)` assumes a tightly packed NV12 buffer
  (`luma = w*h`) with no stride, so it cannot honor the per-plane stride the V4L2
  `DqBuffer` reports. The backend must supply its own stride-aware NV12-to-I420 pack
  (port the `copy_plane`/`interleave_uv` logic from our `extract_decoded_frame`,
  `decoder.rs:356-411`), or the leaf must de-gate and extend moq's `from_nv12` to
  take strides.
- Zero-copy follow-up: the B1 `Frame::DmaBuf(dmabuf::Frame)` variant under
  `#[cfg(all(target_os = "linux", feature = "dmabuf"))]` and the B3
  `decode::Frame::native() -> Option<Native>` accessor.
- `moq_net::Timestamp` and moq's `Error` with an additive device-failure variant.

Source to port, primary `rusty-codecs/src/codec/v4l2/decoder.rs` (521 LOC), verified
against the working tree:

- The `VideoDecoder` impl (`decoder.rs:50`) and `new` (`decoder.rs:55`) construct the
  decoder and hand the whole V4L2 lifecycle to a dedicated thread, "to contain
  v4l2r's unnameable type-state generics" (`decoder.rs:22-24`). All `v4l2r` generics
  stay local to `decoder_thread` (`decoder.rs:162`; `:161` is its doc comment). There
  is no `run_decoder` symbol.
- It uses `v4l2r`'s high-level stateful decoder API (`decoder.rs:169-172`) with
  `MemoryType::Mmap` and an `MmapProvider` for the CAPTURE queue (`decoder.rs:171,
  261`). Decoded frames are downloaded from MMAP buffers.
- Output is CPU NV12: the frame extraction path builds `VideoFrame::new_nv12(...)`
  from the dequeued CAPTURE buffer's planes (`decoder.rs:394` and `decoder.rs:421`),
  honoring the per-plane stride. The stride comes from `FormatState.stride` (read at
  `decoder.rs:360`), populated in the format-changed callback (`decoder.rs:252`), not
  from the `DqBuffer` type parameter that appears in the extraction function's
  signature (`decoder.rs:352-354`).
- Device path selection: `decoder_device_path()`
  (`rusty-codecs/src/codec/v4l2.rs:68-74`) checks `V4L2_DEC_DEVICE` then falls back to
  `/dev/video10`.

Carried over: the dedicated-thread model that contains the v4l2r generics, the
stateful decoder setup, the strided-plane extraction, and the device-path selection.
Dropped or replaced: the `VideoDecoder` trait glue (`decoder.rs:50`) becomes moq's
decode `Backend`; the `VideoConfig`/`DecodeConfig` construction inputs become the
hang catalog `VideoConfig` and moq's decode `Config`; `Duration` becomes
`moq_net::Timestamp`; `anyhow::Result` becomes moq's `Error`; and our
`set_viewport`/`burst_size` decoder-trait extensions are not carried, since moq's
`Vec<Decoded>` return already exposes DPB flush bursts as a longer Vec
(../comparison/traits-api.md:658-666) and moq handles resize through
`Config::resize`.

Ordered implementation steps (the build order deliberately ships the CPU path first,
then a zero-copy follow-up):

1. Add the `v4l2` feature (or extend it if v4l2-encode landed first to add `v4l2r`),
   the cfg-gated `mod v4l2;`, and the decode `HARDWARE` candidate entry. Verify the
   crate builds with the feature on and off, on Linux and off Linux.
2. Port the dedicated-thread stateful decoder from `decoder_thread`
   (`decoder.rs:162`), keeping every `v4l2r` type-state generic local to the thread
   as our code does. Open the node from `decoder_device_path()` semantics (env
   override then `/dev/video10`); on open or negotiation failure return a moq `Error`
   so `Kind::Auto` falls through.
3. Implement `Backend::decode(access_unit, timestamp, keyframe)`: feed the Annex-B
   access unit to the OUTPUT queue, drain ready CAPTURE buffers, and for each produce
   a `Decoded { timestamp, frame: Frame::I420(...) }`. moq's front end already
   converts avc1/hvc1 to Annex-B and injects parameter sets ahead of keyframes
   (../comparison/traits-api.md:283), so the backend receives in-band Annex-B and does
   not repeat that work. Return zero frames while the decoder is still filling its DPB
   and a burst when it flushes.
4. Thread the timestamp: the decode `Backend` already carries a per-access-unit
   `Timestamp` and returns it per `Decoded`. For a one-in one-out stateful decoder the
   timestamp echoes the input; carry it straight through, matching openh264's backend
   (../comparison/maps/moq-video.md:437-439).
5. Convert the decoder's strided NV12 output to a tightly packed `Frame::I420`. moq's
   `I420::from_nv12` is Windows-gated and stride-less (see moq API consumed), so pack
   it with an own stride-aware NV12-to-I420 routine ported from `extract_decoded_frame`
   (`decoder.rs:356-411`), reading the per-plane stride from `FormatState.stride`. This
   is the CPU path and the first shippable PR.
6. Follow-up, zero-copy EXPBUF: switch the CAPTURE queue to `MemoryType::DmaBuf` or
   export MMAP buffers with `VIDIOC_EXPBUF`, wrap each exported descriptor as the B1
   `dmabuf::Frame`, and return `Decoded { timestamp, frame: Frame::DmaBuf(...) }`. The
   B3 `native()` accessor then hands the descriptor to a renderer with no CPU round
   trip.

Adaptation notes:

- CPU MMAP first, EXPBUF zero-copy as a follow-up: ship the CPU MMAP-to-I420 path
  first because it is exactly what our decoder already does and is validated on
  bcm2835-codec, and it needs neither B1 nor B3. The EXPBUF zero-copy path is
  genuinely new code, and bcm2835-codec's exported buffers carry a DRM format modifier
  whose import is unvalidated on Pi hardware, so the zero-copy path carries real
  integration risk that should not block the CPU decoder from landing. State in the PR
  that the first commit is B1/B3-independent so it can merge ahead of the base if
  scheduling requires, with the zero-copy commit gated on the base landing.
- v4l2r versus raw ioctl: the decoder uses `v4l2r`'s high-level stateful API, which
  handles the stateful-decoder state machine (sequence and resolution-change events,
  DPB management) that would be painful to reimplement on raw ioctls. The encoder uses
  raw ioctls instead, for the control-ordering reasons in codec-v4l2-encode.md. This
  asymmetry is intentional; note it in the PR.
- moq's decode front end owns first-keyframe gating and parameter-set injection
  (../comparison/traits-api.md:284), so the backend must not re-gate on keyframes or
  re-inject SPS/PPS; it decodes what it is handed.
- Timestamps, errors, and config follow ../base/B5-adaptation-conventions.md. No
  ffmpeg, including in the test's bitstream generation, which uses openh264.

iroh-live removal side: the local module `rusty-codecs/src/codec/v4l2/`
(encode+decode, 1,856 LOC combined) is deleted only after the matching moq leaves
merge and release, on the `up/v4l2-decode` pair branch, gated by the atomic
per-platform rule (Linux non-NVIDIA flips only once the VAAPI and V4L2 leaves have
all released) and, for the decode deletion, by render-adopt and the B1 frame
vocabulary so the decode-to-render path survives. See codec-remove in
../comparison/iroh-live-code-map.md and the disposition register.

## Tests

- A hardware round-trip test modeled on moq's own `round_trip(encoder, decoder, w, h)`
  helper (`rs/moq-video/src/decode/backend/nvdec.rs:513`), which encodes a gradient
  with openh264 and decodes it through the hardware backend: feed a short H.264
  Annex-B sequence produced by openh264 (no ffmpeg, per the conventions) into the V4L2
  decoder, assert one decoded I420 frame per input access unit after the first
  keyframe, that timestamps are preserved, and that the decoded luma matches the source
  within a tolerance. Follow nvdec's gating rather than `#[ignore]`: put the test in a
  `feature = "v4l2"`-gated module and skip at runtime with an `hw_available()`-style
  guard (`nvdec.rs:465`) when no V4L2 M2M decoder node opens.
- For the follow-up: assert `decode::Frame::native()` returns `Some(Native::DmaBuf(_))`
  on the zero-copy path and that `into_i420()` still yields a correct CPU download from
  the exported descriptor.

CI cannot test this: moq CI has no Raspberry Pi or embedded ARM board, so the
compile-only gate plus our hardware validation is the story, matching the encode leaf.

## Evidence

- iroh-live source: ../comparison/maps/rusty-codecs.md:237 (the V4L2 row: decoder on
  `v4l2r` 0.0.7, tested on Pi bcm2835-codec, `/dev/video10`), and
  ../comparison/codecs.md, section 1 decode-backends table and the "decoder uses the
  `v4l2r` crate" note (lines 60-64, 194-195).
- moq target shape: ../comparison/maps/moq-video.md:308-352 (the decode `Backend`
  trait, the `Decoded { timestamp, frame }` type, the per-candidate `supports`
  predicate, and `open(codec, config)` dispatch), ../comparison/maps/moq-video.md:386-407
  (the public `decode::Frame`).
- The B1 frame vocabulary and B3 accessor this depends on for the zero-copy follow-up:
  ../base/B1-frame-vocabulary.md and ../comparison/moq-changes.md:206-243 (decoders
  export a handle, the `native()` accessor, the VAAPI decode reference including the
  `vaSyncSurface`-before-export and per-frame export caching notes).
- Removal ledger: codec-remove, the V4L2 row (1,856 LOC, upstream-ours; pair branches
  `up/v4l2-encode`, `up/v4l2-decode`), and the disposition register V4L2 row (the
  driver stride and alignment handling travels with the port).

## Coordination

- Base plans needed: ../base/B1-frame-vocabulary.md (the `Frame::DmaBuf` variant) and
  ../base/B3-decode-native-accessor.md (the `native()` accessor). Why B1 and B3 are
  listed as dependencies even though step 5 needs neither: the plan's end state is a
  decode candidate that can feed the renderer, which is the reason a decode backend is
  worth more than a software fallback. That end state is the EXPBUF path, and it needs
  the B1 `Frame::DmaBuf` variant and the B3 `native()` accessor. The CPU-only
  intermediate is a strict subset.
- Coordination point 2 (shared candidate tables): add only this backend's decode
  `Candidate`; do not refactor the table. Rebase behind any decode leaf that merges
  first (notably vaapi-decode, which also adds a Linux decode candidate; see
  codec-vaapi-decode.md).
- Coordination point 1 (base API freeze): code the zero-copy follow-up against the
  frozen B1 `Native`/`DmaBuf` vocabulary and B3 `native()`. If the EXPBUF descriptor
  cannot be expressed by the B1 `DmaBuf` fields (for example if a Pi modifier needs a
  field B1 does not carry), stop and file the gap against B1 rather than extending the
  vocabulary in this leaf.
- Feature sharing with codec-v4l2-encode.md: both leaves add a `v4l2` feature.
  Whichever lands first introduces the feature; the second extends it (encode needs no
  extra crate, decode adds `v4l2r`). Coordinate the Cargo.toml edit so the two do not
  conflict beyond a rebase.
- CI hardware gating: not testable in moq CI (no Pi or embedded board); compile-only
  gate plus our hardware validation.
- Release gate: the iroh-live-side deletion of `codec/v4l2/` waits for the leaves to
  reach a pinned moq release and travels with the version bump in a single revertible
  commit on `up/v4l2-decode`, after render-adopt has re-homed the decode-to-render
  path, per the upstream gating and zero-copy rules.
