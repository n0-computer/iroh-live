# av1-software. Software AV1: rav1e encode and rav1d decode

Branch: moq-upstream/av1-software          PR target: base branch, then moq main
Depends on: B2 (PTS through encode); external prerequisite: rav1d fork resolution (coordination point 4)
Path: A (in-tree)
Size: M-L

## Goal

Give moq-video its first AV1 encoder of any kind, and its first software AV1
decoder, by porting iroh-live's rav1e encode and rav1d decode backends as two new
in-tree candidates. moq today has AV1 decode only through NVDEC, gated to 8-bit
4:2:0 non-monochrome, and no AV1 encode anywhere; its public encode `Codec` enum
is `{H264, H265}`. The rav1e encoder is the only AV1 encoder on either side, and
rav1d decode is the only software AV1 decode on either side, so this contribution
completes moq's hardware-then-software fallback story for AV1 the way openh264
completes it for H.264. It is a Path A in-tree contribution that needs only the
B2 PTS change from the base, plus one hard external prerequisite that must be
resolved before the PR can open.

## Evidence

- `comparisons/codecs.md` section 3 (AV1): the only software AV1 path on either
  side, moq is decode-only NVDEC-only, the git-fork dependency risk, and the
  encoder CPU-cost caveat at speed 10.
- `comparisons/moq-changes.md` section 4 recommendation (AV1 in-tree, Path A,
  once the dependency pin is resolved) and change table row for the AV1 backends.
- `comparisons/maps/rusty-codecs.md` section 2 (av1 row) and section 5 (the
  `rav1d` git memorysafety fork with `bitdepth_8/16` and `asm` features).

## moq API consumed

- B2 `Packet` and the timestamp argument on `Backend::encode(&mut self, frame:
  &Frame, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Packet>, Error>`.
  The rav1e encoder needs this because it reorders frames through its lookahead,
  so output packets must carry the timestamp of the frame they belong to, not the
  call-site frame.
- The decode side uses the existing `Decoded { timestamp, frame }` shape; no B1
  or B3 handle work is needed, since software AV1 decodes to CPU I420 and returns
  `Frame::I420`, which needs no `Native` vocabulary. This backend is therefore
  independent of B1 and B3.

## Source to port

- `rusty-codecs/src/codec/av1/encoder.rs` (369 LOC). rav1e 0.8 live-streaming
  configuration: `min_key_frame_interval = 0` (`av1/encoder.rs:52`), `low_latency
  = true` (`:54`), `error_resilient = true` (`:55`), `quantizer = 0` with
  `min_quantizer = 1` for bitrate-based rate control (`:58-59`), the lookahead
  timestamp map (`timestamp_map` field `:27-29`, insert at `:241`, recovery in
  the drain loop `:110-117`), and the ISOBMFF codec-string catalog parameters
  matched to the BT.601 conversion pipeline (`:148-178`, mirrored from
  `config.rs:90-116`).
- `rusty-codecs/src/codec/av1/decoder.rs` (365 LOC) and its safe shim
  `rusty-codecs/src/codec/av1/rav1d_safe.rs` (196 LOC). `set_n_threads(0)` and
  `set_max_frame_delay(1)` for latency (`av1/decoder.rs:47-48`), and
  stride-checked plane conversion (`:91-98`).

Carries over: the rav1e tuning, the lookahead timestamp map (this is what B2
exists to serve), the stride-checked I420 plane extraction, and the codec-string
parameter set. Dropped: our `VideoEncoderFactory`/`VideoDecoder` trait glue and
our `config.rs` catalog mirror. The construction config comes from moq's encode
`Config` and hang catalog types, and the encoder's catalog output becomes a
`hang::catalog::VideoConfig` rather than our mirror.

## Target in moq

- New `rs/moq-video/src/encode/backend/av1.rs` (rav1e), implementing the crate's
  `Backend` trait and returning `Vec<Packet>` per the B2 signature.
- New `rs/moq-video/src/decode/backend/av1.rs` (rav1d), implementing the decode
  `Backend` and producing `Frame::I420`.
- One `const Candidate` added to the SOFTWARE slice in
  `rs/moq-video/src/encode/backend/mod.rs` and one to
  `rs/moq-video/src/decode/backend/mod.rs`, each additive (coordination point 2).
- The public encode `Codec` enum in `rs/moq-video/src/encode/encoder.rs:21-40`
  gains an `Av1` variant. The enum is `#[non_exhaustive]` (`encoder.rs:22`), so
  adding the variant is additive, not breaking. The decode path already knows
  AV1 (NVDEC), so decode registration is likewise purely additive.

## Implementation steps

1. Resolve the rav1d dependency first (see Coordination). Nothing below can land
   until the git-fork pin becomes a crates.io dependency, a vendored safe
   wrapper, or an accepted git exception. Do not open the PR until this is done.
2. Add the rav1e encode backend behind a `av1` (software) feature, dlopen-free
   because rav1e is pure Rust and builds everywhere, matching the compile-
   everywhere posture. Port the tuning and the lookahead timestamp map; the map
   is the reason the B2 timestamp argument exists, so wire output packet
   timestamps through it.
3. Implement an honest `set_bitrate`: rav1e cannot cheaply retune a live context,
   so return `Error::BitrateUnsupported` rather than a silent no-op, per the
   adaptation conventions. State the limitation in the backend doc.
4. Support per-frame forced IDR through the `keyframe` argument, as every moq
   encoder must.
5. Add the rav1d decode backend, producing `Frame::I420`. Port the safe shim and
   the stride-checked plane conversion. Add the AV1 decode `Candidate` with a
   `supports` predicate.
6. Build the encoder's catalog output as a `hang::catalog::VideoConfig` with the
   AV1 codec-string parameters, dropping our `config.rs` mirror.

## Tests

- A software round-trip test in moq's style: encode a synthetic frame sequence
  through rav1e, decode it through rav1d, and assert frames come back with
  monotonic timestamps (mirrors our `timestamps_increase` test at
  `av1/encoder.rs:324-334`). Unlike moq's `av1_is_supported_by_hardware_only`
  test, which pins that software AV1 decode currently fails to open, this test
  demonstrates the software path now opening and round-tripping.
- Keep resolution and frame count small: rav1e at speed 10 is roughly 4x slower
  per resolution step and dominates CI time (our own note at
  `av1/encoder.rs:342`). State the CPU-cost honesty in the PR: rav1e at speed 10
  is usable at conference resolutions, not at 1080p60 on small cores.

## Adaptation notes

- Timestamps are `moq_net::Timestamp` at the boundary; the internal lookahead map
  may keep `Duration` behind the boundary if convenient.
- No ffmpeg, no dlopen needed (rav1e and rav1d are pure Rust plus optional asm).
- Errors adopt moq's `Error`; `set_bitrate` returns `Error::BitrateUnsupported`.
- Configs come from hang catalog types, not our `config.rs`.

## Coordination

- Coordination point 4 (rav1d fork resolution) is a hard gating prerequisite.
  Our rav1d dependency is a git pin on the memorysafety fork with `bitdepth_8`,
  `bitdepth_16`, and `asm` features (`rusty-codecs/Cargo.toml:33`, feature at
  `:65`), which is unacceptable upstream: moq pins crates.io versions throughout,
  runs release-plz, and explicitly forbids git dependencies (overview adaptation
  conventions, and moq-changes.md section 4). This is not a decision the leaf
  agent makes alone. The pin must first be resolved to a published rav1d or
  dav1d-rs release, a vendored safe wrapper in the style of moq-nvenc, or an
  explicitly accepted exception, coordinated with a human. The rav1e 0.8
  dependency is crates.io and unproblematic. Flag this and proceed only once
  resolved.
- Coordination point 2 (shared candidate tables): add only the AV1 candidates;
  do not refactor the tables.
- Coordination point 4 (rav1d fork): do not open this PR until the git-fork pin
  is resolved (published, moved to a released rav1d, or vendored).

## Transcode and rate control (overview coordination point 7)

A fresh rav1e `Context` per group fits per-group transcoding naturally and is the
cheapest reconfigure of any backend, so rav1e with rav1d is the software
transcode fallback for relay hosts without hardware, subject to the CPU cost of
software encode. Expose a per-encode target-bitrate and defer the rate-control
policy to moq-transcode rather than embedding a streaming controller.

## Acceptance checklist

- rav1d dependency resolved to a non-git source, confirmed with a human.
- Encode and decode backends implemented against the frozen B2 signature.
- Honest `set_bitrate` returning `Error::BitrateUnsupported`, per-frame forced
  IDR supported.
- One encode candidate and one decode candidate added, additively.
- Software round-trip test passes with small dimensions; CPU-cost note in the PR.
- `hang::catalog::VideoConfig` output, no `config.rs` mirror in contributed code.
