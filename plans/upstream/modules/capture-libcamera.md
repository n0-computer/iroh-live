# libcamera capture (raw YUV and on-device H.264 pre-encoded)

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: the pre-encoded H.264 path carries an already-compressed bitstream,
> so no raw frame and no GPU surface ever crosses the process boundary; that is
> its own kind of no-download path. The raw path is CPU YUV420. See
> ../zerocopy.md, section 2a.

## What it is

Raspberry Pi and CSI-camera capture through `rpicam-vid`, in two forms. The raw
form (`libcamera`) spawns `rpicam-vid --codec yuv420` and reads I420 frames for
moq's ordinary encoder. The pre-encoded form (`libcamera_h264`) spawns
`rpicam-vid --codec h264` so the Pi's hardware H.264 encoder runs on the device
and the source emits an already-encoded Annex-B bitstream, bypassing the software
encoder entirely. The verdict is move-ours because both backends are absent
upstream, and the pre-encoded source is unique on either side and a required
outcome of this series: on a Pi Zero 2 it is the difference between working and
not.

## iroh-live vs moq

Ours has two libcamera backends, a deliberate fork. `LibcameraCapturer`
(`rusty-capture/src/platform/linux/libcamera.rs:128`) spawns
`rpicam-vid --codec yuv420`, reads `w*h*3/2`-byte I420 frames via `read_exact`
from stdout, slices into y/u/v, and emits `VideoFrame::new_i420`
(`libcamera.rs:213-252`), synthesizing PTS from a frame counter and configured
fps (`libcamera.rs:235`). It is a plain pull-based `VideoSource`.

`LibcameraH264Source` (`rusty-capture/src/platform/linux/libcamera_h264.rs:116`,
522 lines) is the only encoded backend and the only `PreEncodedVideoSource` on
either side. It spawns `rpicam-vid --codec h264 --inline --flush` running the
Pi's hardware encoder using the ISP-to-encoder DMABUF path internally
(`libcamera_h264.rs:8-10`), reads the Annex-B bytestream in 32 KB chunks, splits
into access units by scanning VCL NAL boundaries (`find_first_au_end`,
`:406-440`), detects IDR via `contains_idr_nal` (`:376-398`), extracts SPS/PPS
from the first keyframe into an avcC description for the catalog (`:326-336`), and
yields `EncodedFrame { is_keyframe, timestamp, payload }` (`:351-355`).
`config()` returns a `VideoConfig` with an H.264 codec (baseline, profile 0x42,
constraints 0xE0, level 0x1E, `inline: true`, `optimize_for_latency: Some(true)`,
`:82-103`), and `start()` retries the Pi's exclusive-camera lock with exponential
backoff (`:174-213, 174-269`).

moq has neither backend. Its model always runs its own encoder, so on a Pi-class
device it cannot capture at all without a pre-encoded-source concept. moq does
have the primitive the concept rests on: `Producer::publish(Vec<Bytes>,
Timestamp)` (`rs/moq-video/src/encode/producer.rs:85-105`) already accepts
external whole-access-unit Annex-B and runs it through
`split.decode`/`split.flush`/`import.decode`. What is missing is a turnkey entry
point that pairs an encoder-bypassing source with a catalog config and drives the
same demand-gated loop as `publish_capture` (`producer.rs:183-215`) with no
encoder in the middle.

## Zero-copy

The pre-encoded path is a distinct kind of no-download path. On a Pi Zero 2 the
camera ISP feeds a hardware H.264 encoder over an internal DMA-BUF path and
`rpicam-vid` exposes that pipeline; the zero-copy happens inside the device, and
what crosses the process boundary is already compressed H.264. There is no raw
frame and no GPU surface to import, so the frame vocabulary does not apply. The
comparison calls this "arguably the strongest form of no download available"
(../zerocopy.md, section 2a) because the download question never arises. Our own
backend documents that the pre-encoded path "avoids the ~10 MB/s raw-YUV pipe and
redundant NV12 conversion, using rpicam-vid's DMABUF zero-copy ISP->encoder path"
(`libcamera_h264.rs:8-10`). The raw `libcamera` companion is a plain CPU YUV420
pipe with no zero-copy path.

## What to do

MOVE both backends into moq, in two ordered parts: the `publish_preencoded`
concept first, then the concrete libcamera implementation. This is a REQUIRED
deliverable of the series; the libcamera on-device H.264 path working well
through moq is committed, and only the API shape is negotiable.

Open question: the exact shape of the `publish_preencoded` entry point (base
coordination point 5). Current proposal: mirror `publish_capture` minus
`encode::Options`, taking a bitstream source plus a catalog config. In moq terms:

- A small source trait, the moq analog of our `PreEncodedVideoSource`
  (`rusty-codecs/src/traits.rs:268-287`), living in moq-video's capture layer:

  ```rust
  /// A capture source that emits an already-encoded bitstream, bypassing the
  /// software encoder. Implemented by devices whose ISP or hardware encoder
  /// produces the codec's wire framing directly (rpicam-vid on Raspberry Pi,
  /// hardware RTSP cameras, file demuxers).
  pub(crate) trait PreEncoded: Send {
      /// The codec configuration for the catalog rendition.
      fn config(&self) -> hang::catalog::VideoConfig;
      /// The next access unit and its presentation timestamp, or `None` at
      /// end of stream. One whole access unit per call, in the codec's framing.
      fn read(&mut self) -> Result<Option<(Bytes, Timestamp)>, Error>;
  }
  ```

- A `publish_preencoded` entry point beside `publish_capture`
  (`producer.rs:183`) that advertises the track up front from `source.config()`,
  opens the source only while a subscriber watches and releases it on
  `demand.unused()`, forces an IDR-aligned restart on each reopen, and forwards
  each read access unit to `Producer::publish`. It is `publish_capture` with the
  encoder step deleted and the catalog config taken from the source rather than
  synthesized from the first encoded SPS. Proposed signature:

  ```rust
  pub async fn publish_preencoded(
      broadcast: moq_net::broadcast::Producer,
      catalog: moq_mux::catalog::Producer,
      source: impl PreEncoded,
      clock: moq_mux::Clock,
  ) -> Result<(), Error>;
  ```

  The `encode::Options` argument `publish_capture` carries (`producer.rs:186`) is
  dropped, not defaulted: with no encoder there is nothing for it to configure.
  Everything it would supply now comes from `source.config()`, and `Producer::new`
  takes the codec from that config (`producer.rs:51-83`). The `clock` stays
  because `Producer::publish` still needs a `Timestamp`.

The open design questions shape the API, not whether libcamera ships: (1) the
right shape for an encoder-bypassing source given moq's posture that the encoder
always runs, and where that entry point lives; (2) where the catalog config comes
from (proposal: advertise from the declared config and refine from the first
keyframe's SPS, the same refinement `import.decode` already does, rather than
moq's current wait-for-first-SPS registration); (3) codec scope, H.264 first with
H.265 following the same shape since `Producer` already has an `H265` arm
(`producer.rs:97-104`). If upstream prefers the entry point not live in moq, the
fallback keeps `LibcameraH264Source` in iroh-live over moq's existing
`Producer::publish` directly, needing no moq change, and the committed outcome
still holds.

Source to port after the gate, from
`rusty-capture/src/platform/linux/libcamera_h264.rs`:

- The subprocess spawn. `rpicam-vid --codec h264 --inline --flush --timeout 0
  --nopreview -o -` with width, height, framerate, bitrate, and intra interval
  (`libcamera_h264.rs:182-209`). `--inline` prepends SPS and PPS before every
  IDR; `--flush` gives low latency; the exclusive-camera-lock retry with
  exponential backoff (`:174-215`) handles the Pi's single-camera reopen race,
  which matters because demand-gated reopen is exactly when that race fires.
- The Annex-B framing. Read the bytestream in chunks, split into access units at
  VCL NAL boundaries (`find_first_au_end`), detect IDR (`contains_idr_nal`), and
  extract SPS/PPS from the first keyframe. Under the concept, most of this moves
  into or beside moq's existing `split`/`import` path, which already parses SPS
  from the keyframe; port only the access-unit chunking from a byte stream, since
  our source reads a raw pipe rather than pre-split units.
- The catalog config. `video_config()` (`libcamera_h264.rs:82-103`) already
  targets hang catalog types, so it maps to `hang::catalog::VideoConfig` with
  little change.

The raw companion `libcamera.rs` spawns `rpicam-vid --codec yuv420` and reads
exact-size I420 frames from the pipe (`libcamera.rs:213-252`); it is a plain
`FrameStream` producer with no new concept and can ship in the same PR or a
follow-up as an ordinary Linux camera backend feeding moq's encoder.

What is dropped in the port: our `PreEncodedVideoSource` trait glue and
`VideoSource` facade; our `Duration` timestamps and `EncodedFrame` type (the
entry point carries `Timestamp` and `Bytes` to match `Producer::publish`); our
avcC/`description` synthesis where moq's `import` already produces it.

Target in moq: a new `preencoded` module in `rs/moq-video/src/capture/` holding
the `PreEncoded` trait and a `libcamera_h264` backend (and optionally a
`libcamera` raw backend feeding the ordinary encoder path); a `publish_preencoded`
function beside `publish_capture` in `rs/moq-video/src/encode/producer.rs`; and a
`libcamera` feature, off by default, so hosts without `rpicam-vid` build and the
backend degrades to a clean spawn error (it is a subprocess, so it fails at spawn,
`libcamera_h264.rs:213`).

Implementation steps after the gate: (1) land the `PreEncoded` trait and
`publish_preencoded` with no backend, driving a synthetic in-test source through
`Producer::publish` to prove the concept end to end on a runner with no hardware;
(2) port the `rpicam-vid` H.264 source; (3) port the exclusive-lock retry with
backoff; (4) wire the demand-gated open/release loop mirroring `publish_capture`,
forcing an IDR-aligned restart on reopen (with `--inline`, every IDR carries
SPS/PPS, so a reopen is decodable at once); (5) optionally port the raw YUV
backend; (6) gate everything on the `libcamera` feature.

The iroh-live removal side: `libcamera_h264.rs` (522 LOC) and `libcamera.rs`
(268 LOC), both disposition upstream-ours, are deleted only after the upstream
contribution merges and releases, on the paired `up/libcamera-preencoded`
branch. If the entry point stays local per the fallback, the leaf closes as "kept
local" with the capability delivered rather than dropped.

## Tests

- A concept test with a synthetic `PreEncoded` source (a canned Annex-B clip)
  driven through `publish_preencoded`, asserting the track is advertised from
  `config()`, the catalog rendition registers from the first keyframe, and the
  published access units reach a subscriber. Runs in CI, no hardware.
- A `rpicam-vid` round-trip test marked `#[ignore]` with a stated reason (needs a
  Raspberry Pi with a CSI camera): open the source, read access units, assert the
  first is an IDR with in-band SPS/PPS and that `config()` matches the negotiated
  stream. Confirm on named hardware in the PR; CI cannot run it.
- An access-unit chunking unit test over a fixture bytestream (IDR and non-IDR
  boundaries), needing no hardware.

## Evidence

- Verdict: ../comparison/capture.md (Linux camera libcamera raw row, "268 lines,
  rpicam-vid I420 pipe, absent" upstream; Linux camera libcamera H.264 row, "522
  lines, on-device HW encode, absent, ours only unique"; the "backends only we
  have" detail at `capture.md:328-339`; the Pi Zero 2 "difference between working
  and not" at `capture.md:334-337`; the section 5 verdict at `capture.md:490-494`
  naming the pre-encoded source the strongest upstream candidate).
- Concept fit: ../comparison/moq-changes.md section 3 item 7 and change 12 (the
  pre-encoded source needs a `publish_preencoded` sibling feeding
  `Producer::publish`; moq already accepts external Annex-B, so the concept
  fits).
- Code map: ../comparison/maps/rusty-capture.md (libcamera H.264 backend detail
  at `libcamera_h264.rs:116, 182-213, 326-336, 351-355`; the raw/encoded trait
  split at `:57-78, 286-309`; moq "its encoder always runs" at `:431-432`).
- Zero-copy: ../zerocopy.md, section 2a (Pi on-device encode is arguably the
  strongest form of no download, `capture.md:386-388`).

## Coordination

- Coordination point 5 (the `publish_preencoded` API shape). This is the one open
  question, about the signature, not whether libcamera support ships. Agree the
  exact shape upstream before implementing the moq-side loop past step 1's
  synthetic-source proof.
- Independent of the base API contract: this leaf touches no `Native`/`Frame` GPU
  vocabulary and adds no candidate-table entry, so ../base/B1-frame-vocabulary.md
  through B4 and the shared-table point do not apply. The pre-encoded bitstream
  path never constructs a raw frame.
- Fallback: if upstream prefers the entry point not live in moq, keep
  `LibcameraH264Source` in iroh-live over `Producer::publish` directly, needing
  no moq change; the committed outcome still holds and the leaf closes as kept
  local.
- Adaptation constraints: no ffmpeg (the source is a subprocess emitting Annex-B;
  moq's `split`/`import` handle framing, no demuxer dependency enters moq);
  `rpicam-vid` is an external binary, not a linked library, so no dlopen or link
  concern; `moq_net::Timestamp` at the boundary, converting our
  frame-counter-plus-framerate PTS or stamping from `moq_mux::Clock`, not
  carrying `Duration`; `hang::catalog::VideoConfig` used directly, dropping our
  `config.rs` mirror; errors use moq's `Error` with an additive variant per B5 if
  a spawn or framing failure is new.
- Release gate: the local modules are cut only when the release carrying the leaf
  is pinned, on the paired `up/libcamera-preencoded` branch.
