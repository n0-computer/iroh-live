# sync-adaptive-align

Branch: align/sync-adaptive-align          Wave: 1
Depends on: Wave 0 pin bump (moq release cut from main carrying moq-mux 0.7.x with
the per-rendition `Estimate` metrics that populate catalog `jitter` and
`bitrate`). Independent of every other task.
Kind: independent

## Goal
This task aligns iroh-live's two subscriber-side gap-filler layers, the playout
clock (`moq-media/src/sync.rs`) and the adaptive ABR (`moq-media/src/adaptive.rs`),
to moq-mux's on-main per-rendition `Estimate { jitter, bitrate }`. A subscriber can
now read publisher-measured jitter and bitrate straight from the catalog, because
moq-mux auto-detects both per rendition from the frame stream and writes them into
the hang `VideoConfig` / `AudioConfig` `jitter` and `bitrate` fields. The playout
clock reads catalog `jitter` to seed its per-codec latency terms rather than
leaving them dead, and the ABR selects on honest measured bitrate rather than
asserted static preset values. Both layers stay in iroh-live: they are Rust-side
gaps with no moq equivalent (moq Rust has no subscriber-side rendition selection
and no playout clock), and they are separate upstream candidates tracked under
`plans/upstream/`, so this is an alignment and wiring task that reads and uses the
catalog `Estimate`, not a deletion. Direct deletion here is small, on the order of
a few stale doc lines, so the proof is a proof-of-behavior rather than a
proof-before-deletion. The deeper upstreaming of adaptive and sync into moq is out
of scope.

## Evidence
- `../upstream/comparisons/pubsub.md` is the anchor. Section 2 (publish path)
  describes moq-mux's per-rendition `Estimate` and `record_frame` metrics that fill
  catalog `jitter` and `bitrate`. Section 5 (adaptive) establishes that no
  subscriber-side rendition selection exists in moq Rust, that `adaptive.rs` is
  strictly richer than the JS `recvBandwidth * 0.8` heuristic, and that the layer
  stays in iroh-live and is a separate upstream candidate. Section 6 (sync)
  establishes that no Rust playout clock exists upstream, that the catalog `jitter`
  field is present (correcting the stale claim in `sync.rs`), that the producing
  half is now closed by moq-mux metrics, and that the reading half is ours to wire.
  Section 10 gives the per-area verdicts: keep both layers, then upstream
  separately; the local fixes now are to correct the stale `sync.rs` doc and read
  catalog `jitter` into the clock.
- `../upstream/comparisons/maps/moq-media.md` for the moq-media module map, and
  the stale "our Rust catalog does not carry this field yet" claim it repeats.
- `cut-plan.md` for the keep verdict on `sync.rs`, `playout.rs`, and `adaptive.rs`.

## moq primitive adopted
moq-mux's per-rendition catalog `Estimate` and the catalog fields it populates.
- `Estimate { jitter: Option<Duration>, bitrate: Option<u64> }`
  (`rs/moq-mux/src/catalog/tracks.rs:16-20`) is the set of auto-detectable catalog
  fields.
- `Rendition::record_frame(ts, bytes)`, `record_reorder`, and `record_group_end`
  (`tracks.rs:390-410`) feed the per-rendition metrics detector that auto-fills
  `jitter` (from frames and reorder) and `bitrate` (over a one-second group
  window), only for fields the config left absent; `Rendition::set`
  (`tracks.rs:345`) captures a supplied `Estimate` as authoritative and never
  overwrites it.
- The values land in the hang catalog `jitter: Option<Duration>` and
  `bitrate: Option<u64>` fields on both `VideoConfig` and `AudioConfig`
  (`rs/hang/src/catalog/video/mod.rs`, `audio/mod.rs`), serialized as integer
  milliseconds for jitter. This is a read-only adoption on the subscribe side:
  nothing about the moq API changes, iroh-live simply starts reading fields it
  ignored.

## iroh-live code changed
`moq-media/src/sync.rs` (420 LOC) is the primary edit target for the seeding.
- The module doc claims "Our Rust catalog does not carry this field yet"
  (sync.rs:18-22) and the field docs repeat it (sync.rs:99-106); these are stale
  per `pubsub.md` section 4 and section 6 and must be corrected.
- `audio_ms: Option<i64>` (sync.rs:102) and `video_ms: Option<i64>`
  (sync.rs:104-106) are currently dead, always `None` (treated as 0). They must be
  seeded from the catalog `jitter` fields when a moq-mux-based publisher populates
  them. `latency_ms = max(audio_ms, video_ms) + jitter_ms` (sync.rs:108) then
  reflects the publisher-measured bound, feeding `wait(pts)` (sync.rs:187-224,
  per the grep the `wait` and `received` methods gate the decode OS threads). The
  seed tightens rather than replaces the receiver-side clock, because the estimate
  is a publish-side jitter bound, not a full receiver jitter buffer (section 6).
- `moq-media/src/playout.rs` (92 LOC) is the policy layer (`SyncMode`,
  `PlaybackPolicy { sync, max_latency }`); it is reviewed for where the seeded
  clock is constructed but changes little.
`moq-media/src/adaptive.rs` (592 LOC including tests) is the ABR alignment target.
- `rank_renditions` reads `config.bitrate.unwrap_or(0)` (adaptive.rs:93-102) into
  `RankedRendition.bitrate_bps` (adaptive.rs:80-86). Once moq-mux populates
  catalog `bitrate` with a measured value, this input becomes honest with no
  code change, but the `unwrap_or(0)` fallback path and the `bitrate_bps == 0`
  guards in `evaluate` (adaptive.rs:171, :207) should be reviewed so a
  measured-but-low bitrate and an absent bitrate stay distinguishable.
- `evaluate` (adaptive.rs:150-157) and `should_abort_probe` (adaptive.rs:226) are
  pure decision logic and stay unchanged in shape; the alignment is that their
  bitrate input is now publisher-measured. The runtime driver
  `adaptation_task_v2` (`moq-media/src/subscribe.rs:1293-1493`) and
  `switch_rendition_v2` (subscribe.rs:1497-1533) are not restructured here.
The subscribe-side read plumbing that surfaces catalog `jitter` and `bitrate` from
`CatalogSnapshot` (`moq-media/src/subscribe.rs:250-256`) into the sync clock and
the ABR ranking is the connective code this task adds.

## Steps
Each step is small enough to commit; this is wiring, so the order is read-side
plumbing, then seed, then the ABR review, then tests and doc.

1. Correct the stale documentation. Update the `sync.rs` module doc (sync.rs:18-22)
   and the `audio_ms` / `video_ms` field docs (sync.rs:99-106) to state that the
   catalog `jitter` field exists and is populated by moq-mux's per-rendition
   metrics, and that the reading half is wired here. Do the same for any repeat of
   the stale claim in `maps/moq-media.md` if a note belongs there. This step is a
   doc-only change and must be bundled with the code steps below, not committed on
   its own.

2. Surface catalog `jitter` and `bitrate` on the subscribe read path. Ensure the
   `CatalogSnapshot` and per-rendition read helpers expose the hang
   `VideoConfig` / `AudioConfig` `jitter: Option<Duration>` and
   `bitrate: Option<u64>` fields to the sync clock and the ABR ranking, honoring
   the `Option` (a `None` means the publisher did not measure it, for example a
   non-moq-mux publisher).

3. Seed the playout clock from catalog `jitter`. Wire the video rendition's catalog
   `jitter` into `video_ms` and the audio rendition's into `audio_ms`, converting
   the serialized millisecond `Duration` to the clock's `i64` ms, so
   `latency_ms = max(audio_ms, video_ms) + jitter_ms` uses the publisher-measured
   bound when present and falls back to today's behavior (both `None`, treated as
   0) when absent. Keep the seed as a tightening input to the receiver-side clock,
   not a replacement for `sync.rs` (section 6): audio remains the effective master
   via its sink ring buffer, and the video receive path still updates `reference`.

4. Confirm the ABR reads measured bitrate. With catalog `bitrate` now populated,
   verify `rank_renditions` (adaptive.rs:93-102) consumes the measured value, and
   review the `bitrate_bps == 0` guard paths in `evaluate` (adaptive.rs:171, :207)
   so absent and measured-low bitrates remain distinguishable. No change to the
   decision policy itself.

5. Add proof-of-behavior tests (see below).

## Proof before deletion
Because this task deletes little, the gate is proof-of-behavior rather than
proof-before-deletion (coordination point 1 applies only to the stale doc lines).
Add tests, and an end-to-end check, that:
- a subscriber reading a catalog whose renditions carry moq-mux-populated `jitter`
  seeds `sync.rs` `audio_ms` / `video_ms` accordingly, and the computed
  `latency_ms` reflects `max(audio_ms, video_ms) + jitter_ms` rather than the
  jitter-only default;
- a catalog with absent `jitter` (a non-moq-mux publisher, both fields `None`)
  reproduces today's behavior exactly, so the seed is purely additive;
- `rank_renditions` orders renditions by measured `bitrate` when present, and the
  existing `adaptive.rs` unit tests (which construct `VideoConfig` values, per the
  `hang::catalog::{H264, VideoCodec}` use at adaptive.rs:238) still pass;
- an existing publish-then-subscribe example continues to render, confirming the
  seeded clock does not regress playout timing.

## Coordination
- Depends only on the Wave 0 pin bump for the moq-mux release that populates
  catalog `jitter` and `bitrate`; independent of every other task, so it runs
  freely within Wave 1.
- Out of scope, deferred to `plans/upstream/`: the deeper upstreaming of
  `adaptive.rs` (the pure policy toward moq-mux's catalog module next to `Select`,
  and the switching driver as a `moq_video::decode` switcher) and of `sync.rs`
  plus `playout.rs` (toward moq-mux next to `container::Consumer`). This task only
  reads and uses the catalog `Estimate`; it neither moves these layers into moq nor
  deletes them (`pubsub.md` sections 5, 6, and 10).
- Related enablers noted in `pubsub.md` but not part of this task:
  `container::Consumer::set_latency` for mid-stream `PlayoutMode` retuning
  (`pubsub.md` section 3) and consuming `moq_net::bandwidth::recv_bandwidth` in
  place of the cwnd `available_bps` math (`pubsub.md` section 7) are separate
  follow-ups.

## Acceptance checklist
- The stale "our Rust catalog does not carry this field yet" claims in `sync.rs`
  (and `maps/moq-media.md` if noted) are corrected, bundled with the code change.
- The subscribe read path exposes catalog `jitter` and `bitrate` to the clock and
  the ABR, honoring the `Option`.
- `sync.rs` `audio_ms` / `video_ms` are seeded from catalog `jitter` when present,
  and fall back to today's behavior when absent, with audio still the master and
  the video path still updating `reference`.
- `adaptive.rs` `rank_renditions` selects on measured `bitrate`, with the absent
  versus measured-low distinction preserved and the existing unit tests passing.
- Both layers remain in iroh-live; nothing is moved into moq and only stale doc
  lines are deleted.
- The proof-of-behavior tests pass, an end-to-end publish-subscribe example
  renders without a playout regression, and `cargo make check-all` passes.
