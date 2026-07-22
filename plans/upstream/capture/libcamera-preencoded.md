# libcamera-preencoded. Pre-encoded source concept and on-device H.264 libcamera capture

> Campaign: upstream | Kind: leaf plan | Branch: up/libcamera-preencoded |
> PR target: base branch, then moq main | Read ../0-overview.md first.

Status: REQUIRED deliverable; libcamera on-device H.264 through moq is a committed outcome
Depends on: the `publish_preencoded` API shape (open question, coordination point 5); independent of the base API contract
Path: independent
Size: M

## Goal

Give moq a capture source that emits an already-encoded bitstream and bypasses
the software encoder entirely, then implement the Raspberry Pi on-device H.264
path on top of it. This is moq-changes change 12, and it
is a required deliverable of this series: iroh-live's Raspberry Pi libcamera
on-device H.264 path working well through moq is a committed outcome, not an
opportunistic maybe. It has two parts, in order: first a `publish_preencoded`
sibling of `publish_capture` that takes a source yielding H.264 (or H.265) access
units plus a hang catalog config and drives them straight to the track, and
second the concrete implementation, a libcamera backend that runs `rpicam-vid`'s
hardware H.264 encoder and emits Annex-B directly. Open question: the exact
shape of the `publish_preencoded` API, discussed under Target in moq below and
in `../0-overview.md` coordination point 5; current proposal: mirror
`publish_capture` minus `encode::Options`, taking a bitstream source plus a
catalog config. A source that bypasses the encoder is a pattern no existing moq
path uses, so that is a design conversation about the signature, not a gate on
whether libcamera support lands: the outcome is committed, and only the API
shape is negotiable.

## Why it matters

The target is the Raspberry Pi Zero 2 and similar CSI-camera SoCs. On those
devices the camera ISP feeds a hardware H.264 encoder over an internal DMA-BUF
path, and `rpicam-vid` exposes that pipeline. The board has no CPU headroom for a
software encode: our own backend documents that the pre-encoded path "avoids the
~10 MB/s raw-YUV pipe and redundant NV12 conversion, using rpicam-vid's DMABUF
zero-copy ISP->encoder path"
(`rusty-capture/src/platform/linux/libcamera_h264.rs:8-10`), and the comparison
records that on a Pi Zero 2 "this is the difference between working and not"
(`comparisons/capture.md:334-337`). moq's model always runs its own encoder
(`comparisons/maps/rusty-capture.md:431-432`, "its encoder always runs"), so on
this class of device moq cannot capture at all without this concept. It is also
"arguably the strongest form of no download available"
(`comparisons/capture.md:386-388`) because what crosses the process boundary is
already compressed, and it is called out as the strongest single upstream
candidate we have short of adding the concept
(`comparisons/capture.md:487-492, 523-525`). This is why libcamera support is a
required deliverable of the series rather than an opportunistic
one: on the Pi-class devices iroh-live targets, the pre-encoded path is
the only way to capture at all, so the outcome that it works well through moq is
committed, and the negotiable part is only the shape of the entry point that
carries it.

## Evidence

- The concept and its fit: `comparisons/moq-changes.md:499-519` (section 3 item
  7, "Our one genuinely additive concept is the pre-encoded source ... which
  needs a `publish_preencoded` sibling of `publish_capture` feeding
  `Producer::publish` directly. moq's `Producer::publish(Vec<Bytes>, Timestamp)`
  already accepts external Annex-B, so the concept fits; the change is a turnkey
  entry point plus buy-in that a source may bypass the encoder"). Change 12 in the
  sequenced list: `comparisons/moq-changes.md:664` ("Pre-encoded source +
  `publish_preencoded` ... Pi Zero on-device H.264 ... additive ... concept
  buy-in").
- The concept gate: overview coordination point 5
  (`plans/upstream/0-overview.md:279-282`) and Wave 3 placement
  (`../0-overview.md:250-251`).
- moq already has the primitive the concept rests on, verified against HEAD
  3a3e0ea8:
  - `rs/moq-video/src/encode/producer.rs:85-105` `Producer::publish(packets:
    Vec<bytes::Bytes>, timestamp: Timestamp)`, doc "Publish already-encoded
    packets at the given timestamp. Each packet is one whole access unit in the
    producer's codec framing." This already runs external Annex-B through
    `split.decode`/`split.flush`/`import.decode`.
  - `rs/moq-video/src/encode/producer.rs:51-83` `Producer::new(broadcast,
    catalog, codec)` and `demand()`, the track and catalog reservation.
  - `rs/moq-video/src/encode/producer.rs:183-215` `publish_capture`, the
    demand-driven capture-and-encode loop the new entry point mirrors without the
    encoder.
- Our backend: `comparisons/maps/rusty-capture.md:236-248` (libcamera H.264, the
  only encoded backend and the only `PreEncodedVideoSource`) and
  `comparisons/maps/rusty-capture.md:57-78, 286-309` (the trait split).

## The `publish_preencoded` API shape (agree the signature first, implement second)

### What `publish_preencoded` looks like in moq terms

moq's `Producer` already accepts external encoded access units:
`publish(Vec<Bytes>, Timestamp)` (`producer.rs:87`) splits each Annex-B access
unit and registers the catalog rendition from the parsed SPS, exactly as
`publish_capture` does after its encoder. The only thing missing is a turnkey
entry point that pairs a source yielding those access units with a catalog config
and drives the same demand-gated loop as `publish_capture` but with no encoder in
the middle. In moq's vocabulary:

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

- A `publish_preencoded` entry point next to `publish_capture`
  (`producer.rs:183`) that advertises the track up front from `source.config()`,
  opens the source only while a subscriber watches and releases it on
  `demand.unused()`, forces an IDR-aligned restart on each reopen, and forwards
  each read access unit to `Producer::publish`. It is `publish_capture` with the
  encoder step deleted and the catalog config taken from the source rather than
  synthesized from the first encoded SPS. Its concrete signature mirrors
  `publish_capture` (verified async form at `producer.rs:183-215`) with the
  encoder inputs removed:

  ```rust
  pub async fn publish_preencoded(
      broadcast: moq_net::broadcast::Producer,
      catalog: moq_mux::catalog::Producer,
      source: impl PreEncoded,
      clock: moq_mux::Clock,
  ) -> Result<(), Error>;
  ```

  The `encode::Options` argument that `publish_capture` carries
  (`producer.rs:186`) is dropped, not merely defaulted: with no encoder there is
  nothing for it to configure. Everything it would supply now comes from
  `source.config()`, which returns a `hang::catalog::VideoConfig` carrying the
  codec, dimensions, bitrate, and latency hints, and `Producer::new` takes the
  codec from that config (`producer.rs:51-83`) rather than from `Options::codec`.
  The `clock` stays because `Producer::publish` still needs a `Timestamp` and the
  source may pace off the clock the same way `publish_capture` does.

The key open design questions, which shape the API rather than
decide whether libcamera support ships:

1. What is the right shape for an encoder-bypassing source in moq's model, given
   its posture that the encoder always runs? The honest framing is that
   `Producer::publish` already accepts external Annex-B, so the wire and catalog
   machinery exists; the ask is the turnkey source-side loop and where an
   encoder-bypassing source sits as a first-class entry point. If upstream
   prefers not to add it to moq, the same libcamera source lives in iroh-live over
   `Producer::publish` directly, so the deliverable holds either
   way; this question decides where the entry point lives, not whether the
   Pi path works.
2. Where does the catalog config come from? Our source supplies a full
   `VideoConfig` up front (profile, constraints, level, dimensions, bitrate,
   `optimize_for_latency`), so the track can be advertised before the first
   keyframe. moq's `publish_capture` instead waits for one encoded SPS to register
   the rendition (`producer.rs`, the catalog reservation path). The proposal is to
   let a pre-encoded source advertise from its declared config and refine from the
   first keyframe's SPS, which is the same refinement `import.decode` already does.
3. Codec scope: H.264 first (what `rpicam-vid` emits), with H.265 following the
   same shape since `Producer` already has an `H265` codec arm
   (`producer.rs:97-104`).

Agree the `publish_preencoded` signature upstream before implementing
the moq-side entry point past step 1's synthetic-source proof. This is an
API-shape conversation, not a go/no-go on the deliverable: libcamera on-device
H.264 working well through moq is required. If
upstream prefers the entry point not live in moq, the fallback keeps the
libcamera H.264 source in iroh-live over moq's `Producer::publish` directly, which
needs no moq change and still delivers the committed outcome; in that case the
leaf closes as "kept local," with the capability delivered rather than dropped.

## Source to port (after the gate)

`rusty-capture/src/platform/linux/libcamera_h264.rs` (522 LOC) and, for the raw
companion, `libcamera.rs` (268 LOC). What carries over from the H.264 source:

- The subprocess spawn. `rpicam-vid --codec h264 --inline --flush --timeout 0
  --nopreview -o -` with width, height, framerate, bitrate, and intra interval
  (`libcamera_h264.rs:182-209`). `--inline` prepends SPS and PPS before every IDR;
  `--flush` gives low latency; the exclusive-camera-lock retry with exponential
  backoff (`libcamera_h264.rs:174-215`) handles the Pi's single-camera reopen
  race, which matters because demand-gated reopen is exactly when that race fires.
- The Annex-B framing. Read the bytestream in chunks, split into access units at
  VCL NAL boundaries (`find_first_au_end`), detect IDR via `contains_idr_nal`, and
  extract SPS/PPS from the first keyframe
  (`comparisons/maps/rusty-capture.md:236-248`). Under the concept, most of this
  moves into or beside moq's existing `split`/`import` path, which already parses
  SPS from the keyframe; port only what `Producer::publish` does not already do
  (the access-unit chunking from a byte stream, since our source reads a raw pipe
  rather than pre-split units).
- The catalog config. `video_config()` (`libcamera_h264.rs:82-103`) builds a
  `VideoConfig` with `VideoCodec::H264 { inline: true, profile: 0x42, constraints:
  0xE0, level: 0x1E }`, `optimize_for_latency: Some(true)`, dimensions, bitrate,
  and framerate. It already targets hang catalog types, so it maps to moq's
  `hang::catalog::VideoConfig` with little change (`comparisons/moq-changes.md`
  section 3 item 2, we drop our mirror and use hang types directly).

The raw companion (`libcamera.rs`) spawns `rpicam-vid --codec yuv420` and reads
exact-size I420 frames from the pipe (`libcamera.rs:213-252`); it is a plain
`FrameStream` producer with no new concept and can ship in the same PR or a
follow-up as an ordinary Linux camera backend feeding moq's encoder.

What is dropped:
- Our `PreEncodedVideoSource` trait glue and `VideoSource` facade
  (`comparisons/maps/rusty-capture.md:57-78`); moq gets the `PreEncoded` trait
  above instead.
- Our `Duration` timestamps and `EncodedFrame` type; the entry point carries
  `Timestamp` and `Bytes` to match `Producer::publish`.
- Our avcC/`description` synthesis where moq's `import` already produces it.

## Target in moq

- `rs/moq-video/src/capture/`: a new `preencoded` module holding the `PreEncoded`
  trait and a `libcamera_h264` backend (the ported `rpicam-vid` H.264 source), and
  optionally a `libcamera` raw backend feeding the ordinary encoder path.
- `rs/moq-video/src/encode/producer.rs`: a `publish_preencoded` function beside
  `publish_capture` (`producer.rs:183`), reusing `Producer::new`, `demand()`, and
  `publish` (`producer.rs:51-105`) with the encoder step removed and the catalog
  config taken from `source.config()`.
- Feature gate: a `libcamera` feature analogous to our own
  (`comparisons/maps/rusty-capture.md:353-356`), off by default, so hosts without
  `rpicam-vid` build and the backend degrades cleanly (it is a subprocess, so it
  fails at spawn with a clear error, `libcamera_h264.rs:213`).

## Implementation steps (after the gate)

1. Land the `PreEncoded` trait and `publish_preencoded` entry point with no
   backend, driving a synthetic in-test source through `Producer::publish`, to
   prove the concept end to end on a runner with no hardware.
2. Port the `rpicam-vid` H.264 source: spawn, access-unit chunking, IDR
   detection, and `config()` returning `hang::catalog::VideoConfig` from
   `video_config()` (`libcamera_h264.rs:82-103`).
3. Port the exclusive-lock retry with backoff (`libcamera_h264.rs:174-215`) so a
   demand-driven reopen does not fail on the dying process's camera lock.
4. Wire the demand-gated open/release loop by mirroring `publish_capture`
   (`producer.rs:183-215`), forcing an IDR-aligned restart on reopen (with
   `--inline`, every IDR already carries SPS/PPS, so a reopen is decodable at
   once).
5. Optionally port the raw `libcamera` YUV backend as an ordinary encoder-fed
   Linux camera source (`libcamera.rs:213-252`).
6. Gate everything on the `libcamera` feature; ensure a non-Pi Linux host builds
   and the backend returns a clean spawn error rather than failing to link.

## Tests

- A concept test with a synthetic `PreEncoded` source (a canned Annex-B clip)
  driven through `publish_preencoded`, asserting the track is advertised from
  `config()`, the catalog rendition registers from the first keyframe, and the
  published access units reach a subscriber. Runs in CI, no hardware.
- A `rpicam-vid` round-trip test marked `#[ignore]` with a stated reason (needs a
  Raspberry Pi with a CSI camera): open the source, read access units, assert the
  first is an IDR with in-band SPS/PPS and that `config()` matches the negotiated
  stream. Confirm on named hardware in the PR body; CI cannot run it.
- An access-unit chunking unit test over a fixture bytestream (IDR and non-IDR
  boundaries), needing no hardware.

## Adaptation notes

- No ffmpeg: the source is a subprocess emitting Annex-B; moq's `split`/`import`
  handle the framing. No demuxer dependency enters moq.
- Minimal dependencies: `rpicam-vid` is an external binary, not a linked library,
  so there is no dlopen or link concern; the backend degrades to a spawn error on
  hosts without it.
- Timestamps: `moq_net::Timestamp` at the entry-point boundary. Our source
  synthesizes PTS from a frame counter and framerate
  (`libcamera_h264.rs`/`libcamera.rs:235`); convert that to `Timestamp` (or stamp
  from `moq_mux::Clock` as `publish_capture` does) rather than carrying
  `Duration`.
- Config: use `hang::catalog::VideoConfig` directly; drop our `config.rs` mirror.
- Errors: moq's `Error` with an additive variant if a spawn or framing failure is
  new; B5 owns the additive variant set.

## Coordination

- Coordination point 5 (the `publish_preencoded` API shape). This is the one open
  question, and it is about the signature, not about whether libcamera support
  ships. Current proposal: mirror `publish_capture` minus `encode::Options`,
  taking a bitstream source plus a catalog config. Agree the exact shape of the
  entry point upstream before implementing the moq-side loop past step 1's
  synthetic-source proof. libcamera on-device H.264 working well through moq is
  a required deliverable; the backend is the motivating user, and the API shape
  is the ask.
- Independent of the base API contract: this leaf touches no `Native`/`Frame`
  GPU vocabulary and adds no candidate table entry, so B1 through B4 and
  coordination points 1 and 2 do not apply. Its only open point is point 5.
- If upstream prefers the entry point not live in moq, fall back to keeping
  `LibcameraH264Source` in iroh-live over moq's existing `Producer::publish`,
  needing no moq change; the committed outcome still holds.

## Acceptance checklist

- The `publish_preencoded` API shape is agreed upstream before
  implementation past the synthetic-source proof (coordination point 5 satisfied
  and recorded in the PR), with libcamera support treated as required regardless
  of where the entry point lives.
- `publish_preencoded` advertises the track from `source.config()`, registers the
  rendition from the first keyframe, and forwards access units to
  `Producer::publish` under demand gating.
- The `rpicam-vid` H.264 source builds behind the `libcamera` feature and
  degrades to a clean spawn error on hosts without `rpicam-vid`.
- The synthetic-source concept test and the chunking unit test pass in CI; the Pi
  hardware round-trip test exists, is `#[ignore]`d with a stated reason, and is
  confirmed on named hardware in the PR.
- `cargo clippy` clean; hang catalog types used directly; no ffmpeg; no
  `Duration` at a boundary.
