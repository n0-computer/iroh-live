# V4L2 M2M H.264 encode

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: partial. V4L2 M2M can import DMA-BUF input in principle, but this
> port ships CPU NV12 input; its load-bearing correctness is the driver-negotiated
> stride and 16-aligned height handling, not a zero-copy surface path. See the
> short section below and ../zerocopy.md, section 2a "Capture to encode".

## What it is

A hardware H.264 encoder for the Raspberry Pi and the broader embedded ARM SoC
class, driving the kernel V4L2 Memory-to-Memory stateful encoder through raw
`libc` ioctls. moq covers this platform today only with the openh264 software
encoder because it has no V4L2 M2M backend of any kind
(../comparison/codecs.md, table row "V4L2 M2M (HW, ARM Linux) ... none"). The
verdict is move-to-moq because moq lacks the capability entirely and ours is a
genuinely pipelined driver carrying stride and alignment handling that was
debugged on real bcm2835-codec hardware (commit 625c16f). It adds one
`Candidate` to the encode table behind a new `v4l2` feature, compiles cleanly on
every Linux host, and degrades at device-open time on machines without an M2M
encoder node.

## iroh-live vs moq

moq has no counterpart: the encode-backends grid shows V4L2 M2M as ours-only
(../comparison/codecs.md, section 1 "V4L2 M2M (ours only)"). Ours is
`rusty-codecs/src/codec/v4l2/encoder.rs` (1,253 LOC), a raw-ioctl M2M driver that
sets profile, level (auto-selected from resolution because bcm2835-codec defaults
to Level 1.0), bitrate, GOP, and both SPS/PPS-repeat controls so every IDR is
self-contained, respects the driver-negotiated stride and 16-aligned height when
filling OUTPUT buffers, and runs the device on a dedicated OS thread with bounded
channels. moq's Backend contract is stronger than ours on two points the port
must adopt: a mandatory `set_bitrate` (ours inherits a silent no-op, but V4L2
exposes `V4L2_CID_MPEG_VIDEO_BITRATE` at runtime so an honest implementation is
available) and per-frame forced IDR through the trait (ours forces no keyframe
today). The port keeps our raw-ioctl mechanism and gains moq's honest rate and
keyframe contract.

## Zero-copy

V4L2 M2M can in principle take a DMA-BUF as its OUTPUT buffer, so a zero-copy
input path exists as future work, but this port is CPU-input: it matches moq's
`Frame::I420` directly and calls `frame.to_i420()` for any GPU variant (a device
download), needing no B1 `Frame` variant. The load-bearing correctness is not a
surface path but the driver stride and height handling from commit 625c16f
(`queue_frame`, `encoder.rs:620-699`): it reads the driver-negotiated
`bytesperline` (`encoder.rs:634`), derives the aligned height from `sizeimage`
(`aligned_h = (sizeimage * 2) / (stride * 3)`, `encoder.rs:649-654`), copies the
Y plane row-by-row honoring stride padding (`encoder.rs:659-664`), deinterleaves
to YU12 when the driver negotiated I420 instead of NV12 (`encoder.rs:669-684`),
and zero-fills padding regions (`encoder.rs:638-640`). This must be carried
unchanged; it is the hard-won part. The DMA-BUF OUTPUT path stays a scoped
follow-up, aligned with the V4L2 decode EXPBUF follow-up (see
codec-v4l2-decode.md) and the shared `dmabuf` feature in ../base/B1-frame-vocabulary.md.

## What to do

MOVE. Target crate: moq-video.

New file `rs/moq-video/src/encode/backend/v4l2.rs` holds the backend struct, its
`Backend` impl, `pub const NAME: &str = "v4l2"`, and the `raw_v4l2` FFI submodule,
cfg-gated `#[cfg(all(target_os = "linux", feature = "v4l2"))]`. One additive
`Candidate` in the `const HARDWARE` slice of
`rs/moq-video/src/encode/backend/mod.rs:68-93` (the `const SOFTWARE` slice follows
at `:98-102`):

```rust
#[cfg(all(target_os = "linux", feature = "v4l2"))]
Candidate { name: v4l2::NAME, codecs: &[Codec::H264], open: v4l2::open },
```

It sits alongside the existing `nvenc` and `vaapi` entries. Open question: the
ordering within `HARDWARE`; current proposal: place it after `vaapi` so desktop
GPU encoders win on hosts that have both. A new `v4l2` feature in
`rs/moq-video/Cargo.toml` must add `dep:libc` (optional), because `libc` is not
currently a moq-video dependency: it is declared nowhere in
`rs/moq-video/Cargo.toml` and used nowhere under `rs/moq-video/src/` (it appears
only transitively in `Cargo.lock`). Unlike `nvenc` and `vaapi`, the feature pulls
in no external system library: `libc` is a crates.io crate providing the raw
ioctl bindings, not a `.so` loaded at build or run time, so the degrade-cleanly
posture holds while the feature also enables the `libc` dependency.

Source to port, primary `rusty-codecs/src/codec/v4l2/encoder.rs` (1,253 LOC),
verified against the working tree:

- Construction spawns a dedicated OS thread and wires three bounded channels: the
  command channel `input_tx: SyncSender<EncoderCmd>` created with capacity 4 at
  `encoder.rs:69` (field at `encoder.rs:27`), an output channel of capacity 8 at
  `encoder.rs:70`, and a one-shot init channel at `encoder.rs:71`. The device, its
  OUTPUT and CAPTURE queues, and every `!Send` ioctl handle stay confined to that
  thread.
- `push_frame` (`encoder.rs:222`) converts the incoming frame to a contiguous NV12
  buffer and sends `EncoderCmd::Encode { nv12, timestamp_us }` over the bounded
  `SyncSender` at `encoder.rs:267`, then returns immediately without blocking on
  the device.
- `pop_packet` (`encoder.rs:274`) drains completed CAPTURE buffers from
  `packet_buf: VecDeque<EncodedFrame>` (field at `encoder.rs:42`, filled at
  `encoder.rs:119` from the output channel) via `pop_front` at `encoder.rs:276`.
- The presentation timestamp rides the V4L2 buffer timestamp end to end: written
  onto the OUTPUT buffer at queue time (`buf.timestamp_sec` and `buf.timestamp_usec`
  at `encoder.rs:974-975`) and read back off the dequeued CAPTURE buffer at
  `encoder.rs:718` (`buf.timestamp_sec as u64 * 1_000_000 + buf.timestamp_usec as
  u64`). Bitstream for frame N routinely surfaces during a later `pop_packet` and
  carries frame N's own PTS, not the PTS of the frame being pushed when it emerges.
- Level auto-select because bcm2835-codec defaults to Level 1.0 (max 128x96):
  `h264_level_for_resolution` (`encoder.rs:365-380`) picks a level admitting the
  requested resolution, applied at `encoder.rs:541-542`. Profile is not
  auto-selected; it is hardcoded to `CONSTRAINED_BASELINE` (`encoder.rs:539`, the
  constant at `encoder.rs:359`).
- Both SPS/PPS-repeat controls are set so every IDR is self-contained for late
  joiners: `V4L2_CID_MPEG_VIDEO_REPEAT_SEQ_HEADER` (the bcm2835-codec spelling) and
  `V4L2_CID_MPEG_VIDEO_PREPEND_SPSPPS_TO_IDR` at `encoder.rs:549-553`.
- All device interaction is raw `libc::ioctl` over an owned fd (`VIDIOC_QBUF` at
  `encoder.rs:977`, `VIDIOC_DQBUF` at `encoder.rs:711` and `encoder.rs:741`,
  `VIDIOC_S_FMT`, `VIDIOC_S_CTRL`, `VIDIOC_STREAMON`), inside a `raw_v4l2` FFI
  submodule (`encoder.rs:311-315`).
- Device path selection: `encoder_device_path()`
  (`rusty-codecs/src/codec/v4l2.rs:57-63`) checks `V4L2_ENC_DEVICE` then falls back
  to `/dev/video11`.

Carried over: the pipelined thread and channel model, the stride and aligned
height handling, the NV12-versus-YU12 deinterleave, the profile and level
auto-selection, the SPS/PPS-repeat controls, the timestamp-on-buffer plumbing, and
the raw ioctl FFI layer. Dropped or replaced: the
`VideoEncoderFactory`/`VideoEncoder` trait glue (`encoder.rs:184` and
`encoder.rs:205`) is replaced by moq's `Backend`; our
`VideoFrame`/`FrameData`/`Nv12Planes` input by moq's `Frame`; our `EncodedFrame`
output by moq's `Packet`; `Duration` timestamps by `moq_net::Timestamp`;
`anyhow::Result` by moq's `Error`; and the `config.rs` catalog mirror is not
carried, since moq derives the catalog from the encoded SPS in its `Producer`.

Ordered implementation steps:

1. Add the `v4l2` feature and the cfg-gated `mod v4l2;` line plus the `HARDWARE`
   candidate entry. Confirm the crate still builds with the feature off and on, on
   Linux and on a non-Linux target (the module is cfg'd out entirely off Linux).
2. Port the `raw_v4l2` FFI submodule verbatim: the ioctl request constants, the
   `v4l2_format`/`v4l2_buffer`/`v4l2_control` layout structs with their alignment
   padding (`encoder.rs:389-476`), and the thin `s_ctrl`, `s_fmt`, `qbuf`, `dqbuf`
   wrappers. Keep it internal with the existing `#[allow(unreachable_pub,
   dead_code)]` and its stated reason.
3. Port the device thread and its lifecycle: open the node from
   `encoder_device_path()` semantics (env override then `/dev/video11`), negotiate
   OUTPUT and CAPTURE formats with `VIDIOC_S_FMT`, set the framerate, allocate and
   map buffers with `VIDIOC_REQBUFS`/`VIDIOC_QUERYBUF`, and stream on. On any
   failure at open or negotiation, return a moq `Error` so `Kind::Auto` falls
   through to the next candidate (the degrade path; see Coordination).
4. Set the encode controls at init: bitrate via `V4L2_CID_MPEG_VIDEO_BITRATE`, GOP,
   the auto-selected profile and level (`h264_level_for_resolution`), and both
   SPS/PPS-repeat controls. Derive the default bitrate from moq's `Config::bitrate`,
   not our config mirror.
5. Implement `Backend::encode(&mut self, frame, timestamp, keyframe)`. Convert the
   moq `Frame` to a contiguous NV12 buffer (match `Frame::I420` and pack; for any
   GPU variant call `frame.to_i420()`, which downloads it, then interleave to NV12),
   send it plus `timestamp.as_micros()` and the `keyframe` flag over the bounded
   command channel, then drain any completed CAPTURE buffers into `Vec<Packet>`,
   stamping each `Packet` with the timestamp read back off its dequeued buffer.
   Return zero packets when the device has not yet produced output for an earlier
   frame and several when a drain catches up. This is the pipelined shape B2 makes
   expressible.
6. Wire per-frame forced IDR: when `keyframe` is true, set
   `V4L2_CID_MPEG_VIDEO_FORCE_KEY_FRAME` before queueing the OUTPUT buffer. moq's
   contract requires it (../comparison/maps/moq-video.md:107-112).
7. Implement an honest `Backend::set_bitrate` via `VIDIOC_S_CTRL` on
   `V4L2_CID_MPEG_VIDEO_BITRATE`, applied without forcing an IDR so the retune is
   safe for the congestion controller (../comparison/maps/moq-video.md:114-123).
   This replaces our current silent no-op. Return `Ok(())` on success; return the
   device error only on a real ioctl failure, never a silent no-op.
8. Implement `Backend::finish` to stream off, drain remaining CAPTURE buffers, and
   return them as `Vec<Packet>`, and `Backend::name` returning `NAME`.
9. Carry the stride and aligned-height handling from `queue_frame` unchanged in the
   conversion step of `encode`; this is the load-bearing correctness code and must
   not be simplified away.

Raw libc ioctl versus the v4l2r crate: the sibling codec-v4l2-decode.md ports our
decoder, which uses the `v4l2r` crate. The encoder deliberately does not.
bcm2835-codec needs several controls set in a specific order before format
negotiation (profile and level before the OUTPUT format, the two distinct
SPS/PPS-repeat controls) and needs the driver-negotiated stride and aligned height
read back precisely, and the raw ioctl path gives that exact control without
fighting v4l2r's type-state generics. The raw approach was also cross-validated
against ffmpeg's h264_v4l2m2m for driver compatibility (`encoder.rs:308`); the port
removes the ffmpeg cross-check comment's dependency, keeping only the raw ioctl
code it validated. Timestamps, errors, and config follow ../base/B5-adaptation-conventions.md:
`moq_net::Timestamp` at the boundary, moq's `Error` with an additive
device-failure variant, and moq's `Config`.

iroh-live removal side: the local module `rusty-codecs/src/codec/v4l2/`
(encode+decode, 1,856 LOC combined) is deleted only after the matching moq leaf
merges and releases, on the `up/v4l2-encode` pair branch, gated by the atomic
per-platform rule (Linux non-NVIDIA flips only once the VAAPI and V4L2 leaves have
all released). See codec-remove in ../comparison/iroh-live-code-map.md and the
disposition register.

## Tests

- A hardware round-trip test in the style of moq's VideoToolbox and NVENC tests
  (../comparison/maps/moq-video.md:711-713): open the V4L2 encoder, feed a small
  synthetic I420 sequence with monotonically increasing timestamps, collect the
  `Packet`s, and assert every packet carries a timestamp drawn from the input set
  (proving the PTS survives the queue), that a forced-keyframe frame produces an IDR
  carrying in-band SPS/PPS, and that the total packet count matches the frame count
  once `finish` drains the tail.
- Follow moq's own hardware-test pattern rather than `#[ignore]`: model on the
  cfg-gated `round_trip(encoder, decoder, w, h)` helper in
  `rs/moq-video/src/decode/backend/nvdec.rs:513`, whose tests live in a
  `feature = "nvdec"`-gated module and skip at runtime with an `hw_available()`
  guard (`nvdec.rs:465`, `if !hw_available() { return; }`). Gate the V4L2 test
  module behind the `v4l2` feature and skip at runtime when no M2M encoder node
  opens.
- A pure-unit test for the NV12-versus-YU12 deinterleave and the aligned-height
  derivation, driven by a fabricated `sizeimage`/`bytesperline` pair, so the 625c16f
  logic is covered without a device.

CI cannot test this: moq CI has no Raspberry Pi or embedded ARM board, so the
compile-only gate plus our hardware validation is the agreed story
(../comparison/moq-changes.md:597-603).

## Evidence

- iroh-live source and structure: ../comparison/maps/rusty-codecs.md:237 (the V4L2
  row: encoder on raw libc ioctls, decoder on `v4l2r`, tested on Pi bcm2835-codec,
  device paths `/dev/video11` and `/dev/video10`, env-overridable), and
  ../comparison/codecs.md, section 1 "V4L2 M2M (ours only)" (lines 184-204: the
  verdict, profile and level auto-selection, stride and alignment handling, the
  SPS/PPS-repeat controls, the dedicated OS thread with bounded channels).
- The pipelined-encoder analysis that makes B2 a hard prerequisite:
  ../comparison/traits-api.md:680-706 (section 6, "Streaming versus one-shot") and
  ../comparison/moq-changes.md:104-118, 313-345 (change 2 and change 7 point 1).
- moq target shape: ../comparison/maps/moq-video.md:100-153 (the encode `Backend`
  trait, the `Candidate` struct, the `const HARDWARE`/`SOFTWARE` slices, and `open`
  dispatch).
- Removal ledger: codec-remove, the V4L2 row (1,856 LOC, upstream-ours, waits on the
  v4l2 encode and decode leaves; pair branches `up/v4l2-encode`, `up/v4l2-decode`),
  and the disposition register V4L2 row.

## Coordination

- Base plan needed: ../base/B2-pts-through-encode.md is a hard prerequisite. The
  V4L2 M2M encoder is a hardware queue: `push_frame` returns before the device has
  produced any bitstream, and `pop_packet` drains packets belonging to frames pushed
  several inputs earlier (../comparison/traits-api.md:680-694). Under moq's pre-B2
  `encode(&Frame, keyframe) -> Vec<Bytes>`, which stamps output with the call-site
  timestamp, this backend would either block a full device round trip per call
  (adding a frame or more of latency and stalling the capture loop) or return frame
  N-k's bitstream mis-stamped with frame N's PTS. B2 threads the timestamp through
  and returns it per `Packet`, making the one-shot signature capability-equivalent to
  our push/pop model for pipelined devices while changing no existing backend's
  observable behavior. No B1, B3, or B4 dependency: this is a CPU-input, in-tree
  backend, so it needs neither the GPU-frame vocabulary nor the registration API.
- Degrade path, adapted to a device backend: moq's convention is that a backend
  builds on hosts without the hardware and degrades cleanly, matching moq-nvenc's
  dlopen stub. The V4L2 encoder has no system library to dlopen; it is pure `libc`
  ioctl against a character device, so it compiles on every Linux host
  unconditionally, and "degrade cleanly" is satisfied at runtime: opening the device
  node fails on a host without an M2M encoder, `open` returns a moq `Error`, and
  `Kind::Auto` falls through. State this explicitly so a reviewer expecting a
  libloading probe understands why there is none.
- dep:libc: the `v4l2` feature adds `dep:libc` (optional), a crates.io crate not
  previously in moq-video, providing the raw ioctl bindings and no system `.so`.
- Coordination point 2 (shared candidate tables): add only this backend's own
  `Candidate` to `encode/backend/mod.rs`; do not refactor the table or touch another
  backend's entry. If it lands after another Wave 2 encode leaf, rebase onto the
  merged table.
- Coordination point 1 (base API freeze): code against B2's frozen `encode`
  signature and `Packet` type. If the port finds B2 insufficient (for example if the
  drained-packet timestamp needs more than a single `Timestamp`), stop and file the
  gap against B2 rather than diverging.
- Feature sharing with codec-v4l2-decode.md: both leaves add a `v4l2` feature.
  Whichever lands first introduces it; the second extends it (encode needs no extra
  crate beyond `libc`, decode adds `v4l2r`). Coordinate the Cargo.toml edit so the
  two do not conflict beyond a rebase.
- Per-segment transcode and FETCH rate control (overview coordination point 7):
  per-group transcoding builds a fresh encoder per fetched group. The V4L2 encoder is
  the most expensive backend to re-open (full device open plus `REQBUFS` plus
  `STREAMON`), unlike rav1e (cheap) and VAAPI (a VA context). There is no reset path
  today: `EncoderCmd` has only the `Encode` variant (`encoder.rs:45`), `new()` blocks
  on the init one-shot only after the device thread has opened the fd, negotiated
  formats, run `REQBUFS`, and issued `STREAMON` (`encoder.rs:58`, sequence around
  `encoder.rs:595-780`), and `Drop` merely closes the command channel and joins the
  thread (`encoder.rs:280`). The concrete change is a session-reuse path: a new
  `EncoderCmd::Reset { config }` (or a `RawEncoder` re-negotiate method) that makes
  the device thread re-run `S_FMT` and re-apply the controls without re-opening the
  fd or re-running `REQBUFS`/`STREAMON`, so a per-group transcode loop holds one open
  V4L2 session and only resets its rate state and forces an IDR at each group
  boundary. Expose per-segment rate primitives (an honest `set_bitrate`, forced IDR
  per GOP, and a target-bitrate knob) and defer the rate-control policy to
  moq-transcode; do not embed a streaming controller.
- CI hardware gating: not testable in moq CI (no Pi or embedded board); compile-only
  gate plus our hardware validation, stated in the PR
  (../comparison/moq-changes.md:597-603).
- Release gate: the iroh-live-side deletion of `codec/v4l2/` waits for this leaf to
  reach a pinned moq release and travels with the version bump in a single revertible
  commit on `up/v4l2-encode`, per the upstream gating rule.
