# Android MediaCodec H.264 encode and decode

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: yes. The ImageReader decoder keeps decoded frames as `AHardwareBuffer`s
> all the way to a GLES or Vulkan importer, feeding the B1 `HardwareBuffer` frame
> variant; it is the Android analog of the VAAPI DMA-BUF and VideoToolbox
> CVPixelBuffer decode-to-render paths. See the section below and ../zerocopy.md,
> section 2b "Decode to render".

## What it is

A whole platform moq has no support for. moq-video has zero Android code in any
crate (../comparison/codecs.md:259, 270-274). This module contributes an Android
MediaCodec H.264 encoder plus two decoders (a ByteBuffer CPU decoder and a
zero-copy ImageReader decoder that keeps decoded frames as `AHardwareBuffer`s),
driving the NDK `AMediaCodec` API. The verdict is move-to-moq because moq lacks the
platform entirely. Android is the Path B motivator of the whole campaign: moq
cannot build or test NDK code in its CI (no Android emulator with hardware codecs),
so the primary plan targets the external registration path, where the code stays in
our tree and plugs in through `register_encoder`/`register_decoder` rather than an
in-tree `const Candidate`. An in-tree fallback is written up but gated on the open
placement question.

## iroh-live vs moq

moq has no Android backend of any kind (../comparison/codecs.md, section 1 "Android
MediaCodec (ours only)"). Ours is `rusty-codecs/src/codec/android/` (1,528 LOC): an
encoder that configures NV12 MediaCodec sessions with an error-reset counter and an
honest-but-weak `set_bitrate`, and two decoders selected by the dynamic dispatcher.
moq's Backend traits are stronger on the contract points every backend must adopt
(the honest `set_bitrate` and per-frame forced IDR the B2 encode contract requires),
and moq's candidate/registration model replaces our `codec/dynamic.rs` dispatch.
Ours brings the standout capability: the zero-copy ImageReader decoder producing
`HardwareBuffer` frames, which moq has no analog for. The port keeps the MediaCodec
session setup, the NV12 conversion, both decoders, and the `AHardwareBuffer`
wrapping, and adapts them to moq's public B4 traits and B1 frame vocabulary.

## Zero-copy

The ImageReader decoder is the zero-copy path. `hw_decoder.rs` (325 LOC) uses an
`ImageReader` surface as the MediaCodec output target (`hw_decoder.rs:1-6, 67, 72`);
decoded frames stay in GPU memory. The output buffer is released with `render=true`
to route it through the ImageReader surface (the
`release_output_buffer(output_buf, true)` call at `hw_decoder.rs:249`; the comment is
at `hw_decoder.rs:247`), then the latest image is acquired as a HardwareBuffer-backed
`AndroidGpuFrame` and wrapped as a `GpuFrame` (`hw_decoder.rs:254-270`). The
`ImageReader` is created with `GPU_SAMPLED_IMAGE | CPU_READ_OFTEN` usage so a
consumer can import the buffer (`hw_decoder.rs:107-111`). `gpu_frame.rs` (209 LOC)
holds `AndroidGpuFrame`, the `GpuFrameInner` impl over a locked `AHardwareBuffer`
(`gpu_frame.rs:47, 95`), with `download_rgba` and `download_nv12` CPU fallbacks
(`gpu_frame.rs:96, 153`) and `native_handle() ->
Some(NativeFrameHandle::HardwareBuffer(HardwareBufferInfo { ... }))` acquiring an
extra reference so the buffer outlives the Image (`gpu_frame.rs:195-200`).

The port produces `Decoded { timestamp, frame: Frame::HardwareBuffer(...) }`,
wrapping the acquired `AHardwareBuffer` as moq's B1 `android::HwBuffer` and acquiring
the extra reference exactly as `gpu_frame.rs:195-200` does. Field reconciliation: our
`NativeFrameHandle::HardwareBuffer(HardwareBufferInfo { .. })` (`gpu_frame.rs:200`,
struct at `format.rs:96`) carries the acquired buffer handle plus `width`, `height`,
`y_stride`, `uv_offset`, and `uv_stride`, the NV12 plane layout `AndroidGpuFrame::new`
computes (`hw_decoder.rs:268`). B1's `android::HwBuffer` does not exist yet, so B1
must be shaped to carry exactly that field set: the acquired `AHardwareBuffer` handle
(owning an extra ref) plus `width`, `height`, `y_stride`, `uv_offset`, and
`uv_stride`. If B1 carries the handle but omits the strides and UV offset, the
ImageReader path cannot describe its NV12 layout to an importer; that is a
coordination-point-1 gap to file against B1. The B3 `native()` accessor exposes the
handle as `Native::HardwareBuffer`, and `into_i420()` remains the CPU download
fallback. Without B1's `HardwareBuffer` variant and B3's accessor the decoder
degrades to a CPU download and loses the point. See ../zerocopy.md, section 2b
"Decode to render", and the sibling capture-android.md for the capture-side
HardwareBuffer input.

## What to do

MOVE. Target: Path B, external and registered (current proposal), with an in-tree
Path A fallback held on the open placement question.

Primary (Path B, external and registered): the Android backends live in our tree, in
a new crate (working name `moq-android` or a module in the iroh-live workspace) that
depends on moq-video and `ndk` 0.9. It implements moq's public B4 `Backend` traits
and produces and consumes moq's B1 `Frame::HardwareBuffer`. At startup, before the
first `Encoder::new`/`Decoder::new`, the crate calls:

```rust
moq_video::encode::register_encoder(moq_video::encode::Registration {
    name: "android-mediacodec",
    codecs: &[moq_video::encode::Codec::H264],
    open: android::open_encoder,
});
moq_video::decode::register_decoder(moq_video::decode::DecodeRegistration {
    name: "android-mediacodec",
    supports: |c| matches!(c, moq_video::decode::Codec::H264),
    open: android::open_decoder,
});
```

`Kind::Auto` then sees the Android candidates after built-in hardware and before
software, `Kind::Hardware` includes them, and `Kind::Named("android-mediacodec")`
selects them, all without editing moq's `const` tables
(../comparison/moq-changes.md:347-401). moq-side change under this plan is only what
B4 already delivers (the public traits, the two `Registration` types, the two
register functions, and the staging-area seeding); the backend code is ours.

In-tree fallback (Path A, gated on the placement question): if the placement resolves
to carrying Android in-tree, the same backend code moves to
`rs/moq-video/src/encode/backend/android.rs` and
`rs/moq-video/src/decode/backend/{android,android_hw}.rs`, cfg-gated
`#[cfg(target_os = "android")]`, and adds two `const Candidate` entries to the encode
and decode tables instead of registering. In that case B4 is not needed (an in-tree
backend just adds a candidate), and the plan's dependency set drops to B1 and B2. The
moq CI gate becomes compile-only for the Android target, matching moq-nvenc's
compile-everywhere stub, with our device validation standing in for runtime tests
(../comparison/moq-changes.md:610-617).

moq API consumed:

- B4 public `Backend` trait (encode): `encode(&mut self, frame: &Frame, timestamp:
  Timestamp, keyframe: bool) -> Result<Vec<Packet>, Error>`, `finish`, `set_bitrate`,
  `name`, plus `#[non_exhaustive] Registration { name, codecs, open }` and
  `register_encoder(reg)`.
- B4 public decode `Backend` trait: `decode(&mut self, access_unit: Bytes, timestamp:
  Timestamp, keyframe: bool) -> Result<Vec<Decoded>, Error>` and `name`, plus
  `#[non_exhaustive] DecodeRegistration { name, supports, open }` where `open:
  fn(Codec, &Config) -> Result<Box<dyn Backend>, Error>`, and `register_decoder(reg)`.
  Both Android codecs support only `Codec::H264`.
- B1 frame vocabulary: `Frame::HardwareBuffer(android::HwBuffer)` under
  `#[cfg(target_os = "android")]` as the decoder output and the encoder's potential
  zero-copy input, and the public `Native::HardwareBuffer` reached through B3's
  `native()` on the decode side. The CPU decoder produces `Frame::I420`.
- B2's `Packet { payload, timestamp }` and `moq_net::Timestamp` at the encode
  boundary; the decode side already carries per-picture timestamps.
- moq's `Error` with additive variants for MediaCodec configure, dequeue, and
  HardwareBuffer failures.

Source to port, directory `rusty-codecs/src/codec/android/` (1,528 LOC total),
verified against the working tree:

- `encoder.rs` (366 LOC). Synchronous ByteBuffer MediaCodec encoder
  (`encoder.rs:1-4`), pipelined in exactly the V4L2 shape and for the same reason B2
  exists. `push_frame` (`encoder.rs:297`) converts to NV12 and submits it via
  `queue_input_buffer(input_buf, 0, copy_len, timestamp_us, 0)` (`encoder.rs:148`),
  where the presentation timestamp rides MediaCodec's `presentationTimeUs`.
  `drain_output` dequeues completed buffers with `dequeue_output_buffer`
  (`encoder.rs:166`) and pushes them into `packet_buf: VecDeque<EncodedFrame>`
  (`encoder.rs:51`); `pop_packet` (`encoder.rs:343`) drains that queue. Output for a
  frame surfaces on a later drain, and its PTS comes back on the dequeued output
  buffer's info, not from the frame being pushed. It has a consecutive-error counter
  that triggers `try_reset` (`try_reset` fn at `encoder.rs:238`, called at
  `encoder.rs:326`) after `MAX` failures, and a `set_bitrate` (`encoder.rs:349-359`)
  that is honest but weak: `AMediaCodec_setParameters` needs API 26+ plumbing, so it
  stores the target and applies it on the next codec reset.
- `decoder.rs` (337 LOC). Synchronous ByteBuffer decoder producing CPU NV12 converted
  to the target pixel format (`decoder.rs:1-4, 61, 66, 251-302`).
- `hw_decoder.rs` (325 LOC). Zero-copy decoder using an `ImageReader` surface (see the
  Zero-copy section).
- `gpu_frame.rs` (209 LOC). `AndroidGpuFrame` and its `GpuFrameInner` impl (see the
  Zero-copy section).
- `format.rs` (276 LOC). MediaFormat construction: `MIME_AVC = "video/avc"`
  (`format.rs:12`), `COLOR_FormatYUV420SemiPlanar` (`format.rs:33`), and the encoder
  and decoder format builders setting dimensions, bitrate, framerate, color format,
  and IDR interval (`format.rs:82-124`).

Carried over: the MediaCodec session setup, the NV12 conversion and copy, the
timestamp-on-buffer plumbing, the error-reset counter, both decoders including the
ImageReader zero-copy path and the `AHardwareBuffer` wrapping, and the MediaFormat
builders. Dropped or replaced: the
`VideoEncoderFactory`/`VideoEncoder`/`VideoDecoder` trait glue (`encoder.rs:259, 280`;
`decoder.rs:61`; `hw_decoder.rs:67`) is replaced by moq's public B4 `Backend` traits;
our `GpuFrame`/`GpuFrameInner`/`NativeFrameHandle`/`HardwareBufferInfo` model by moq's
B1 `android::HwBuffer` and `Native::HardwareBuffer`; our `VideoFrame`/`FrameData`
input by moq's `Frame`; `EncodedFrame`/`MediaPacket` by `Packet`/`Bytes`; `Duration`
by `moq_net::Timestamp`; `anyhow::Result` by moq's `Error`; and the dynamic-dispatch
selection (`codec/dynamic.rs`) by moq's candidate table plus the B4 registration
ordering.

Ordered implementation steps:

1. Settle the placement question upstream before authoring the moq-facing wiring:
   Android in-tree (Path A, adds candidates, needs B1 and B2, does not need B4) or
   external registered (Path B, needs B1, B2, and B4). Author the backend code first,
   since it is identical either way, and defer only the ten-line
   registration-versus-candidate seam until the question is settled.
2. Port `format.rs` MediaFormat builders to construct from moq's `Config` (width,
   height, bitrate, framerate, gop as the IDR interval) rather than our config mirror.
3. Port the encoder as a B4 `Backend`. `encode(frame, timestamp, keyframe)` converts
   the moq `Frame` to NV12 (match `Frame::I420` and pack; any GPU variant via
   `frame.to_i420()`, which downloads it), queues it with `timestamp.as_micros()` as
   the MediaCodec `presentationTimeUs`, requests a sync frame from MediaCodec when
   `keyframe` is true, drains completed output buffers, and returns a `Vec<Packet>`
   stamping each `Packet` with the PTS read back off its output-buffer info. Keep the
   consecutive-error reset. This is the pipelined shape B2 makes expressible.
4. Port `set_bitrate`. Prefer an honest runtime retune via `AMediaCodec_setParameters`
   with `PARAMETER_KEY_VIDEO_BITRATE` when the API level supports it, so the backend
   does not force an IDR. If the minimum API level cannot guarantee it, keep the
   store-and-apply-on-reset behavior but surface it honestly: return
   `Error::BitrateUnsupported` rather than a silent success when a live retune is
   impossible. The variant takes a `&'static str` reason (`error.rs:32`), so supply
   one, for example `Error::BitrateUnsupported("MediaCodec live retune needs API
   26+")`. moq's rate controller retires cleanly on that error
   (../comparison/codecs.md:524-540). Decide the API-level floor and document it.
5. Port the ByteBuffer CPU decoder as a B4 decode `Backend` producing
   `Decoded { timestamp, frame: Frame::I420(...) }`, converting MediaCodec's NV12
   output to tightly packed I420. Echo the input timestamp per picture.
6. Port the ImageReader zero-copy decoder as a second B4 decode `Backend` producing
   `Decoded { timestamp, frame: Frame::HardwareBuffer(...) }`. Wrap the acquired
   `AHardwareBuffer` as moq's B1 `android::HwBuffer`, acquire the extra reference so
   the buffer outlives the Image (`gpu_frame.rs:195-200`), and reconcile the field set
   with B1 as detailed in the Zero-copy section. The B3 `native()` accessor exposes it
   as `Native::HardwareBuffer`, and `into_i420()` remains the CPU download fallback.
7. Wire selection. Under Path B, register both decoders (the dispatcher probes HW
   ImageReader first, then ByteBuffer, then software openh264, matching our current
   order at ../comparison/maps/rusty-codecs.md:214-216); under Path A, add the two
   `const Candidate` entries. Either way, expose them so `Kind::Named` can pin a
   specific one.
8. Provide the `register_*` calls (Path B) behind a public init function in our crate
   that an application calls once at startup, and document that it must run before the
   first `Encoder::new`/`Decoder::new` (../comparison/moq-changes.md:362-363).

Adaptation notes:

- Android is pipelined for the same reason V4L2 is, so B2 is a hard prerequisite.
  MediaCodec is dequeue-based and asynchronous: `push_frame` queues an input buffer
  with a `presentationTimeUs` and returns, and output for that frame surfaces on a
  later `dequeue_output_buffer` carrying its own PTS (`encoder.rs:148, 166`;
  ../comparison/traits-api.md:690 notes "Android MediaCodec has the same dequeue-based
  shape"). Under moq's pre-B2 `encode(&Frame, keyframe) -> Vec<Bytes>` that stamps at
  the call site, this backend would mis-stamp drained packets or block a full round
  trip per frame. B2 threads the timestamp through and returns it per `Packet`, the
  only honest way to contribute this backend.
- The zero-copy decoder is the standout capability, the Android analog of the VAAPI
  DMA-BUF and VideoToolbox CVPixelBuffer decode-to-render paths (see
  codec-vaapi-decode.md). It depends on B1's `HardwareBuffer` frame variant and B3's
  `native()` accessor; without them it degrades to a CPU download and loses the point.
- Timestamps, errors, and config follow ../base/B5-adaptation-conventions.md:
  `moq_net::Timestamp` at boundaries, moq's `Error` with additive MediaCodec and
  HardwareBuffer variants, moq's `Config` and the hang catalog rather than our
  `config.rs` mirror. No ffmpeg is introduced.

iroh-live removal side: the local module `rusty-codecs/src/codec/android/` (1,528 LOC)
is deleted only after the matching moq leaf is accepted (in-tree or registered per the
placement decision) and released, on the `up/android-mediacodec` pair branch,
preserving the HardwareBuffer decode-to-render input via render-adopt (the zero-copy
rule). The app-glue in `moq-media-android/` stays ours; only the codec dependency
travels via this module. See codec-remove in ../comparison/iroh-live-code-map.md and
the disposition register.

## Tests

- Because moq cannot run Android device tests in CI, the primary gate is compile-only
  for the `aarch64-linux-android` target plus device validation on our hardware. State
  this in the PR (../comparison/moq-changes.md:610-617).
- Device-gated round-trip tests modeled on moq's own `round_trip(encoder, decoder, w,
  h)` helper (`rs/moq-video/src/decode/backend/nvdec.rs:513`): follow nvdec's gating
  rather than `#[ignore]`, putting the tests in a `target_os = "android"`-gated (Path
  A) or crate-gated (Path B) module that skips at runtime with an `hw_available()`-style
  guard (`nvdec.rs:465`) when no device MediaCodec is reachable. Cover an encode round
  trip asserting per-packet timestamps survive the MediaCodec queue and forced
  keyframes carry in-band SPS/PPS, a ByteBuffer decode round trip, and a zero-copy
  decode test asserting `decode::Frame::native()` returns `Some(Native::HardwareBuffer(_))`
  and that `into_i420()` still downloads a correct frame from it.
- A unit test for the NV12 packing and the encoder's error-reset counter that needs no
  device.

CI cannot test it: no Android emulator with hardware codecs exists in moq CI, so the
compile-only gate plus device validation is the whole story.

## Evidence

- iroh-live source: ../comparison/maps/rusty-codecs.md:237 (the Android row:
  `AndroidEncoder`, `AndroidDecoder`, `AndroidHwDecoder`, `ndk` 0.9 MediaCodec, extra
  `format.rs` and `gpu_frame.rs` files), and ../comparison/codecs.md:259-274 (the
  "Android MediaCodec (ours only)" verdict: NV12 sessions with an error-reset counter,
  the honest-but-weak `set_bitrate`, the two decoders selected by the dynamic
  dispatcher).
- Path B rationale and the in-tree-versus-external decision:
  ../comparison/moq-changes.md:523-639 (section 4, the vendor-in-tree posture, the two
  paths, and the per-backend recommendation naming Android as "the strongest case for
  Path B").
- The B4 registration API this consumes: ../base/B4-backend-trait-registration.md and
  ../comparison/moq-changes.md:313-401 (the public `Backend` trait, `Registration` and
  `DecodeRegistration`, `register_encoder`/`register_decoder`, and the staging-area
  seeding).
- The B1 HardwareBuffer variant: ../base/B1-frame-vocabulary.md and
  ../comparison/moq-changes.md:91-134 (the `HardwareBuffer(HwBuffer)` arm and the
  public `Native::HardwareBuffer`).
- Removal ledger: codec-remove, the Android row (1,528 LOC, upstream-ours; B1
  HardwareBuffer variant, B2, B4 registration or in-tree per the placement decision;
  pair branch `up/android-mediacodec`), and the disposition register Android row.

## Coordination

- Base plans needed: ../base/B1-frame-vocabulary.md (the `HardwareBuffer` frame
  variant), ../base/B2-pts-through-encode.md (the PTS-through-encode contract), and,
  under Path B, ../base/B4-backend-trait-registration.md (the public registerable
  Backend trait). This is the only module in the campaign that needs B4, because
  Android's NDK dependencies cannot build in moq CI, making it the external "Path B"
  backend.
- The B4 placement question (open): moq vendors every backend behind `pub(crate)`
  seams and cannot build or test NDK code in CI. An in-tree Android backend forces moq
  to carry an `ndk`/NDK dependency surface foreign to its desktop and server focus and
  to accept code it cannot exercise; that is precisely the case the B4 registration API
  exists to serve, letting the Android code stay in our tree while plugging into moq's
  selection (../comparison/moq-changes.md:610-617, 635-639). B4 is the only breaking
  change in the whole campaign and the only one exclusive to Path B, so it must not be
  opened as a PR until the placement is settled upstream. Open question: in-tree (Path
  A, adds candidates, needs only B1 and B2) versus external registered (Path B, needs
  B1, B2, and B4); current proposal: external (Path B), since moq cannot test NDK code
  in CI. Author the backend code first (identical either way), then apply the decided
  seam.
- Coordination point 1 (base API freeze): code against the frozen B1, B2, and B4
  contracts. If the B4 `Registration`/`DecodeRegistration` shape cannot express the
  two-decoder selection (HW then ByteBuffer under one name), or B1's `HwBuffer` cannot
  carry the NV12 plane layout the ImageReader path needs (handle plus `width`,
  `height`, `y_stride`, `uv_offset`, `uv_stride`), stop and file the gap against the
  relevant base plan rather than diverging.
- Coordination point 2 (shared candidate tables): applies only on the in-tree
  fallback; if taken, add only the two Android candidates additively and rebase behind
  other leaves.
- NDK dlopen: the backend links the NDK `AMediaCodec` and `AHardwareBuffer` surfaces
  through `ndk` 0.9; it compiles for `aarch64-linux-android` and the moq-video
  dependency builds unchanged on non-Android hosts because the Android code is entirely
  `#[cfg(target_os = "android")]` or lives in a separate crate.
- CI hardware gating: moq CI cannot test it; compile-only for the Android target plus
  device validation on our hardware.
- Release gate: the iroh-live-side deletion of `codec/android/` waits for this leaf to
  reach a pinned moq release (Path A or B per the placement decision) and travels with
  render-adopt so the HardwareBuffer decode-to-render input survives, on the
  `up/android-mediacodec` pair branch.
