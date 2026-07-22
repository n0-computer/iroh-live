# v4l2-decode. V4L2 M2M hardware H.264 decode for Raspberry Pi and embedded ARM

Branch: moq-upstream/v4l2-decode          PR target: base branch, then moq main
Depends on: B1 (frame vocabulary), B3 (decode native accessor)
Path: A (in-tree)
Size: M-L

## Goal

Give moq-video a hardware H.264 decoder for the Raspberry Pi and embedded ARM SoC
class. moq's Linux decode story today is NVDEC plus the openh264 software
fallback (`comparisons/maps/moq-video.md:348-352`), so every non-NVIDIA ARM board
decodes in software. The backend drives the kernel V4L2 M2M stateful decoder
through the `v4l2r` crate on a dedicated OS thread and adds one `Candidate` to the
decode table behind the `v4l2` feature. The first PR delivers the simple and
already-validated CPU path (MMAP output downloaded to I420); a documented
follow-up adds the EXPBUF zero-copy path that exports the decoded surface as
`Frame::DmaBuf`, which is why the plan is filed against B1 and B3 even though the
CPU path needs neither.

## Evidence

- iroh-live source: `comparisons/maps/rusty-codecs.md:235` (the V4L2 row: decoder
  on `v4l2r` 0.0.7, tested on Pi bcm2835-codec, `/dev/video10`), and
  `comparisons/codecs.md:60-64, 194-195` (the decode-backends grid showing V4L2 as
  ours-only, and the "decoder uses the `v4l2r` crate" note).
- moq target shape: `comparisons/maps/moq-video.md:308-352` (the decode `Backend`
  trait, the `Decoded { timestamp, frame }` type, the per-candidate `supports`
  predicate, and `open(codec, config)` dispatch),
  `comparisons/maps/moq-video.md:386-407` (the public `decode::Frame`).
- The B1 frame vocabulary and B3 accessor this depends on for the zero-copy
  follow-up: `0-overview.md:54-130` and `comparisons/moq-changes.md:206-243`
  (decoders export a handle, the `native()` accessor, the VAAPI decode reference
  including the `vaSyncSurface`-before-export and per-frame export caching notes).

## moq API consumed

- The decode `Backend` trait (`rs/moq-video/src/decode/backend/mod.rs:36-75`):
  `decode(&mut self, access_unit: Bytes, timestamp: Timestamp, keyframe: bool) ->
  Result<Vec<Decoded>, Error>` and `name`. Output is `Decoded { timestamp:
  Timestamp, frame: crate::frame::Frame }`.
- The decode registration shape is a `Candidate` carrying a `supports: fn(Codec)
  -> bool` predicate and `open: fn(Codec, &Config) -> Result<Box<dyn Backend>,
  Error>` (`rs/moq-video/src/decode/backend/mod.rs:80-85`); this backend supports
  only `Codec::H264`.
- CPU path: constructs `Frame::I420` (`rs/moq-video/src/frame.rs:79-85`) from the
  decoder's strided NV12 output. moq's `I420::from_nv12` (`frame.rs:208-225`)
  cannot be reused as-is: it is gated `#[cfg(target_os = "windows")]`
  (`frame.rs:210`) so it does not compile on the Linux target this backend runs
  on, and its signature `from_nv12(nv12: &[u8], width, height)` assumes a
  tightly-packed NV12 buffer (`luma = w*h`) with no stride, so it cannot honor the
  per-plane stride the V4L2 `DqBuffer` reports. The backend must supply its own
  stride-aware NV12-to-I420 pack (port the `copy_plane`/`interleave_uv` logic from
  our `extract_decoded_frame`, `decoder.rs:356-411`), or the leaf must de-gate and
  extend moq's `from_nv12` to take strides; see the adaptation notes.
- Zero-copy follow-up: the B1 `Frame::DmaBuf(dmabuf::Frame)` variant under
  `#[cfg(all(target_os = "linux", feature = "dmabuf"))]` and the B3
  `decode::Frame::native() -> Option<Native>` accessor, so the exported EXPBUF
  descriptor reaches a renderer without a CPU download. The EXPBUF follow-up would
  add `v4l2` to the set of features that enable `dmabuf` (B1 documents `v4l2` as a
  future `dmabuf` producer for exactly this path).
- `moq_net::Timestamp` and moq's `Error` with an additive device-failure variant.

## Source to port

Primary: `rusty-codecs/src/codec/v4l2/decoder.rs` (521 LOC), verified against the
working tree:

- The `VideoDecoder` impl (`decoder.rs:50`) and `new` (`decoder.rs:55`) construct
  the decoder and hand the whole V4L2 lifecycle to a dedicated thread, "to contain
  v4l2r's unnameable type-state generics" (`decoder.rs:22-24`). All `v4l2r`
  generics stay local to `decoder_thread` (`decoder.rs:162`; `:161` is its doc
  comment). There is no `run_decoder` symbol.
- It uses `v4l2r`'s high-level stateful decoder API (`decoder.rs:169-172`) with
  `MemoryType::Mmap` and an `MmapProvider` for the CAPTURE queue
  (`decoder.rs:171, 261`). Decoded frames are downloaded from MMAP buffers.
- Output is CPU NV12: the frame extraction path builds `VideoFrame::new_nv12(...)`
  from the dequeued CAPTURE buffer's planes (`decoder.rs:394` and
  `decoder.rs:421`), honoring the per-plane stride. The stride comes from
  `FormatState.stride` (read at `decoder.rs:360`), populated in the
  format-changed callback (`decoder.rs:252`), not from the `DqBuffer` type
  parameter that appears in the extraction function's signature
  (`decoder.rs:352-354`).
- Device path selection: `decoder_device_path()`
  (`rusty-codecs/src/codec/v4l2.rs:68-74`) checks `V4L2_DEC_DEVICE` then falls
  back to `/dev/video10`.

There is no EXPBUF or DMA-BUF export anywhere in the V4L2 tree today: a grep for
`EXPBUF`, `DmaBuf`, `DMABUF`, and `dma_buf` across `src/codec/v4l2/` returns
nothing. The encoder's `raw_v4l2` submodule (`encoder.rs:316`) is `QBUF`/`DQBUF`/
`S_FMT`/`STREAMON` encode FFI with no `VIDIOC_EXPBUF` and no export machinery, and
the decoder is CPU MMAP end to end. The zero-copy decode path is therefore new
work, not a port; it starts from nothing.

Carried over: the dedicated-thread model that contains the v4l2r generics, the
stateful decoder setup, the strided-plane extraction, and the device-path
selection.

Dropped or replaced: the `VideoDecoder` trait glue (`decoder.rs:50`) becomes
moq's decode `Backend`; the `VideoConfig`/`DecodeConfig` construction inputs
become the hang catalog `VideoConfig` and moq's decode `Config`; `Duration`
becomes `moq_net::Timestamp`; `anyhow::Result` becomes moq's `Error`; and our
`set_viewport`/`burst_size` decoder-trait extensions are not carried, since moq's
`Vec<Decoded>` return already exposes DPB flush bursts as a longer Vec
(`comparisons/traits-api.md:658-666`) and moq handles resize through
`Config::resize`.

## Target in moq

- New file `rs/moq-video/src/decode/backend/v4l2.rs` holding the backend struct,
  its `Backend` impl, `pub const NAME: &str = "v4l2"`, and a `supports` predicate
  returning true only for `Codec::H264`, cfg-gated `#[cfg(all(target_os =
  "linux", feature = "v4l2"))]`.
- One additive `Candidate` in the decode `HARDWARE` slice of
  the HARDWARE candidate slice `rs/moq-video/src/decode/backend/mod.rs:89-107`,
  alongside the NVDEC entry (the single `const SOFTWARE` sits at `:110-114`):

  ```rust
  #[cfg(all(target_os = "linux", feature = "v4l2"))]
  Candidate { name: v4l2::NAME, supports: v4l2::supports, open: v4l2::open },
  ```

- The `v4l2` feature in `rs/moq-video/Cargo.toml` (shared with the v4l2-encode
  leaf), which pulls in the `v4l2r` crate for the decode half. Confirm `v4l2r`
  0.0.7 is on crates.io with no git pin, per the minimal-dependencies convention.

## Implementation steps

The build order deliberately ships the CPU path first, then a zero-copy
follow-up.

1. Add the `v4l2` feature (or extend it if v4l2-encode landed first to add
   `v4l2r`), the cfg-gated `mod v4l2;`, and the decode `HARDWARE` candidate entry.
   Verify the crate builds with the feature on and off, on Linux and off Linux.
2. Port the dedicated-thread stateful decoder from `decoder_thread`
   (`decoder.rs:162`), keeping every `v4l2r` type-state generic local to the
   thread as our code does. Open the node
   from `decoder_device_path()` semantics (env override then `/dev/video10`); on
   open or negotiation failure return a moq `Error` so `Kind::Auto` falls through.
3. Implement `Backend::decode(access_unit, timestamp, keyframe)`: feed the
   Annex-B access unit to the OUTPUT queue, drain ready CAPTURE buffers, and for
   each produce a `Decoded { timestamp, frame: Frame::I420(...) }`. moq's front
   end already converts avc1/hvc1 to Annex-B and injects parameter sets ahead of
   keyframes (`comparisons/traits-api.md:283`), so the backend receives in-band
   Annex-B and does not repeat that work. Return zero frames while the decoder is
   still filling its DPB and a burst when it flushes.
4. Thread the timestamp: the decode `Backend` already carries a per-access-unit
   `Timestamp` and returns it per `Decoded`. For a one-in one-out stateful decoder
   the timestamp echoes the input; carry it straight through, matching openh264's
   backend (`comparisons/maps/moq-video.md:437-439`).
5. Convert the decoder's strided NV12 output to a tightly packed `Frame::I420`.
   moq's `I420::from_nv12` is Windows-gated and stride-less (see "moq API
   consumed"), so pack it with an own stride-aware NV12-to-I420 routine ported
   from `extract_decoded_frame` (`decoder.rs:356-411`), reading the per-plane
   stride from `FormatState.stride`. This is the CPU path and the first shippable
   PR.
6. Follow-up, zero-copy EXPBUF: switch the CAPTURE queue to `MemoryType::DmaBuf`
   or export MMAP buffers with `VIDIOC_EXPBUF`, wrap each exported descriptor as
   the B1 `dmabuf::Frame` (fourcc, modifier, coded and display size, per-plane
   offset and pitch, and a lazy fd exporter that mints an `OwnedFd` on demand
   rather than holding one per buffered frame, mirroring the B1 design), and
   return `Decoded { timestamp, frame: Frame::DmaBuf(...) }`. The B3 `native()`
   accessor then hands the descriptor to a renderer with no CPU round trip.

## Tests

- A hardware round-trip test modeled on moq's own `round_trip(encoder, decoder,
  w, h)` helper (`rs/moq-video/src/decode/backend/nvdec.rs:513`), which encodes a
  gradient with openh264 and decodes it through the hardware backend: feed a short
  H.264 Annex-B sequence produced by openh264 (no ffmpeg, per the conventions)
  into the V4L2 decoder, assert one decoded I420 frame per input access unit after
  the first keyframe, that timestamps are preserved, and that the decoded luma
  matches the source within a tolerance. Follow nvdec's gating rather than
  `#[ignore]`: put the test in a `feature = "v4l2"`-gated module and skip at
  runtime with an `hw_available()`-style guard (`nvdec.rs:465`) when no V4L2 M2M
  decoder node opens.
- For the follow-up: assert `decode::Frame::native()` returns `Some(Native::
  DmaBuf(_))` on the zero-copy path and that `into_i420()` still yields a correct
  CPU download from the exported descriptor.

## Adaptation notes

- CPU MMAP first, EXPBUF zero-copy as a follow-up, with the recommendation stated
  plainly: ship the CPU MMAP-to-I420 path first because it is exactly what our
  decoder already does and is validated on bcm2835-codec, and it needs neither B1
  nor B3. The EXPBUF zero-copy path is genuinely new code (our decoder has no
  EXPBUF today), and bcm2835-codec's exported buffers carry a DRM format modifier
  whose import into a wgpu or Vulkan renderer is unvalidated on Pi hardware, so
  the zero-copy path carries real integration risk that should not block the CPU
  decoder from landing. Ship CPU first, then add EXPBUF behind the same feature
  once the B1 `Frame::DmaBuf` variant and the B3 `native()` accessor exist and a
  renderer can consume the descriptor. This is the same layering the VAAPI decode
  leaf follows, and it keeps the two Linux decode backends aligned.
- Why B1 and B3 are listed as dependencies even though step 5 needs neither: the
  plan's end state is a decode candidate that can feed the renderer, which is the
  reason a decode backend is worth more than a software fallback. That end state
  is the EXPBUF path, and it needs the B1 `Frame::DmaBuf` variant and the B3
  `native()` accessor. The CPU-only intermediate is a strict subset. State in the
  PR that the first commit is B1/B3-independent so it can merge ahead of the base
  if scheduling requires, with the zero-copy commit gated on the base landing.
- v4l2r versus raw ioctl: the decoder uses `v4l2r`'s high-level stateful API,
  which handles the stateful-decoder state machine (sequence and resolution-change
  events, DPB management) that would be painful to reimplement on raw ioctls. The
  encoder uses raw ioctls instead, for the control-ordering reasons in the
  v4l2-encode plan. This asymmetry is intentional; note it in the PR.
- moq's decode front end owns first-keyframe gating and parameter-set injection
  (`comparisons/traits-api.md:284`), so the backend must not re-gate on keyframes
  or re-inject SPS/PPS; it decodes what it is handed.
- Timestamps, errors, and config follow base plan B5. No ffmpeg, including in the
  test's bitstream generation, which uses openh264.

## Coordination

- Coordination point 2 (shared candidate tables): add only this backend's decode
  `Candidate`; do not refactor the table. Rebase behind any decode leaf that
  merges first (notably vaapi-decode, which also adds a Linux decode candidate).
- Coordination point 1 (base API freeze): code the zero-copy follow-up against the
  frozen B1 `Native`/`DmaBuf` vocabulary and B3 `native()`. If the EXPBUF
  descriptor cannot be expressed by the B1 `DmaBuf` fields (for example if a Pi
  modifier needs a field B1 does not carry), stop and file the gap against base
  plan B1 rather than extending the vocabulary in this leaf.
- Feature sharing with v4l2-encode: both leaves add a `v4l2` feature. Whichever
  lands first introduces the feature; the second extends it (encode needs no extra
  crate, decode adds `v4l2r`). Coordinate the Cargo.toml edit so the two do not
  conflict beyond a rebase.

## Acceptance checklist

- The crate builds with `--features v4l2` and without it, on Linux and off Linux;
  `v4l2r` is a crates.io dependency with no git pin.
- The CPU path produces one correct `Frame::I420` per input access unit after the
  first keyframe, with timestamps preserved, verified on hardware where available
  and by the ignored round-trip test otherwise.
- The decode candidate is added additively with a `supports` predicate limited to
  `Codec::H264`; no other table entry is touched.
- The follow-up EXPBUF commit produces `Frame::DmaBuf` and returns it through
  `native()`, with `into_i420()` still correct as the fallback, and is clearly
  separated from the CPU commit so the CPU decoder can land independently.
- The PR states the v4l2r-versus-raw-ioctl asymmetry, the CPU-first ordering, and
  the missing-hardware test gating.
