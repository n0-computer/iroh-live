# Worklog: moq-alignment refactor planning

## ROUND 3 (2026-07-22): upstream PR campaign planning (plans/upstream/)

Goal: full upstream of all iroh-live codec + capture code to moq (or
improve moq's existing code). Produce plans/upstream/ as a tree-shaped,
ordered, independently-executable plan set ready for a coordinated
multi-subagent session that opens a base PR series then a fan of leaf
PRs to moq. moq is one codebase now (HEAD 3a3e0ea8).

Git setup done first, per the user:
- .gitignore: removed /plans, kept /plans/old ignored.
- New branch plan-upstream; committed the current plan set (36 files,
  refactor/ + worklog, no plans/old) as f45b663 "docs: track refactor
  planning docs...". No push (not requested).

plans/upstream/ structure:
- 0-overview.md (authored by coordinator): goal, base-then-leaves
  strategy, the FROZEN BASE API CONTRACT (Native enum + DmaBuf accessor,
  Backend::encode timestamp + Packet, decode::Frame::native(),
  register_encoder/decoder), dependency tree, wave ordering, 6
  coordination points, the autonomous-agent runbook, plan template,
  git/PR model, and the plan index.
- comparisons/ (copied from refactor): codecs, capture, audio, zerocopy,
  traits-api, moq-changes, iroh-live-code-map, moq-inventory + maps/.
- base/ B1-B5, codec/ (vaapi-decode/encode, vtb-mf-decode-surface,
  v4l2-encode/decode, android, av1, opus, pcm, bitstream), capture/
  (pipewire-dmabuf, v4l2-camera-enum, libcamera-preencoded), render/
  (moq-video-render). ~19 leaf/base plans.

Base API contract frozen from moq-changes.md changes 1,2,3,7 (verified
against 3u sketches). Change map: 1=B1 (keystone), 2=B2 (PTS), 3=B3
(native accessor), 7=B4 (public Backend trait, breaking, Path B only),
9=B5 (conventions). Leaves = changes 4,5,6,8,10,11,12 + the codec
comparison backends (v4l2, android, av1).

Six plan-writing agents dispatched in parallel, each reading the
overview (contract+template) + its comparison sections + real source,
writing concrete executable plans. After they land: review set for
template/contract/dependency consistency, fix anchor mismatches, then
commit plans/upstream on plan-upstream.

All six done; 19 plans written, all anchors verified against source.
Notable source confirmations: our VAAPI decoder is H.264-only;
moq-vaapi is external crates.io 0.0.2 encode-only (so its growth is a
real version-bump, owned by vaapi-decode); neither our nor moq's Opus
decoder has a real PLC/decode_lost path (FEC/PLC framed as reservation);
moq's PipeWire capture is CPU-only (the gap pipewire-dmabuf fills); the
V4L2 and Android encoders are genuinely pipelined (bounded SyncSender,
immediate push_frame return, PTS on the driver buffer) = B2 justified.
Base agent flagged one contract-anchor asymmetry: decode SOFTWARE is a
single const, not a slice (encode has two slices) -> fixed the overview
prose at line 165; B4 already captured it. Consistency review dispatched
(contract adherence, dependency tree, the moq-vaapi/B4/render seams,
coordination declarations). Then fix + commit.

### Round 3 closing (2026-07-22)

Consistency review (review-consistency.md): verdict MOSTLY consistent,
ready after one substantive fix; 0 blocking, 2 substantive, 4 nits. The
frozen contract is quoted verbatim across all consumers, dependency
headers match the tree, the three hard seams (moq-vaapi ownership, B4/
Android, render deps) are declared without contradiction. All findings
survived the opposing stance; all fixed inline by the coordinator:
- av1-software invented a false breaking-change gate (claimed the encode
  Codec enum is not #[non_exhaustive]; source encoder.rs:22 IS, so
  adding Av1 is additive). Removed the false gate; replaced with the
  real coordination point 4 (rav1d fork). This mattered: it would have
  sent a PR agent to an unnecessary maintainer stop.
- B1 left the DmaBuf feature-gate undecided (vaapi vs dmabuf). Committed
  to a shared `dmabuf` Cargo feature that vaapi/pipewire/v4l2 each
  enable, so DMA-BUF producers pull the variant without depending on
  vaapi. Updated B1 (4 sites) + the overview contract.
- Nits fixed: render into_i420 signature (consumes self, not &self;
  noted the ownership implication for the fallback), v4l2-decode
  candidate-table anchor (HARDWARE :89-107 not :89-114), overview
  v4l2-camera-enum index row qualifier.

Deliverable complete: plans/upstream/ is a self-contained campaign kit
(overview + frozen contract + comparisons + 19 base/leaf plans + the
consistency review) ready to hand to a coordinated multi-subagent
session that opens the base PR series (B1 keystone first) then the fan
of leaf PRs to moq. Committed on plan-upstream. No push (not requested).

Wall-clock: git setup + overview authoring + 6 parallel plan agents +
consistency review + fixes. moq HEAD re-verified 3a3e0ea8 throughout.

### Round 3b (2026-07-22): review + comprehensiveness + transcode alignment

User asked to: review the plan/structure; make comparisons readable
(top matrix + inline links); ensure comprehensiveness (nothing lost vs
our status quo, no moq main feature regressed). Plus maintainer info to
avoid collision: "per segment transcoding with FETCH support is the
goal ... 1080p -> 360p for group 45 ... rate control needs to be pretty
custom", source = "relay memory, possibly disk".

Assessment: comparison docs have rich per-section tables but NO
top-level consolidated matrix/index (readability gap); the plans/
overview say nothing about transcode/FETCH/rate-control (collision-avoid
gap). Three agents dispatched:
- A: coverage audit (every iroh-live codec/capture feature + subtle
  optimization -> the plan that carries it; flag LOST/PARTIAL). ->
  review-coverage.md.
- B: moq no-regression (B2/B4/vaapi-encode/vtb-mf/opus are strictly
  additive, no moq feature removed) + per-segment-FETCH-transcoding
  alignment (our VAAPI decode+VPP+encode = Intel/AMD analog of
  NVDEC->NVENC for moq-transcode; rate control must expose per-segment
  primitives + cheap reconfigure, not a streaming controller; propose
  overview coordination point #7). -> review-transcode-alignment.md.
- C: comparisons/0-index.md = consolidated capability matrix with
  inline links to detailed sections + key findings.
Then integrate: fix coverage gaps, add coordination point #7 + per-plan
transcode/rate-control notes, land the index, re-verify, commit.

Agent results:
- Coverage (review-coverage.md): 28 COVERED, 19 dropped-by-decision,
  1 PARTIAL, 0 LOST. Comprehensive. PARTIAL = PipeWire portal camera
  capturer (screen-only plan drops it; capture.md verdict keeps camera).
- No-regression (review-transcode-alignment.md): PASS, no moq feature
  removed. Transcode-alignment: MOSTLY - our encoders plug into
  moq-transcode's public encode::{Kind,Config,Encoder} seam with zero
  integration (per-rung CBR via Config.bitrate, fresh encoder per group,
  never uses rate::Control). Correction: our decoders have reset() but
  our ENCODERS do not; per-group re-open cost rav1e<VAAPI<V4L2
  (expensive: device open+REQBUFS+STREAMON) -> VAAPI/V4L2 encode plans
  need a session-reuse note.
- Index (comparisons/0-index.md): 31-row consolidated capability matrix
  with inline links, all machine-verified to resolve.
Coordinator added overview coordination point #7 (per-segment
transcoding + FETCH) + scope note (audio device layer out of scope,
separate effort) + provenance pointer to 0-index.md. Dispatched a fixer
for the per-plan transcode notes + the PipeWire camera partial.

### Round 3c (2026-07-22, OVERNIGHT): migrate + delete refactor + full review

New user instruction (overnight discipline): ensure everything from
plans/refactor/ is preserved in plans/upstream/ (add if missing unless
obsolete), remove plans/refactor/, commit; then a fresh adversarial
review of the whole upstream plan cross-checked against source, adding
missing details, written to plans/upstream-review-0722-plan.md.

Migration plan (refactor/ is committed in f45b663, so anything not
migrated survives in git history):
- comparisons already hold 1,2,3,3t,3u,3z,4,6 + the 8 current maps.
- ADD comparisons/pubsub.md <- 5-compare-pubsub.
- NEW analysis/ <- refactor 0-overview, 7-cut-plan, 8-upstream-plan,
  9-room-layer, 10-summary (broader refactor synthesis beyond the
  codec/capture campaign) + an analysis/README explaining status.
- OBSOLETE, not migrated (in git history): the 6 stub maps
  (moq-main-media, moq-dev-*, moq-origin-hop), the 6 review artifacts
  (superseded process notes).
Then git rm -r plans/refactor; commit; then the full adversarial review.



## ROUND 2 (2026-07-21): dev merged into main; full rewrite

Trigger: moq merged dev into main (commit 3c22ecb8 "Merge
remote-tracking branch 'origin/dev'"). The whole main-vs-dev scaffolding
in these plans is now obsolete and must be removed. New scope from the
user:
- Rewrite everything in plans/refactor to current moq main; drop the
  dev-vs-main distinction entirely.
- Render: intend to upstream it too. Investigate upstreaming (render +
  capture zero-copy), or keep in-repo but fully aligned to the upstream
  model.
- Be MORE thorough in the interface/API comparison.
- Outline the concrete moq-side changes to upstream everything
  capture/codec, in sections: (1) zero-copy upstreaming (render AND
  capture); (2) a draft to open the closed codec Backend variants with a
  public trait like ours; (3) other differences. Investigate whether moq
  would accept external codecs at all.

Current moq state verified (2026-07-21T12:32):
- main HEAD 3a3e0ea8 (2026-07-21). dev now 45 commits BEHIND main, 0
  ahead: fully merged, effectively dead. Use moq main as the single
  source; pin maps to HEAD 3a3e0ea8.
- main now carries the full stack: moq-video (all HW backends encode +
  decode), moq-nvenc, moq-transcode, moq-stats, plus moq-hls/rtc/rtmp/
  srt/flate/wasm. Versions: moq-video 0.0.6, moq-audio 0.0.9, hang
  0.19.5, moq-net 0.1.18, moq-native 0.18.3, moq-mux 0.7.6, moq-nvenc
  0.0.1, moq-transcode 0.0.1, moq-stats 0.1.0.

Delta since the round-1 pin 261c2048 (23 commits in media/net crates):
- moq-video CORE CODEC API (encode/encoder, encode/backend/mod,
  decode/decoder, decode/backend/mod, frame, lib): ZERO diff. All the
  round-1 codec/zerocopy/trait analysis is still exact; only reframing
  dev->main is needed there.
- hang catalog: +19 lines, serde aliases for legacy displayRatio* ->
  displayAspect* (#2420). Immaterial to the analysis.
- moq-mux 0.5.5 -> 0.7.6: #2420 unseal catalog renditions + explicit
  shareable timelines, #2425 shared video-import catalog helper, #2426
  per-frame fragments, #2428 CMAF timelines. Affects pubsub/catalog doc.
- moq-net/stats (all breaking): #2396 route everything through
  create_broadcast + gate announce on Route.live; #2424 route by
  cumulative cost (lite-06); #2419 unannounce when last route detaches;
  #2414 empty PATH default ""; #2427 traffic counters in model layer;
  #2411 remove internal tier defaults. Affects rooms + pubsub/stats.
- moq-token #2416: scope signing keys to publish/subscribe paths.
  Directly relevant to the round-1 room spoofing gap (auth for
  announce-under-your-own-id). Fold into 9-room-layer.

Round-1 review artifacts (review-*.md) and the round-1 map filenames
carrying "dev" are being superseded. Round-1 findings on substance
(zero-copy complementarity, codec matrix, open-vs-closed traits) stand.

### Room-layer deltas from the refreshed net map (fold into 9-room rewrite)

- Publishing is now `create_broadcast(path, Route)`; `publish_broadcast`
  gone. `Route.announce` gates advertisement independently of existence
  (a non-announced broadcast is reachable by exact path, invisible to
  `announced()`). Cumulative `RouteCost` selection (#2424).
- #2419 REMOVED `ROUTE_LINGER` (5s grace). The round-1 "roaming for
  free via a lingering front" claim is now WRONG: last-route detach
  unannounces synchronously; a reconnecting peer re-announces. Update
  9-room and 10-summary accordingly.
- #2416 moq-token path-scoping CLOSES the round-1 announce-spoofing
  gap: a per-peer token rooted at <room>/<endpoint-id> with put:[""]
  makes the verifier scope the session origin to that prefix, so
  create_broadcast/announce outside the peer's own id fails
  Unauthorized; key-level Scope bounds even minting. Caveat: hard-wires
  "announce only your own id", conflicting with transitive relay
  re-announce unless relays get a broader scope. This removes the
  round-1 "signed KV state is a hard gossip advantage" point.
- moq-native iroh `accept` is single-phase now (two-phase authorize
  window gone; authorize from SETUP path + token instead). Updates the
  round-1 "what we give up: two-phase IncomingSession accept" note.

### Round-2 execution plan

Wave 1 (map refresh, running): A1 rewrites moq media maps to main
(new files moq-video/moq-audio-nvenc/moq-transcode-stats.md; retire the
dev-named ones); A2 rewrites moq-net-origin.md to main + folds the net
routing/token deltas. moq-main-media.md already retired to a stub.

Wave 2 (deep investigations, running, the new substance the user
asked for):
- B1 rewrites 3t much more thoroughly: exhaustive method-by-method
  interface tables, data-model deep dive, extension-point analysis,
  decision list. de-dev-ified.
- B2 writes NEW doc 3u-moq-changes.md: the concrete moq-side changes to
  upstream all capture/codec, in the user's exact sections (1 zero-copy
  render+capture; 2 open the closed Backend variants with a public
  trait; 3 other differences; 4 would moq accept EXTERNAL codecs at all;
  5 sequenced change list). Rust sketches in moq vocabulary.
- B3 rewrites 3z: de-dev-ified + deepened RENDER UPSTREAMING decision
  (upstream in-tree vs out-of-tree crate vs keep-in-repo-fully-aligned).

Wave 3 (after 1+2): rewrite 0,1,2,3,4,5,6,7,8,9,10 to current moq main:
remove dev/main distinction everywhere; collapse the cut scenarios
(no more dev-gating: now current-main-git vs next-release); fold the
mux/stats/catalog and net/routing/token deltas; fold B1/B2/B3
conclusions; update 8-upstream to reference 3u. Then review + fix.

New doc count target: 0,1,2,3,3t,3u,3z,4,5,6,7,8,9,10 (14 numbered).

Wave 1/2 results (all but A1 media maps landed):
- A2 net map: create_broadcast + RouteCost + no-linger + token
  path-scoping (closes spoofing gap) + single-phase iroh accept.
- B1 3t: deepened; corrections: hang 0.19.5 renamed
  displayRatio->displayAspect so our config.rs mirror no longer
  compiles (strengthens "drop mirror"); moq audio Options already has
  track name, video does not. 12 decisions D1-D12.
- B2 3u: keystone = public frame vocabulary + DmaBuf/AHB variants +
  native() accessor; PTS-through-encode small+unconditional; public
  registerable Backend trait the one breaking ask (external/Android
  path only). VAAPI/V4L2/AV1 in-tree; Android external; render
  out-of-tree. No existing external seam today.
- B3 3z: converged with 3u on Option B (out-of-tree moq-video-render
  crate). U1-U4 requirements. decode->render decisively ours.
3t/3u/3z cross-reference consistently. Only A1 pending.
- A1 media maps (done): moq-video/audio/nvenc/transcode unchanged;
  moq-mux 0.7.6 UNSEALED renditions (#2420) -> public RenditionConfig<E>
  + Estimate{jitter,bitrate} with per-rendition auto-detection (this
  addresses the round-1 "hang lacks jitter field for sync.rs" gap),
  shareable timelines, public Recorder; #2426 poll_read contract
  (Ok(Some(empty)) = poll again); moq-stats 0.1.0 Traffic gained
  fetches+datagrams, Tier now bare PathOwned. hang displayAspect rename
  breaks our config.rs mirror (already flagged by 3t).

Wave 3 (rewrite to current main, running): C1 (3,4,6), C2 (5,2),
C3 (9 rooms + net/token deltas), C4 (7 cut + 8 upstream: scenario
collapse to A ~4,800 adopt-as-is / B ~17,400 +upstreams, no branch
gamble; 8-upstream references 3u for the API design), C5 (0-overview +
1-code-map). Cut scenarios collapse: the thin-main world is gone.
Wave 4: 10-summary synthesis after Wave 3, then review + fix.

Wave 3 status: C1 (3,4,6) done - no verdicts flipped, framing updated.
C2 (5,2) done - moq-mux Estimate{jitter,bitrate} partly satisfies the
sync input side (publish-side jitter bound in catalog seeds our playout
clock, does not replace sync.rs). C3 (9 rooms) done. C5 (0,1) done.
C4 wrote 7-cut (scenario collapse to A ~4,800 / B ~17,400) but
8-upstream NOT yet rewritten (still the 07-18 version) - agent still
running or died mid-batch; will relaunch 8-upstream alone if no
completion. Rewritten docs verified clean of dev/main framing (only
legit merge-notes remain). 10-summary still pending Wave 4.

Wave 3 complete: all 14 numbered docs rewritten to current moq main.
C4 finished both 7-cut (scenario collapse A ~4,800/12% adopt-as-is via
one bump / B ~17,400/42% +upstreams; old thin-main world folded into A)
and 8-upstream (PR program cross-referencing 3u; render folded in as
first-class out-of-tree moq-video-render crate C13, wave 5). Wave 4:
coordinator authored 10-summary to current main (two scenarios, 3u +
render decision surfaced prominently per the user's asks, token-scoping
+ no-linger room updates, single release gate). All rewritten docs
verified clean of live dev/main framing (merge-notes only).

Round-2 review launched: R1 accuracy/leaks/number-consistency (verify
deltas against source, no dev/main leaks, cross-refs, versions), R2
new-substance depth (does 3u/3t/3z deliver the user's three asks:
render upstreaming, thorough interface comparison, moq-side change
design + external-codec question). Then fix + close.

Note: first Wave-1/2 dispatch (~12:35) died on the session usage limit
(reset 12:50); relaunched 12:04 next day after limit cleared. moq HEAD
re-verified unchanged at 3a3e0ea8.



Started: 2026-07-18T13:49+02:00
Mode: overnight
Plan: plans/refactor/0-overview.md
Branch: main (planning only, no code changes)

Goal: produce rigorous plan docs in `plans/refactor/` that (1) map all
overlap between iroh-live media code and moq, (2) compare the
encode/decode/publish/subscribe/capture/audio/video APIs and traits
hands-on with pros/cons, (3) lay out how to cut most owned code by
replacing with moq primitives and how to upstream what moq lacks,
(4) analyze moq's origin/hop/session primitives for a room-layer
redesign, and (5) summarize the end-state and estimated moq PR sizes.

The deliverable is planning docs, not code. No source changes.

## Key early findings (organize/research phase)

### moq has two very different states: main vs dev
- **moq main** (released; iroh-live depends on it): native codec work
  lives in `rs/moq-video` (encode + capture) and `rs/moq-audio` (full
  capture/encode/decode/resample). Net layer is `rs/moq-net`
  (published as package `moq-net`, aliased by iroh-live as `moq-lite`).
- **moq dev** (STRONG CALLOUT: unreleased, major restructure):
  - `moq-net` renamed to `moq-lite` (real crate rename).
  - `moq-video` and `moq-audio` REMOVED. Codec work consolidating into
    new `rs/moq-codec` (currently a lib.rs stub) and heavily into
    `rs/moq-mux` (now carries producer/{aac,annexb,av01,avc1,avc3,
    decoder,fmp4,hev1,hls,opus} + consumer + ordered + cmaf + hang).
  - New crates: `rs/conducer` (generic producer/consumer/lock/waiter/
    weak primitives), `rs/moq-clock`.
  - Origin/hop refactor: "Replace hop count with explicit OriginId
    list" (#1152), "Refactor Origin API" (#1142), subscription-model
    refactor + async subscribe_track (#1134), dynamic SyncTrack API
    (#1138), catalog task APIs renamed subscribe/unsubscribe (#1140),
    moq-native reorg feature-gated modules + builder (#1141).

### moq-native already has native iroh transport
- `rs/moq-native/src/iroh.rs` exists on BOTH main and dev. moq already
  integrates iroh transport. iroh-live's `iroh-moq` (572 LOC) and parts
  of the room layer may be largely replaceable. Needs verification.

### iroh-live owned-code sizes (LOC)
- rusty-codecs: ~22,310 (codecs: h264/av1/opus/pcm + hw: vaapi/v4l2/
  vtb/android + render/dmabuf/gles/metal + processing + traits/format)
- rusty-capture: ~5,507 (linux pipewire/v4l2/libcamera/x11, apple,
  android, nokhwa, xcap)
- moq-media: ~11,441 (publish/subscribe/adaptive/sync/catalog/pipeline/
  audio_backend/stats/chat/transport)
- iroh-moq: 572, iroh-live: ~1,750 (rooms/call/live/ticket/subscription)

## USER DIRECTIVE: go deep on codec/capture/zero-copy + trait API

Standing requirement for the dev-vs-(rusty-codecs/rusty-capture/moq-media)
comparison. Do NOT settle for "moq dev has a similar thing":

1. Per-codec, per-backend depth. For each codec (H.264, H.265, AV1, VP9,
   Opus, PCM) and each backend, compare what we have vs what moq dev has,
   feature by feature. State which is better and why, or exactly how they
   differ.
2. Zero-copy is the special focus, for both rendering AND encoding. Our
   significant zero-copy work (capture GPU surfaces DmaBuf/CVPixelBuffer/
   AHardwareBuffer, VAAPI VPP retiling, wgpu import via Vulkan/ash +
   EGL/GLES + Metal) must be preserved. Determine: is ours better or
   theirs, how do they differ, and what we would need to upstream to use
   theirs while keeping zero-copy everywhere.
3. Detailed trait/interface/Rust-API comparison, side by side with real
   signatures. Any API change needed to adopt moq's traits must be spelled
   out concretely, because that is the first thing to discuss.

Comparison docs restructured accordingly:
- 3-compare-codecs.md (per-codec/backend deep feature comparison)
- 3z-compare-zerocopy.md (zero-copy render + encode/decode deep dive)
- 3t-compare-traits-api.md (trait/interface/Rust API + concrete change
  proposals; highest-priority discussion artifact)
Each deep-dive agent reads BOTH repos' actual source, not just the maps.

## DEFINITIVE TOPOLOGY (2026-07-18, both refs updated by user)

Both main and dev were stale in the local checkout at session start and
were updated by the user. Final verified state:

- main HEAD `2be3a55f` (2026-07-18, #2383).
- dev HEAD `261c2048` (2026-07-17, #2241). Dev maps are pinned to this
  SHA.
- merge-base `b0a8c834` (2026-07-17, #2375). They diverged one day ago.
- topology main...dev = **3 / 282**. The 3 main-only commits are trivial
  fixes (CMAF init strip, libmoq pc path, ietf request ids). The 282
  dev-only commits carry the entire rich media stack.

Native media per branch, verified against the current trees:

- **main**: `moq-video` 0.0.6 is still thin (files: capture.rs,
  encode/{encoder,producer,mod}.rs, error.rs, lib.rs). H.264 encode only,
  via ffmpeg, NO decode, NO backend-dispatch directory. `moq-audio` is
  the old Opus surface (codec/producer/consumer/format/frame/resample/
  capture). main HAS the ingest/egress bridges moq-hls/moq-rtc/moq-rtmp/
  moq-srt, but NOT moq-nvenc/moq-transcode/moq-stats.
- **dev** adds, dev-only: full `moq-video` encode+decode with direct
  backends (encode: nvenc/vaapi/videotoolbox/mediafoundation/openh264;
  decode: nvdec/videotoolbox/mediafoundation/openh264), H.265 decode
  (VideoToolbox #1859 + Media Foundation #1854), AV1 NVDEC decode
  (#2178), congestion-adaptive encoder bitrate (#2303), composable
  Broadcast + per-rendition encoders (#2257), upload bitrate from encoder
  Stats (#2246), PipeWire screen capture (#2238), macOS window/app/
  system-audio capture (#2293); the crates `moq-nvenc` (vendored
  NVENC+NVDEC, #2042), `moq-transcode` (JIT ABR, decode-once + GPU resize
  fanout, zero-copy NVDEC->NVENC, #2140/#2158), and `moq-stats` (#2380).

CENTRAL FINDING: the native codec/capture stack that overlaps
rusty-codecs, rusty-capture, and moq-media's pipeline is DEV-ONLY. Align
to main and almost nothing can be cut; align to dev and most of it can.
This is the concrete answer to "flag what dev enables that main does not."

Valid maps (main media crates only version-bumped; same structure):
rusty-codecs, rusty-capture, moq-media, room-layer, moq-main-media.
Regenerating dev maps against 261c2048: moq-dev-video, moq-dev-audio-
nvenc, moq-dev-transcode-stats, moq-net-origin.

## CRITICAL CORRECTION (2026-07-18, clock reset to 8h)

The first mapping pass read a STALE local `dev` ref (`29a2bad`, April-era)
and drew wrong conclusions. The user updated `dev` to current
(`261c2048`, 2026-07-17) and asked to recheck. Verified topology:

- merge-base(main, dev) = `2788d79f` = main's own HEAD (June 16).
- ahead/behind main...dev = **0 / 506**. dev strictly CONTAINS all of
  main and adds 506 commits. dev is the active media-development line;
  main is a lagging snapshot (frozen at #1769 for the media stack).
- Current dev keeps `moq-net`/`moq-video`/`moq-audio`/`moq-mux` (same
  names as main) and ADDS: `moq-nvenc` (NVENC+NVDEC/cuvid bindings),
  `moq-transcode` (ABR rungs), `moq-rtc`, `moq-rtmp`, `moq-srt`,
  `moq-hls`, `moq-stats`, `moq-flate`, `moq-wasm`.
- The stale-dev crates I first mapped (`conducer`, `moq-lite` rename,
  `moq-codec` stub, moq-mux consolidation, removal of moq-video/audio)
  DO NOT EXIST on current dev. That was an abandoned experimental line.

What current dev's `moq-video` actually contains (this changes
everything): full native encode AND decode with DIRECT hardware
backends, not ffmpeg:
- encode/backend/{nvenc,vaapi,videotoolbox,mediafoundation,openh264}
  + encode/{rate,sink,producer,encoder}
- decode/backend/{nvdec,videotoolbox,mediafoundation,openh264}
  + decode/{consumer,decoder}
- capture/{pipewire,v4l2,screencapture,avfoundation,desktopduplication,
  mediafoundation,surface,pump,channel}
- frame/nv12_resize.cu + .ptx (CUDA NV12 resize), size.rs
`moq-audio` dev: encode/decode/capture/opus/resample/format/frame.
Notable dev commits: #2303 moq-video encoder bitrate adapts to
congestion estimate; #2176 unified latency_max; #2302 caller-driven
(Session, Driver); #2241 subscriptions migrate across connections;
#2380 moq-stats extraction; #2341 non_exhaustive catalog + hang draft
catch-up; #9090 macOS window/app/system-audio capture; #c57f5b PipeWire
screen capture; #2350 moq-audio surface aligned with moq-video.

Consequence: moq dev's native stack now DIRECTLY overlaps rusty-codecs,
rusty-capture, and much of moq-media's pipeline. The refactor premise is
real and strong against dev; against main the native codec stack is
still thin (moq-video 0.0.4 H.264-ffmpeg encode-only, moq-audio Opus).
Per user: treat main and dev as co-equal, highlight differences, and
flag anything dev enables that main does not.

iroh-live does NOT depend on moq-video/moq-audio; it uses rusty-codecs
+ rusty-capture (verified moq-media/Cargo.toml). That is the code the
refactor targets for replacement by moq's native stack.

Stale maps neutralized: `maps/moq-dev-media.md`, `maps/moq-origin-hop.md`
(their MAIN content on moq-net origin is still roughly valid; their DEV
content is abandoned-line and must not be used). Re-mapping current dev
under new files.

## Round 2 closing summary (2026-07-22)

Round 2 is complete. Trigger: moq merged dev into main (2026-07-21),
making the entire main-vs-dev scaffolding obsolete. All 14 numbered docs
in plans/refactor/ were rewritten to current moq main (HEAD 3a3e0ea8),
the moq-side maps refreshed, and the new substance the user asked for
added. Final verification sweep clean: no live dev/main framing (only
merge-notes), no references to retired stub maps, no stray requirement
tokens, no em dashes, no stale round-1 scenario phrases, per-crate LOC
figures sum exactly to 41,564, all numbers consistent across docs.

What changed materially from round 1:
- Cut scenarios collapsed from three to two: A (adopt moq main as-is via
  one version bump) ~4,800 LOC / 12%; B (adopt + upstreams accepted)
  ~17,400 LOC / 42%. The round-1 thin-main floor is gone. The release
  risk dropped from a dev-branch gamble to one ordinary release + bump.
- NEW doc 3u-moq-changes.md: the concrete moq-side changes to upstream
  all capture/codec, in the user's sections (zero-copy render+capture;
  open the closed Backend variants with a public trait; other
  differences; external-codec question; sequenced change table).
  Keystone = public frame vocabulary + DmaBuf/AHardwareBuffer variants +
  native() accessor; PTS-through-encode small+unconditional; public
  registerable Backend trait the one breaking ask (external/Android
  only). External codecs: no seam today; VAAPI/V4L2/AV1 in-tree, Android
  external, renderer out-of-tree.
- Render answer (user's new ask): upstream as an out-of-tree
  moq-video-render crate over moq's public handles (Option B), converged
  independently by 3z and 3u; keep-in-repo-aligned is the fallback.
- 3t deepened to exhaustive method-by-method interface tables (12
  decisions D1-D12).
- Room layer: moq-token path-scoping (#2416) closes the round-1
  announce-spoofing gap; #2419 removed ROUTE_LINGER so the "roaming for
  free" claim was corrected; single-phase iroh accept.
- moq-mux 0.7.6 Estimate{jitter,bitrate} partly satisfies the sync
  upstream input side (publish-side jitter now in the catalog).

Reviews: round 2b accuracy (34 CONFIRMED-OK, 0 WRONG, 0 critical) +
substance (YES on all three asks, 0 critical). Seven findings, all
survived opposing stance, all fixed (six in 3u, one in 7-cut).

Incidents: two subagent waves died on the session usage limit (reset
each time); all relaunched, no data lost. moq HEAD re-verified unchanged
at 3a3e0ea8 before and during the rewrite.

Entry point for the reader: plans/refactor/10-summary.md.

## Staff reviews

### Round 1 (2026-07-18 ~18:00): accuracy + design + prose

Accuracy (review-accuracy.md, 45 rows): 34 CONFIRMED, 3 WRONG, 7
INCONSISTENT, 1 UNVERIFIABLE; 0 critical. Everything decision-bearing
held under source check (topology, versions, pub(crate) claims, VAAPI
placeholder, gaps in moq Rust, set_latency/recv_bandwidth, LOC
denominators). Wrong/inconsistent: 1-code-map sec 3 frames main's
ffmpeg capture as "moq's stack" unflagged; 7-cut test counts overstated
(e2e 4 not 5, room 6 not 11); 10-summary moq-media after-figure not
derivable; wave sums and iroh-moq split figures drift across docs.

Design (review-design.md, 12 findings): F1 dev-churn hedge missing
(critical), F2 no maintainer-bandwidth model or pilot gate (critical),
F3 mixed-stack bridge cost uncounted (critical), F4 rooms wins
overstated, F5 scope()-vs-session-dedup unresolved design question
(reviewer verified scope() exists on main), F6 expectation-setting
("massive" = 12% floor / 42% best), F7 unpriced alternatives, F8 CI gap
makes P1 unenforceable for macOS/Windows, F9/F10 one-sided accounting +
missing merged dependency graph, F11 wave-total drift, F12 RFC-first vs
trust-builder-first ordering.

Prose (review-prose.md): 9 substantive, 12 stylistic, 12 nits; no
broken cross-refs; headline numbers consistent; 4 em dashes (3z
headings), load-bearing x6, budget x4; 8-upstream calls 7-cut "pending"
though it exists.

### Round 2b review (post-rewrite to current main) + resolutions

Two reviewers on the rewritten set.
- Accuracy (review-round2b-accuracy.md): 34 CONFIRMED-OK, 0 WRONG, 0
  INCONSISTENT, 0 critical. All dev/main hits are legit merge-notes (no
  live comparative framing); all 8 key deltas confirmed against
  3a3e0ea8; all numbers consistent (per-crate Today figures sum exactly
  to 41,564); versions correct. 3 substantive cross-ref issues, all in
  3u/7-cut: 3u cites the superseded maps/moq-dev-video.md stub 3x; 3u
  uses undefined "R1-R7" where 3z now uses U1-U4; 7-cut:57 mischaracter-
  ized 2-inventory table 2 as "still pre-merge framing" (it was
  reframed).
- Substance (review-round2b-substance.md): overall verdict YES, all
  three maintainer asks delivered, 0 critical. Refinements, all in 3u:
  F1 wrong cite (encode/encoder.rs 279-281 -> 249-251); F2 3u 1b drops
  the maintainer's third render option (keep-in-repo-aligned), which 3z
  and 10-summary cover as Option C; F3 section 4 external-codec verdict
  is video-only, no PCM/Opus; F4 register_decoder called a "mirror" but
  the decode seam (supports/open fns) is not symmetric. F5/F6 stylistic.

Review-of-reviews (opposing stance): all seven findings survive; none
are noise. 3t device-layer-in-prose (F5) is defensible (no moq
counterpart to tabulate) - skip. Resolutions: fixed 7-cut:57
characterization inline (done); dispatched a focused 3u fixer for the
six 3u items (map cite, R->U, F1 cite, F2 third option, F3 PCM/Opus
verdicts, F4 decode sketch).

### Round 1 review of reviews (opposing stance)

Accepted: all accuracy findings (mechanical, verifiable); all prose
substantives and mechanical rule violations; design F1-F11.
Resolutions: F1 -> add plan-freshness protocol + slip-scenario pricing;
F2 -> add wave-1 velocity gate before wave 2 + bandwidth assumption;
F3 -> count bridge cost in stage 2 + compare atomic-per-platform
alternative; F4 -> rewrite rooms wins honestly (announce does not
remove gossip in Variant A); F5 -> add the static-scope vs
one-session-dedup conflict as a named open question for phase 2;
F6 -> 10-summary states floor/best/expected numbers plainly;
F7 -> add priced rejected-alternatives (do-nothing, fork) section;
F8 -> make macOS/Windows CI an explicit stage 2/3 prerequisite;
F9/F10 -> add churn/rework accounting note + merged stage/wave/R/D
dependency graph reference; F11 -> recompute wave totals.
Partially accepted: F12. Opposing stance holds: D1/D3 gate everything
downstream, so the RFC must open the conversation; but the rationale
must be stated, and wave 1 already carries small goodwill PRs. Fix is
an explicit rationale paragraph, not a reordering.
Rejected: none outright.

Fix wave dispatched with file ownership to avoid conflicts:
A = mechanical fixes everywhere except {0,7,8,9,10};
B = 7-cut-plan + 8-upstream-plan (design + numbers);
C = 9-room-layer + 10-summary + 0-overview (design + numbers).

## Progress

### 2026-07-18T20:30 - round 2 resolved; session closing

Round-2 report (review-round2.md): 42 resolutions verified, 0 blocking,
3 substantive cross-file seams, 6 nits. Coordinator applied inline:
- Seam 1: 7-cut R-c now frames scenario (ii) as an acceptable waypoint
  whose keep-local terminal state is the tri-stack world 8-upstream
  warns about (the two docs disagreed before; now reconciled).
- Seam 2: 10-summary's 12% scenario now carries the atomic-switchover
  qualifier (lands platform by platform, Windows first).
- Seam 3: phase-2-preparatory framing added to 10-summary's room
  paragraph and gating reality 1 ("possible on main" != "pays off on
  main").
- Nits: stage-0 mislabel in the churn note fixed; stage-2 Content line
  now leads with the atomic recommendation instead of plan-then-
  retraction; C13 honesty clause (proof is prospective until C2 lands);
  9-room 3.1 scoping bullet now defers to the 3.2 open question.
Deliberately kept (opposing stance): 0-overview goal wording (the
scenario expectations live one hop away in 10-summary, and the overview
is an index, not an argument); F1 freshness-protocol location in
7-cut/8-upstream rather than 0-overview (same reason).

### Closing checklist (overnight discipline)

- [x] Wall-clock: 6h41m since session start (13:49), 6h10m since the
  user's 8h reset (~14:20); the stated goal is fully met, which the
  rules accept as the stopping criterion.
- [x] Tests: not applicable; this session produced planning docs only,
  no source changes (verified: git status clean, plans/ gitignored).
- [x] No todo!() in scope: no code written.
- [x] Every staff-review finding applied or logged with the opposing
  stance (round 1: all accepted items fixed by the fix wave, F12
  partially accepted with rationale; round 2: all seams + 4 nits fixed,
  2 nits kept with reasons above).
- [x] Staff review round 2 ran on the post-fix docs (review-round2.md).
- [x] No ignored/skipped tests: not applicable.
- [x] No forbidden phrase appears as a resolution: the one "deferred"
  item (gossip-free room variant) is a design decision with a stated
  unblocking condition, made inside the deliverable, not a dodge of it.

### 2026-07-18T20:20 - fix wave complete; round-2 review launched
All three fix agents finished. A: 24 mechanical fixes across the
comparison docs + dated version note on maps/moq-main-media.md
(deliberate skips: map-body em dashes out of scope, verbatim quotes
exempt). B: 7-cut gained slip floor + dev-tracking hedge + freshness
protocol + stage-2 bridge cost (300-600 LOC temporary, gross-vs-net) +
atomic-per-platform alternative (recommended for held platforms) +
platform verification gate + churn note + 20-row merged dependency
table; corrected test counts (e2e 4, room 6); 8-upstream gained
bandwidth assumption + wave-1->2 velocity gate with C2+C1b pilot +
tri-stack statement + recomputed wave totals (wave 3 ~3,250; wave 1
~140/3 PRs) + RFC-first rationale; stale "pending" refs fixed. C:
9-room verdict rewritten honestly (single-protocol claim retracted,
two regressions named with mitigations) + multi-room scope() open
question with recommendation; 10-summary gained expectation paragraph
(3%/12%/42% by scenario) + priced alternatives (do-nothing, fork) +
corrected end-state figures (moq-media ~10.7k/~9.6k, rusty-capture
~4.3k) + D1-D12 count fix + enabler nuance; totals delegated to
8-upstream to prevent drift. Cross-file seam grep: clean (no stale
2,950).
Round-2 reviewer launched over all twelve numbered docs: resolution
application, cross-file seams, coherence of new sections, mechanical
regressions.

### 2026-07-18T20:15 - usage-limit stall; fix wave relaunched
The first fix-wave dispatch (~18:40) died on the session usage limit
(reset 19:20); the failure notifications arrived at 20:12. No doc was
modified by the failed agents. Relaunched all three fix agents with
identical briefs and file ownership. Elapsed 6h26m of 8h. Remaining
plan: fixes land -> round-2 review on changed docs -> closing
checklist + final summary.

### 2026-07-18T17:10 - all 11 docs complete; review wave 1 launched
7-cut-plan landed: cut scenarios ~1,300 (main-only, 3%) / ~4,800 (dev
as-is, 12%) / ~17,400 (dev + upstreams, 42%); 6 stages; top risks: dev
release timing, API churn, upstream acceptance, rav1d/cpal git pins,
test gaps (no macOS/Windows CI, manual zero-copy e2e).
8-upstream-plan landed: 14 contributions, 5 waves, ~9.5-10.5k LOC
across ~20 PRs; wave 1 = D1+D3 RFC + VAAPI validation report + small
goodwill fixes; render stack stays local as reference consumer.
C10 verified: dev already has Consumer::set_latency.
10-summary written by coordinator (end-state table per crate, gating
realities, reading order).
Review wave 1 dispatched: accuracy (verify ~30 decision-relevant claims
against sources), design (attack dev-bet, upstream-bet, mixed-stack
middle state, rooms value, keep-list honesty, missing alternatives,
announce-spoofing auth gap), prose/consistency (writing rules, numbers
across docs, dev-flag discipline, cross-refs).
Next: review-the-reviews with opposing stance, refine, round-2 review.

### 2026-07-18T16:20 - all comparison docs done; launching cut + upstream plans
Landed since last entry: 3-compare-codecs (their VAAPI encode is a
111-line CPU-only unvalidated placeholder; ours decisively upstreamable;
AV1 SW ours alone; opus FEC claims on their side unbacked),
3z-compare-zerocopy (complementary investments; decode->render
decisively ours; R1-R7 upstream requirements, keystone = public
platform-handle vocabulary), 3t-compare-traits-api (open-vs-closed is
the real divide; decode one-shot loses nothing; encode needs PTS
threading through Backend::encode for pipelined V4L2/MediaCodec; D1-D6
decision list), 4-compare-capture (keep Linux, adopt Windows/macOS
camera, keep AEC; two map errors corrected from source),
5-compare-pubsub (subscribe already delegates to moq_mux; catalog
finding resolved; adaptive ~340L + sync are the two Rust gaps;
recv_bandwidth confirmed dev; <100 LOC direct cut in moq-media),
6-compare-audio (merge opus wrappers; keep playback+AEC, zero moq
equivalent; adopt their capture surface on bounded buffers),
2-moq-inventory (11 dev-only enablers register; bandwidth::Consumer on
BOTH branches, only the policy is dev-only; main versions newer than
first map recorded: hang 0.19.5, moq-net 0.1.18), 9-room-layer
(announce-based rooms, gossip for bootstrap only, 3 phases,
main-compatible fallback).
Stale memory in MEMORY.md fixed (subscribe.rs types, hang 0.19,
catalog).
Now launching: 7-cut-plan + 8-upstream-plan (parallel), then
10-summary, then adversarial review wave over all docs.

### 2026-07-18T15:40 - net/origin map done; 2-moq-inventory + 9-room-layer launched
maps/moq-net-origin.md written (after two stalls; inline-read redirect
worked). Attribution correction: for moq-net/moq-native, main ==
merge-base; ALL net refactors are dev-only (#2302 Session/Driver,
#2176 latency_max, #2241 transparent subscription migration via
resume.rs group-boundary splicing, #2349 aggregate clamp, Role,
announce ids + AnnounceOk roster sync, route machinery). Main has NO
subscription model (hard-coded ordered=true, max_latency=0). Both
branches share the iroh transport (iroh 1.0.2, web-transport-iroh 0.6,
EndpointConfig, Client::with_iroh, iroh:// dial); dev collapses accept
to one-phase. Room mapping first cut: shared per-node Origin +
broadcasts at <room>/<endpoint-id>/... + announced() prefix scope;
keep ticket bootstrap, gossip only for membership hints. Caveat
logged: iroh-live pins main-line releases; every dev capability
needs a future breaking bump.
All 7 valid maps complete. Launched 2-moq-inventory and 9-room-layer
drafters. Now 8 agents in flight (6 comparisons + these 2).

### 2026-07-18T15:25 - transcode/stats map in; full comparison wave running
maps/moq-dev-transcode-stats.md done. Headline: subscriber-side
rendition selection does NOT exist in moq Rust (JS-only heuristic in
js/watch); moq-transcode is supply-side JIT ABR (Rung ladder,
demand-driven encode, group N mirrors source group N). Complementary to
our adaptive.rs, which fills moq's Rust demand side. moq-stats =
relay traffic accounting (Traffic/Presence counters), NOT congestion
telemetry; does not replace our stats.rs. hang: NO chat/user sections
on either branch (stale-dev map was wrong; re-map validated).
Rust hang has no live catalog producer/consumer; moq_mux::catalog has
it. dev moq-mux adds mid-stream Consumer::set_latency (main:
builder-only with_latency; mid-stream is required by our adaptive),
publish-side Metrics (jitter + windowed bitrate -> catalog),
Reserved/Rendition atomic catalog gating, Producer::seek, and
cross-broadcast rendition refs (catalog broadcast field, dev-only).
moq-mux Frame carries the keyframe bit; hang Frame does not (both
branches).
Now running in parallel: 3-compare-codecs, 3z-compare-zerocopy,
3t-compare-traits-api, 4-compare-capture, 6-compare-audio,
5-compare-pubsub, and the moq-net-origin mapper (redirected to inline
reads). Next after they land: 2-moq-inventory, then synthesis docs
7/8/9/10, then adversarial reviews.

### 2026-07-18T15:10 - dev maps landing; launching deep-dive comparisons
Done: 1-code-map.md (41,564 owned LOC; codec+capture+gpu-zerocopy =
21,301 = 51%), maps/moq-dev-video.md, maps/moq-dev-audio-nvenc.md.
Key facts locked in from the dev maps:
- moq-video dev Backend traits (encode+decode) are pub(crate): NO
  public backend extension point today. Adding our backends upstream
  requires an API change; this heads the trait-API discussion doc.
- Codec matrix dev: H.264 all backends (openh264 sole SW fallback),
  H.265 HW-only, AV1 NVDEC-decode-only, VP9 none. Our gaps to offer
  upstream: AV1 SW enc+dec (rav1e/rav1d), VAAPI decode (dev's vaapi is
  encode-only via external moq-vaapi crate; Linux HW decode is
  NVDEC-only), V4L2 M2M encode (Pi), Android MediaCodec, PCM.
- Zero-copy dev: capture->encode (IOSurface on macOS, D3D11 on
  Windows), NVDEC->NVENC CUDA transcode; but decode->RENDER has no GPU
  handoff (VT/MF download to I420); only NVDEC output stays on GPU.
  Our decode->render zero-copy (VAAPI DMA-BUF->Vulkan/wgpu, Metal
  import, GLES) is what dev lacks: prime upstream/keep candidate.
- Frame model dev: private enum Frame { Surface(CVPixelBuffer),
  Texture(D3D11 NV12), Cuda(NV12), I420 }; no DMA-BUF variant.
- Rate control #2303: rate::Policy + Control fed from
  moq_net::bandwidth::Consumer; set_bitrate must not force IDR.
- moq-audio dev Opus-only, no FEC/PLC, no runtime bitrate change;
  unbounded mpsc from the cpal realtime callback (contrast with our
  bounded-channel rule); macOS system audio via SCK. moq-nvenc has
  RegisteredResource (external CUDA ptr as encoder input) = the
  zero-copy hook; publishable, safe layer self-described unfinished.
- #2246 encoder Stats is JS-only; the Rust crate has no encoder stats.
Net/origin agent stalled twice on lost child results; redirected to
read inline and write the map itself.
Launching deep-dive wave: 3-compare-codecs, 3z-compare-zerocopy,
3t-compare-traits-api, 4-compare-capture, 6-compare-audio (all read
real sources per user directive). 5-compare-pubsub waits on the
transcode/stats/hang/mux map; 2-moq-inventory after all maps.

### 2026-07-18T14:40 - dev-map agents relaunched
The first relaunch of the four dev maps was dropped in the
interruption window (no output files, empty task list). Relaunched all
four against pinned dev 261c2048 with the deep-dive directive folded
in (zero-copy detail, per-backend features, verbatim trait quotes).
Also dispatched the 1-code-map.md inventory drafter (iroh-live-side
maps are stable). Running: moq-dev-video, moq-dev-audio-nvenc,
moq-dev-transcode-stats, moq-net-origin, 1-code-map.

### 2026-07-18T14:05 - moq architecture grounding (from moq/doc/concept)
Read moq's own layer docs. Authoritative model:
- Layers: QUIC -> WebTransport/WebSocket -> moq-lite (generic pub/sub,
  relay knows nothing about media) -> hang (WebCodecs catalog +
  container) -> application.
- moq-lite terms: Session, Origin (collection of broadcasts scoping a
  session), Broadcast (=Namespace), Track, Group (QUIC stream, GoP,
  starts with keyframe), Frame.
- **`announced(prefix: Path)`**: live broadcast discovery. moq's docs
  call it out for exactly conference rooms: "live discover when
  participants join and leave." moq-relay clustering uses it to
  discover nodes and their broadcasts. This is the primitive iroh-live
  rooms should map onto (point 4).
- Congestion model: subscriber controls latency via track priority,
  group order (default descending, latest first), group timeout
  (default 30s). Groups dropped whole, keyframe-aligned.
- Catalog: WebCodecs renditions; extensions via serde `#[serde(flatten)]`
  root sections + optional per-section tracks (chat/user fit here).
- hang container: fMP4/legacy; codec `description` is avcC or inline
  SPS/PPS; decoder must handle both.
- moq/doc has a `use-case/conferencing.md` (MoQ vs WebRTC) to mine for
  the room-layer plan.

### 2026-07-18T13:49 - Organize + initial research done
Read .agents/ (AGENTS, rules, writing, workflow), overnight+cycle
skills. Surveyed both workspaces. Discovered the main-vs-dev split
above. Set up plan structure. Dispatching parallel mapping subagents
next.

## Summary

All five tasks of the original prompt are complete, twice reviewed,
and fixed. The deliverable is `plans/refactor/`: 13 numbered docs, 9
evidence maps, 4 review reports, 12,415 lines. Entry point:
`plans/refactor/10-summary.md`. No source changes were made; plans/ is
gitignored, so nothing needs committing.

What to read first in the morning:
1. `10-summary.md`: the whole picture in ~165 lines, including the
   scenario expectations (3% floor / 12% dev ships / 42% full success)
   and the priced alternatives.
2. `3t-compare-traits-api.md` section 7: the D1-D12 decision list.
   D1 (public frame vocabulary) and D3 (PTS through Backend::encode)
   are the two decisions to raise with moq first; everything else
   hangs off them.
3. `7-cut-plan.md` and `8-upstream-plan.md`: the actionable plans with
   the merged dependency table, the velocity gate, and the freshness
   protocol.

The three facts that shaped everything:
- moq main and dev diverged 2026-07-17; the entire native media stack
  (moq-video encode+decode with direct HW backends, moq-nvenc,
  moq-transcode, moq-stats) is dev-only. Both local refs were stale at
  session start and were updated mid-session by the user; the first
  mapping pass against April-era dev was discarded and redone.
- The zero-copy investments are complementary: theirs is NVIDIA/Windows
  and capture-to-encode; ours is Linux VAAPI/DMA-BUF, decode-to-render,
  and the wgpu import stack, which they lack entirely. Their VAAPI
  encoder is an unvalidated CPU-only placeholder.
- moq Rust has no subscriber-side ABR, no playout clock, and no audio
  playback/AEC; those are our upstream leverage and our keeps.

Session incidents, for the record: two stale-ref corrections forced a
full re-map (handled); one usage-limit stall killed the first fix wave
at ~18:40 (resets 19:20; relaunched 20:12, no data lost); the
net/origin mapper stalled twice on lost child results and was
redirected to inline reads.
