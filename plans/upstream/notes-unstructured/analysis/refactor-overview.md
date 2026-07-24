# moq-alignment refactor: overview and index

> Campaign: upstream | Kind: analysis | Preserved context; superseded by
> ../0-overview.md where they disagree. Read ../0-overview.md first.

Status: round 2 rewrite in progress (2026-07-21), aligning to current moq
main. This is a planning artifact; no source changes were made. Entry
point: `10-summary.md`.

## Goal

Massively reduce the code iroh-live owns by aligning to moq's native
media stack. Replace our encode, decode, capture, and pub/sub code with
moq's where moq now covers it; upstream the pieces moq lacks that we
have; and keep in this repo only the high-level iroh integration (rooms,
tickets, iroh transport glue) that is genuinely iroh-specific.
Scenario expectations and the priced alternatives (keeping the dual
stack, forking moq-video) are in `10-summary.md`.

## Current moq state

As of 2026-07-21, moq merged its dev branch into main (working tree
`/home/bit/Code/rust/moq`, HEAD `3a3e0ea8`). The dev branch is now
behind main and defunct, so there is no branch distinction anymore and
these docs target a single moq. Every earlier claim that split main from
dev collapses onto current main.

moq main now carries the full native stack. `moq-video` has full encode
and decode with direct hardware backends (encode: nvenc, vaapi,
videotoolbox, mediafoundation, openh264; decode: nvdec, videotoolbox,
mediafoundation, openh264), H.265 decode, AV1 NVDEC decode, capture
backends (pipewire, v4l2, screencapture, avfoundation,
desktopduplication, mediafoundation), encoder bitrate tied to the
congestion-control estimate, composable per-rendition encoders, and CUDA
NV12 resize. The crates `moq-nvenc` (vendored NVENC and NVDEC bindings),
`moq-transcode` (just-in-time ABR ladder with zero-copy NVDEC to NVENC),
and `moq-stats` are part of main. ffmpeg is gone from the native path.
`moq-audio` is Opus, on cpal. Net layer is `moq-net`; catalog and
container live in `hang`; muxing and the catalog producer in `moq-mux`.

iroh-live does not yet consume the merged stack. It still pins the older
moq-net 0.1.11 / moq-native 0.17.1 release line, so this native stack
lands in iroh-live only when it bumps to the next moq release that
includes the merge. iroh-live also does not depend on
`moq-video`/`moq-audio`. It built its own `rusty-codecs` and
`rusty-capture` because moq's native codecs were thin when iroh-live was
written. Those are the crates this refactor targets, and current moq
main directly overlaps `rusty-codecs`, `rusty-capture`, and much of
`moq-media`, so most of that code can be cut.

## Progress checklist

- [x] Organize, research, dispatch first mapping wave
- [x] Verify moq topology; confirm the 2026-07-21 dev-into-main merge
      (HEAD `3a3e0ea8`)
- [x] Re-map current moq media (video, audio+nvenc, transcode+stats) and
      the net/origin layer into `maps/`
- [x] `1-code-map.md`: full inventory of iroh-live owned media code
- [x] `2-moq-inventory.md`: moq primitives on current main
- [x] `3-compare-codecs.md`: per-codec, per-backend deep feature
      comparison (rusty-codecs vs moq-video/moq-nvenc)
- [x] `3z-compare-zerocopy.md`: zero-copy rendering and encode/decode
      deep dive; preserve our zero-copy work, assess theirs, upstream gap
- [x] `3t-compare-traits-api.md`: detailed trait/interface/Rust-API
      side-by-side with concrete change proposals (discuss-first artifact)
- [x] `3u-moq-changes.md`: concrete moq-side API changes to upstream
      capture and codec support
- [x] `4-compare-capture.md`: capture backend breadth and API comparison
- [x] `5-compare-pubsub.md`: publish/subscribe/catalog/container/adaptive
- [x] `6-compare-audio.md`: audio device and codec comparison
- [x] `7-cut-plan.md`: what to delete and replace with moq primitives
- [x] `8-upstream-plan.md`: what moq lacks, integration and upstream PR
      plan with size estimates
- [x] `9-room-layer.md`: origin/announce/session analysis + room redesign
- [x] `10-summary.md`: end-state, what remains, what moves where, moq PR
      size estimates
- [x] Round 1 adversarial review and fix wave (logged in worklog);
      reports in review-*.md
- [x] Round 2: rewrite every doc to current moq main and remove all
      main-vs-dev language
- [x] `10-summary.md` rewritten to current moq main
- [x] Round 2 review of the rewritten doc set (accuracy + substance),
      review-of-reviews, and fixes applied; reports in
      review-round2b-*.md

Status: round 2 complete, aligned to current moq main (HEAD `3a3e0ea8`).
Start with `10-summary.md`.

## Document index

`maps/` holds the raw evidence-backed code maps the numbered docs are
built from. Read a numbered doc for conclusions; read the matching map
for the underlying `file:line` evidence.

Numbered docs: `0-overview`, `1-code-map`, `2-moq-inventory`,
`3-compare-codecs`, `3t-compare-traits-api`, `3u-moq-changes`,
`3z-compare-zerocopy`, `4-compare-capture`, `5-compare-pubsub`,
`6-compare-audio`, `7-cut-plan`, `8-upstream-plan`, `9-room-layer`,
`10-summary`. `3u-moq-changes` is new: it collects the concrete
moq-side API changes needed to upstream our capture and codec work, in
sections for zero-copy render and capture, opening the closed `Backend`
variants with a public trait, other differences, the external-codec
question, and a sequenced change list.

Valid maps: `rusty-codecs`, `rusty-capture`, `moq-media`, `room-layer`,
`moq-net-origin`, `moq-video`, `moq-audio-nvenc`, and
`moq-transcode-stats`. Retired stubs, kept only as redirects:
`moq-main-media`, `moq-dev-media`, `moq-origin-hop`, `moq-dev-video`,
`moq-dev-audio-nvenc`, and `moq-dev-transcode-stats`.

## Prior work incorporated

- `plans/old/review-moq-usage.md`: moq-mux already exposes a generic
  catalog stack (`catalog::hang::Producer/Consumer`, `CatalogExt`) that
  we hand-roll; `iroh-moq` pins a fixed ALPN; origin-id and hop-chain
  mechanics.
- `plans/old/rooms-overhaul.md`: the current room design (gossip peer
  discovery, `PeerState` KV, chat as MoQ track groups).
- `plans/old/adaptive-track-refactor.md`: the adaptive-track design
  (bandwidth-primary selection, channel-swap switching, audio
  co-subscribe), an upstream candidate to weigh against moq's
  `moq-transcode` and moq-video congestion-adaptive bitrate.
