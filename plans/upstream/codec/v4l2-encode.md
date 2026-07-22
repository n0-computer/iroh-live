# v4l2-encode. V4L2 M2M hardware H.264 encode for Raspberry Pi and embedded ARM

Branch: moq-upstream/v4l2-encode          PR target: base branch, then moq main
Depends on: B2 (PTS through encode)
Path: A (in-tree)
Size: L

## Goal

Give moq-video a hardware H.264 encoder for the Raspberry Pi and the broader
embedded ARM SoC class, a platform moq covers today only with the openh264
software encoder because it has no V4L2 Memory-to-Memory backend of any kind
(`comparisons/codecs.md:50`, "V4L2 M2M (HW, ARM Linux) ... none"). The backend
drives the kernel V4L2 M2M stateful encoder through raw ioctls, carries the
driver-negotiated stride and 16-aligned height handling that was debugged on real
bcm2835-codec hardware (commit 625c16f), and arrives with an honest `set_bitrate`
and per-frame forced IDR as moq's `Backend` contract requires. It adds one
`Candidate` to the encode table behind a new `v4l2` feature and compiles cleanly
on every Linux host, degrading at device-open time on machines without an M2M
encoder node.

## Evidence

- iroh-live source and structure: `comparisons/maps/rusty-codecs.md:235` (the
  V4L2 row: encoder on raw libc ioctls, decoder on `v4l2r`, tested on Pi
  bcm2835-codec, device paths `/dev/video11` and `/dev/video10`,
  env-overridable), and `comparisons/codecs.md:184-202` (the "V4L2 M2M (ours
  only)" verdict, profile and level auto-selection, stride and alignment handling,
  the SPS/PPS-repeat controls, the dedicated OS thread with bounded channels).
- The pipelined-encoder analysis that makes B2 a hard prerequisite:
  `comparisons/traits-api.md:680-706` (section 6, "Streaming versus one-shot")
  and `comparisons/moq-changes.md:104-118, 313-345` (change 2 and change 7 point
  1). The V4L2 M2M encoder is the concrete backend for which the zero-frame-delay
  assumption is false by construction.
- moq target shape: `comparisons/maps/moq-video.md:100-153` (the encode `Backend`
  trait, the `Candidate` struct, the `const HARDWARE`/`SOFTWARE` slices, and
  `open` dispatch).

## moq API consumed

- The PTS-through-encode contract from base plan B2: `Backend::encode(&mut self,
  frame: &Frame, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Packet>,
  Error>` with `#[non_exhaustive] pub struct Packet { pub payload: Bytes, pub
  timestamp: Timestamp }`. This is the whole reason B2 is a dependency; see
  "Adaptation notes".
- moq's private input `Frame` enum (`rs/moq-video/src/frame.rs:23-36`), consumed
  through its CPU arm only. This backend takes CPU input, so it matches
  `Frame::I420` and calls `frame.to_i420()` for any other CPU-representable
  variant. It needs no new `Frame` variant and therefore does not depend on B1.
- moq's `Error` (`rs/moq-video/src/error.rs`), including
  `Error::BitrateUnsupported` (unused here because V4L2 retune is honest) and a
  new additive variant for device or ioctl failure per base plan B5.
- `moq_net::Timestamp` at the encode boundary.
- The `Config` construction type (`rs/moq-video/src/encode/encoder.rs:55-70`):
  `width`, `height`, `framerate`, `bitrate`, `gop`, `codec: Codec::H264`.

## Source to port

Primary: `rusty-codecs/src/codec/v4l2/encoder.rs` (1,253 LOC), which is a
genuinely pipelined M2M driver, verified against the working tree:

- Construction spawns a dedicated OS thread and wires three bounded channels:
  the command channel `input_tx: SyncSender<EncoderCmd>` created with capacity 4
  at `encoder.rs:69` (field at `encoder.rs:27`), an output channel of capacity 8
  at `encoder.rs:70`, and a one-shot init channel at `encoder.rs:71`. The device,
  its OUTPUT and CAPTURE queues, and every `!Send` ioctl handle stay confined to
  that thread.
- `push_frame` (`encoder.rs:222`) converts the incoming frame to a contiguous
  NV12 buffer and sends `EncoderCmd::Encode { nv12, timestamp_us }` over the
  bounded `SyncSender` at `encoder.rs:267`, then returns immediately. It does not
  block on the device.
- `pop_packet` (`encoder.rs:274`) drains completed CAPTURE buffers from
  `packet_buf: VecDeque<EncodedFrame>` (field at `encoder.rs:42`, filled at
  `encoder.rs:119` from the output channel) via `pop_front` at `encoder.rs:276`.
- The presentation timestamp rides the V4L2 buffer timestamp end to end: it is
  written onto the OUTPUT buffer at queue time (`buf.timestamp_sec` and
  `buf.timestamp_usec` at `encoder.rs:974-975`) and read back off the dequeued
  CAPTURE buffer at `encoder.rs:718` (`buf.timestamp_sec as u64 * 1_000_000 +
  buf.timestamp_usec as u64`). Bitstream for frame N routinely surfaces during a
  later `pop_packet`, and it carries frame N's own PTS rather than the PTS of the
  frame being pushed when it emerges.
- The stride and height alignment fix from commit 625c16f lives in `queue_frame`
  (`encoder.rs:620-699`): it reads the driver-negotiated `bytesperline`
  (`encoder.rs:634`) and derives the aligned height from `sizeimage`
  (`aligned_h = (sizeimage * 2) / (stride * 3)`, `encoder.rs:649-654`), copies the
  Y plane row-by-row honoring stride padding (`encoder.rs:659-664`), and
  deinterleaves to YU12 when the driver negotiated I420 instead of NV12
  (`encoder.rs:669-684`), zero-filling padding regions (`encoder.rs:638-640`).
- Profile and level are auto-selected because bcm2835-codec defaults to Level 1.0
  (max 128x96): `h264_level_for_resolution` (`encoder.rs:365-388`) picks a level
  that admits the requested resolution, applied at `encoder.rs:541-542`.
- Both SPS/PPS-repeat controls are set so every IDR is self-contained for late
  joiners: `V4L2_CID_MPEG_VIDEO_REPEAT_SEQ_HEADER` (the bcm2835-codec spelling)
  and `V4L2_CID_MPEG_VIDEO_PREPEND_SPSPPS_TO_IDR` at `encoder.rs:549-553`.
- All device interaction is raw `libc::ioctl` over an owned fd (for example
  `VIDIOC_QBUF` at `encoder.rs:977`, `VIDIOC_DQBUF` at `encoder.rs:711` and
  `encoder.rs:741`, `VIDIOC_S_FMT`, `VIDIOC_S_CTRL`, `VIDIOC_STREAMON`), inside a
  `raw_v4l2` FFI submodule (`encoder.rs:311-315`).
- Device path selection: `encoder_device_path()`
  (`rusty-codecs/src/codec/v4l2.rs:57-62`) checks `V4L2_ENC_DEVICE` then falls
  back to `/dev/video11`.

Carried over: the pipelined thread and channel model, the stride and aligned
height handling, the NV12-versus-YU12 deinterleave, the profile and level
auto-selection, the SPS/PPS-repeat controls, the timestamp-on-buffer plumbing,
and the raw ioctl FFI layer.

Dropped or replaced: the `VideoEncoderFactory`/`VideoEncoder` trait glue
(`encoder.rs:184` and `encoder.rs:205`) is replaced by moq's `Backend` trait; our
`VideoFrame`/`FrameData`/`Nv12Planes` input model is replaced by moq's `Frame`;
our `EncodedFrame` output is replaced by moq's `Packet`; `Duration` timestamps
become `moq_net::Timestamp`; `anyhow::Result` becomes moq's `Error`; and the
`config.rs` catalog mirror is not carried, since moq derives the catalog from the
encoded SPS in its `Producer`.

## Target in moq

- New file `rs/moq-video/src/encode/backend/v4l2.rs` holding the backend struct,
  its `Backend` impl, the `pub const NAME: &str = "v4l2"`, and the `raw_v4l2` FFI
  submodule, cfg-gated `#[cfg(all(target_os = "linux", feature = "v4l2"))]`.
- One additive `Candidate` in the `const HARDWARE` slice of
  `rs/moq-video/src/encode/backend/mod.rs:68-102`:

  ```rust
  #[cfg(all(target_os = "linux", feature = "v4l2"))]
  Candidate { name: v4l2::NAME, codecs: &[Codec::H264], open: v4l2::open },
  ```

  It sits alongside the existing `nvenc` and `vaapi` entries. Ordering within
  `HARDWARE` is a maintainer call; propose placing it after `vaapi` so desktop GPU
  encoders win on hosts that have both.
- A new `v4l2` feature in `rs/moq-video/Cargo.toml`. Unlike `nvenc` and `vaapi`,
  it pulls in no external system-library crate: the backend is pure `libc` ioctl,
  and `libc` is already a moq dependency. The feature exists only to gate the
  module and the candidate.

## Implementation steps

1. Add the `v4l2` feature and the cfg-gated `mod v4l2;` line plus the `HARDWARE`
   candidate entry. Confirm the crate still builds with the feature off and on,
   on Linux and on a non-Linux target (the module is cfg'd out entirely off
   Linux).
2. Port the `raw_v4l2` FFI submodule verbatim: the ioctl request constants, the
   `v4l2_format`/`v4l2_buffer`/`v4l2_control` layout structs with their alignment
   padding (`encoder.rs:389-476`), and the thin `s_ctrl`, `s_fmt`, `qbuf`, `dqbuf`
   wrappers. This is mechanical and platform-specific; keep it as an internal
   module with the existing `#[allow(unreachable_pub, dead_code)]` and its stated
   reason.
3. Port the device thread and its lifecycle: open the node from
   `encoder_device_path()` semantics (env override then `/dev/video11`), negotiate
   OUTPUT and CAPTURE formats with `VIDIOC_S_FMT`, set the framerate, allocate and
   map buffers with `VIDIOC_REQBUFS`/`VIDIOC_QUERYBUF`, and stream on. On any
   failure at open or negotiation, return a moq `Error` so `Kind::Auto` falls
   through to the next candidate. This is the degrade path (see Adaptation notes).
4. Set the encode controls at init: bitrate via `V4L2_CID_MPEG_VIDEO_BITRATE`,
   GOP, the auto-selected profile and level (`h264_level_for_resolution`), and
   both SPS/PPS-repeat controls. Derive the default bitrate from moq's
   `Config::bitrate` (already computed by the front end), not our config mirror.
5. Implement `Backend::encode(&mut self, frame, timestamp, keyframe)`. Convert
   the moq `Frame` to a contiguous NV12 buffer (match `Frame::I420` and pack; for
   any other CPU variant call `frame.to_i420()` then interleave to NV12), send it
   plus `timestamp.as_micros()` and the `keyframe` flag over the bounded command
   channel, then drain any completed CAPTURE buffers into `Vec<Packet>`, stamping
   each `Packet` with the timestamp read back off its dequeued buffer. Return zero
   packets when the device has not yet produced output for an earlier frame and
   several when a drain catches up. This is the pipelined shape that B2 makes
   expressible.
6. Wire per-frame forced IDR: when `keyframe` is true, set
   `V4L2_CID_MPEG_VIDEO_FORCE_KEY_FRAME` before queueing the OUTPUT buffer. Our
   current code hardcodes no forced keyframe; moq's contract requires it
   (`comparisons/maps/moq-video.md:107-112`).
7. Implement an honest `Backend::set_bitrate` via `VIDIOC_S_CTRL` on
   `V4L2_CID_MPEG_VIDEO_BITRATE`, applied without forcing an IDR, so the retune
   is safe for the congestion controller
   (`comparisons/maps/moq-video.md:114-123`). This replaces our current silent
   no-op (`comparisons/codecs.md:198-202` notes V4L2 exposes the runtime control,
   so the honest implementation is available). Return `Ok(())` on success; return
   the device error only on a real ioctl failure, never a silent no-op.
8. Implement `Backend::finish` to stream off, drain remaining CAPTURE buffers, and
   return them as `Vec<Packet>`, and `Backend::name` returning `NAME`.
9. Carry the stride and aligned-height handling from `queue_frame` unchanged in
   the conversion step of `encode`; this is the load-bearing correctness code and
   must not be simplified away.

## Tests

- A hardware round-trip test in the style of moq's VideoToolbox and NVENC tests
  (`comparisons/maps/moq-video.md:711-713`): open the V4L2 encoder, feed a small
  synthetic I420 sequence with monotonically increasing timestamps, collect the
  `Packet`s, and assert every packet carries a timestamp drawn from the input set
  (proving the PTS survives the queue), that a forced-keyframe frame produces an
  IDR carrying in-band SPS/PPS, and that the total packet count matches the frame
  count once `finish` drains the tail.
- Mark the test `#[ignore]` with a reason when no V4L2 M2M encoder node is
  present, exactly as the plan runbook prescribes for hardware not in CI runners.
  The moq maintainer has no Pi in CI, so the compile-only gate plus our hardware
  validation is the agreed story (`comparisons/moq-changes.md:597-603`).
- A pure-unit test for the NV12-versus-YU12 deinterleave and the aligned-height
  derivation, driven by a fabricated `sizeimage`/`bytesperline` pair, so the
  625c16f logic is covered without a device.

## Adaptation notes

- Why B2 is a hard prerequisite, stated precisely: the V4L2 M2M encoder is a
  hardware queue. `push_frame` returns before the device has produced any
  bitstream, and `pop_packet` drains packets that belong to frames pushed several
  inputs earlier (`comparisons/traits-api.md:680-694`). Under moq's current
  `encode(&Frame, keyframe) -> Vec<Bytes>`, which stamps output with the
  call-site timestamp (`comparisons/traits-api.md:668-679`), this backend would
  either have to block a full device round trip per call (adding a frame or more
  of latency and stalling the capture loop) or return frame N-k's bitstream to be
  mis-stamped with frame N's PTS. B2 threads the timestamp through and returns it
  per `Packet`, which makes the one-shot signature capability-equivalent to our
  push/pop model for pipelined devices and changes no existing moq backend's
  observable behavior. This backend cannot be contributed honestly without it.
- dlopen and degrade, adapted to a device backend: moq's convention is that a
  backend builds on hosts without the hardware and degrades cleanly, matching
  moq-nvenc's dlopen stub (`0-overview.md:184-186`). The V4L2 encoder has no
  system library to dlopen. It is pure `libc` ioctl against a character device, so
  it compiles on every Linux host unconditionally, and the "degrade cleanly" half
  of the convention is satisfied at runtime: opening the device node fails on a
  host without an M2M encoder, `open` returns a moq `Error`, and `Kind::Auto`
  falls through to the next candidate. State this explicitly in the module doc so
  a reviewer expecting a libloading probe understands why there is none.
- Raw libc ioctl versus the v4l2r crate: the sibling v4l2-decode plan ports our
  decoder, which uses the `v4l2r` crate. The encoder deliberately does not.
  bcm2835-codec needs several controls set in a specific order before format
  negotiation (profile and level before the OUTPUT format, the two distinct
  SPS/PPS-repeat controls) and needs the driver-negotiated stride and aligned
  height read back precisely, and the raw ioctl path gives that exact control
  without fighting v4l2r's type-state generics. The raw approach was also
  cross-validated against ffmpeg's h264_v4l2m2m for driver compatibility
  (`encoder.rs:308`). Keep the two backends on their respective mechanisms rather
  than forcing one crate on both; note the split in the PR description so the
  maintainer sees it is intentional.
- Timestamps, errors, and config follow base plan B5: `moq_net::Timestamp` at the
  boundary, moq's `Error` with an additive device-failure variant, and moq's
  `Config` rather than our `config.rs` mirror. No ffmpeg is introduced; the port
  removes even the historical ffmpeg cross-check comment's dependency, keeping
  only the raw ioctl code it validated.

## Coordination

- Coordination point 2 (shared candidate tables): this leaf adds only its own
  `Candidate` to `encode/backend/mod.rs`. It must not refactor the table or touch
  another backend's entry. If it lands after another Wave 2 encode leaf, rebase
  onto the merged table.
- Coordination point 1 (base API freeze): the backend codes against B2's frozen
  `encode` signature and `Packet` type. If the port finds B2 insufficient (for
  example if the drained-packet timestamp needs more than a single `Timestamp`),
  stop and file the gap against base plan B2 rather than diverging.
- No B1, B3, or B4 dependency: this is a CPU-input, in-tree backend, so it needs
  neither the GPU-frame vocabulary nor the registration API.

## Transcode and rate control (overview coordination point 7)

Per-group transcoding builds a fresh encoder per fetched group. Our V4L2 encoder
is the most expensive backend to re-open (full device open plus `REQBUFS` plus
`STREAMON`), unlike rav1e (cheap) and VAAPI (a VA context). This backend must add
a reset or session-reuse path so a per-group transcode loop reuses one V4L2
session rather than re-opening per group. Expose per-segment rate primitives (an
honest `set_bitrate`, forced IDR per GOP, and a target-bitrate knob) and defer
the rate-control policy to moq-transcode; do not embed a streaming controller.

## Acceptance checklist

- The crate builds with `--features v4l2` and without it, on a Linux target and a
  non-Linux target, with no new external system-library dependency.
- `Backend::encode` returns `Vec<Packet>` with per-packet timestamps that match
  the frames they belong to, verified on hardware where available and by the
  ignored round-trip test otherwise.
- `set_bitrate` is honest: it applies `V4L2_CID_MPEG_VIDEO_BITRATE` without an IDR
  and never silently no-ops.
- Per-frame `keyframe: true` forces an IDR that carries in-band SPS/PPS.
- The stride, aligned-height, and NV12-versus-YU12 handling from 625c16f is
  present and unit-tested.
- The candidate is added additively; no other table entry is touched.
- The PR description states the raw-ioctl-versus-v4l2r split, the device-probe
  degrade path, and the missing-hardware test gating.
