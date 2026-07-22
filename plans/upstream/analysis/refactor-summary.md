# Summary: iroh-live after the moq alignment

This is the executive synthesis of the refactor planning documents in this
directory. Read this first; every claim links to the doc that carries the
evidence. moq state as analyzed on 2026-07-21: moq merged its long-lived
rewrite branch (`dev`) into `main` on that date, so there is one moq codebase,
working tree at HEAD `3a3e0ea8`, and no branch distinction remains
(`0-overview.md`). iroh-live still pins the older `moq-net 0.1.11` /
`moq-native 0.17.1` / `hang 0.19.1` release line, so the merged native stack
lands for us with one version bump to the next moq release cut from main.

## The one-paragraph answer

iroh-live owns 41,564 LOC of media code (`1-code-map.md`). Because moq main now
carries the full native stack (`moq-video` with hardware encode and decode,
`moq-nvenc`, `moq-transcode`, `moq-stats`, native capture, no ffmpeg), the cut
is real and needs no branch gamble, only a release and a bump. Two scenarios
frame it (`7-cut-plan.md`): Scenario A, adopt current moq main as-is, cuts about
4,800 LOC (12%) with a single version bump and no upstreaming; Scenario B,
adopt plus land our upstream contributions, cuts about 17,400 LOC (42%). Roughly
24k LOC stays by design: the wgpu render and zero-copy import stack, the audio
playback backend with echo cancellation, the adaptive policy and playout clock
until they move upstream, Linux capture, and the room layer. In exchange moq
gains the pieces it verifiably lacks: VAAPI decode and a hardware-validated
VAAPI encoder, V4L2 and Android backends, software AV1, Opus fixes,
subscriber-side adaptive selection, and a playout clock, roughly 9,500 to 10,500
LOC across about 20 PRs (`8-upstream-plan.md`), plus the render stack as an
out-of-tree crate.

Set expectations by scenario, not by the headline. Scenario A (12%) is available
whenever iroh-live bumps to a moq release carrying the merge; it is ordinary
dependency work, not a bet on an unreleased branch, which is the single biggest
change from the round-1 analysis. Scenario B (42%) requires the upstream program
to succeed, which depends on moq maintainer review bandwidth more than on our
authoring. A realistic 12-month expectation sits between the two, and the ~24k
LOC that stays is by design the highest-maintenance code we own. That is honest,
not deflating: the strategic value, upstream ownership of the gaps and alignment
with moq's direction, holds in every scenario.

## What the deep comparison established

The codec and zero-copy investments of iroh-live and moq are complementary, not
duplicate (`3-compare-codecs.md`, `3z-compare-zerocopy.md`):

- moq is ahead on: NVENC and NVDEC end to end, Windows (Media Foundation encode,
  decode, and capture), H.265, VideoToolbox encode, publish-side rate control
  tied to the congestion estimate, GPU transcode (NVDEC to NVENC without a CPU
  round trip), and demand-gated capture.
- iroh-live is ahead on: everything Linux non-NVIDIA (VAAPI encode with DMA-BUF
  input and VPP, VAAPI decode, V4L2 M2M, PipeWire DMA-BUF capture), Android,
  software AV1 (the only AV1 encoder on either side), decode-to-render zero-copy
  (moq decoders download to CPU except NVDEC; we import DMA-BUF, CVPixelBuffer,
  and EGL images straight into wgpu), audio playback and echo cancellation (moq
  has no Rust playback code), subscriber-side adaptive selection (moq Rust has
  none), and the A/V playout clock (same).
- moq's VAAPI encode backend is a 111-line CPU-only placeholder marked "NOT YET
  VALIDATED ON HARDWARE"; ours replaces it outright.

The API divide is open versus closed, not a design disagreement
(`3t-compare-traits-api.md`): moq keeps its `Backend` traits and its `frame::Frame`
enum `pub(crate)` and vendors backends in-tree, while we expose public traits any
crate can implement. Adopting moq's codec stack as-is would delete our zero-copy
capability rather than deduplicate it, because moq's decoders download to CPU and
its frame enum has no DMA-BUF or AHardwareBuffer variant. The decision list in
`3t-compare-traits-api.md` has twelve items (D1 to D12); D1 through D3 gate the
rest, and the two that must land first are D1 (a public platform-handle frame
vocabulary) and D3 (threading PTS through `Backend::encode`, without which our
pipelined V4L2 and Android encoders cannot be expressed).

## The moq-side changes to upstream capture and codec (task of this round)

`3u-moq-changes.md` is the new artifact that outlines, concretely, what moq must
change so we can upstream everything capture and codec related. Its findings:

1. Zero-copy (render and capture). The keystone is a public frame vocabulary: a
   closed `Native` handle enum with `DmaBuf` and `HardwareBuffer` variants added
   beside the existing `Surface`, `Texture`, and `Cuda`, plus a public
   `decode::Frame::native()` accessor so a decoder can hand out its GPU surface
   instead of only `into_i420()`. Capture backends feed the handle, hardware
   encoders consume it, and the renderer imports it. This single change unblocks
   both capture-to-encode and decode-to-render zero-copy.
2. Opening the closed codec `Backend` variants with a trait, like ours. The
   proposal publishes moq's `pub(crate)` `Backend` trait as additive-sealed (it
   only ever carries public vocabulary types, so moq keeps "no backend types in
   the public API"), adds a `register_encoder` / `register_decoder` entry point
   that seeds the existing internal `Candidate` table, and threads PTS through
   `Backend::encode`. The PTS change is small and unconditional; the public trait
   is the one genuinely breaking ask and is needed only for the external-backend
   path.
3. Would moq accept external codecs at all? There is no seam for out-of-tree
   backends today, and moq's posture is to vendor in-tree. The recommendation is
   per backend: VAAPI, V4L2, and AV1 go in-tree (they need only the frame
   vocabulary and PTS, not the public trait), Android MediaCodec is the strongest
   case for the external-registration path because moq cannot test NDK code in
   CI, and the renderer stays out-of-tree over the public handles. This is an
   agenda item for the maintainer, framed with the evidence they need to decide.

## Render: upstream as an out-of-tree crate

The maintainer asked whether the render stack can be upstreamed too.
`3z-compare-zerocopy.md` and `3u-moq-changes.md` independently reached the same
answer: publish it as an out-of-tree `moq-video-render` crate that we maintain,
consuming moq's public frame handles (Option B). All three options (in-tree,
out-of-tree, or keep-in-repo-but-aligned) require the identical moq change, the
public handle vocabulary; they differ only in who carries the wgpu, ash, and glow
dependency tree and the vendor FFI. Option B upstreams the render effort in the
sense that matters (it is public, reusable, and the existence proof that the
public handles suffice) while keeping the hardware-specific code, Vulkan import
and the Intel Y_TILED-to-CCS VPP re-tile, where it is tested on real hardware.
In-tree adoption is correct only once moq CI grows per-vendor GPU runners.
Keeping it in the iroh-live repo fully aligned to moq's frame model is the
minimal fallback if the handle work stalls. Either way, our render code moves
from "keep forever local" to "upstream as a crate once the frame vocabulary
lands", and it is the lead argument for that vocabulary RFC.

## End state of the iroh-live repo

Figures from the `7-cut-plan.md` ledger under Scenario B (moq main adopted plus
upstreams accepted and released).

| Crate | Today | After |
|---|---:|---|
| rusty-codecs | 22,310 | ~6-8k: the render and processing remainder plus anything upstream declines; codec impls and the trait and config mirrors are gone |
| rusty-capture | 5,507 | ~4.3k: Linux backends (PipeWire, V4L2, libcamera, x11) and the facades; Apple, Windows, nokhwa, and xcap gone |
| moq-media | 11,441 | ~10.7k after the ledger cuts, ~9.6k if the adaptive and sync layers shrink to wrappers once their upstreams land: audio backend and AEC, stats, chat, and the controller stay; encode wiring and pipeline glue gone |
| iroh-moq | 572 | ~350-400: the actor keeps dedup, fan-out, and the Router handler; the handshake and ALPN delegate to moq-native |
| iroh-live | 1,734 | similar size, different internals: rooms on moq announce with gossip bootstrap (`9-room-layer.md`) |

The render stack does not vanish from the count so much as relocate: about 3,500
LOC of it becomes the out-of-tree `moq-video-render` crate.

## What moves to moq, and where

Priority order from `8-upstream-plan.md`, which carries the per-wave size
estimates and cross-references `3u-moq-changes.md` for the API design.

1. Wave 1: the D1 and D3 RFC (public frame vocabulary and PTS-through-encode),
   the moq-vaapi hardware-validation report, and goodwill fixes (Opus pre-skip
   and `set_bitrate`, bounded capture channel). Opens the conversation with
   value and leads with the AV1 offer as motivation.
2. Wave 2, the heart of the effort: the public frame vocabulary, the PTS change,
   moq-vaapi growth (surface export, VPP, decode FFI), the VAAPI decode backend,
   the VAAPI encode replacement, and V4L2 M2M encode and decode.
3. Wave 3: software AV1 (rav1e and rav1d), Android MediaCodec with
   AHardwareBuffer, PipeWire DMA-BUF capture delivery, and the pre-encoded-source
   concept.
4. Wave 4: subscriber-side adaptive policy and the switching driver, and the
   playout clock, both building on moq-mux's on-main per-rendition jitter and
   bitrate estimation (`Estimate`) and `recv_bandwidth` rather than adding those
   primitives from scratch.
5. Wave 5, opportunistic: the SPS VUI patcher, the PCM codec offer, Opus channel
   remix, and the out-of-tree `moq-video-render` crate.

## The room layer (task 4)

Rooms move from gossip plus smol-kv announcement onto moq announce: one shared
per-node `origin::Producer`, broadcasts published at `<room>/<endpoint-id>/<name>`
via `create_broadcast(path, Route::announced())`, RoomEvents derived from
`announced()` on a room-scoped consumer, and chat and user metadata in catalog
extension sections. Gossip is retained for bootstrap and presence hints; a
gossip-free variant is specified but deferred until a rendezvous and
membership-repair story exists (`9-room-layer.md`).

Two moq changes since round 1 improve this materially, both now on main. moq-token
path-scoping (`#2416`) closes the announce-spoofing gap the round-1 analysis
flagged as unresolved: a per-peer token rooted at `<room>/<endpoint-id>` makes the
verifier scope the session origin to that prefix, so a peer that tries to announce
under another peer's id fails `Error::Unauthorized`, which gives cryptographic
announce-under-your-own-id and weakens the old argument that gossip's signed KV
state was a decisive advantage. The honest caveat is that this hard-wires
"announce only your own id", which conflicts with transitive relay re-announce
unless relay nodes carry a broader scope. Separately, `#2419` removed the 5s route
linger, so the round-1 "roaming for free via a lingering front" claim no longer
holds: a reconnecting peer re-announces, and the room layer owns a permanent
reconnect debounce rather than disposable code. The phases collapse to work
ordering rather than a release wait, since every primitive they need is on main.

## The gating realities

1. One release gate, not a branch gamble. Every capability the plan depends on is
   on moq main; it lands for us with a single version bump to the release that
   carries the merge (`2-moq-inventory.md`, the "pending release" section). Local
   work that needs no moq change (dead-code deletion, the already-broken catalog
   mirror, transport-handshake delegation, and the announce-based room migration)
   can start against the release we pin today.
2. Upstream acceptance is probable but not assured for the three hardest items:
   moq-vaapi absorbing export, VPP, and decode; in-tree backends for hardware moq
   cannot run in CI (Pi and Android); and the frame vocabulary's home. Per-item
   fallbacks are in `8-upstream-plan.md`.
3. Nothing is cut until its replacement passes an e2e test on the new path, and
   no cut may regress a zero-copy path (`7-cut-plan.md` principles). The
   macOS and Windows CI gap makes this a real prerequisite for the stages that
   switch those platforms.

## Alternatives considered and rejected

Two low-coordination paths were priced against this plan. Doing nothing: keep the
dual stack and track moq-net, hang, and moq-mux releases for transport and
catalog only. Zero cut and zero coordination risk, but the ~21k LOC of codec and
capture code stays duplicated against a stack moq is actively building, the
duplication grows with every release, and we gain no say in the APIs we will
eventually sit on. Forking: fork moq-video and merge our backends into the fork.
This reaches single-stack coherence fastest and has no review gate, but the
divergence is permanent: we own the fork forever, take every upstream improvement
by manual merge, and lose exactly the alignment this effort is for.

Under our constraints (a small team, moq moving fast in our direction, and
differentiators moq verifiably lacks) the recommended path dominates both: unlike
doing nothing it converts duplicate maintenance into shared maintenance, and
unlike the fork it leaves upstream carrying the code we contribute. The fork
becomes the rational fallback only if upstream declines the wave-2 series (the
frame vocabulary and the VAAPI and V4L2 backends), at which point the per-item
keep-local fallbacks of `8-upstream-plan.md` would amount to a scattered fork, and
a deliberate one would be better.

## Reading order

`0-overview.md` (framing), then this doc, then `7-cut-plan.md` and
`8-upstream-plan.md` (the actionable plans), then `3t-compare-traits-api.md` and
`3u-moq-changes.md` (the API discussion agenda and the concrete moq-side change
design), then the remaining comparisons for evidence, then `maps/` for raw source
citations.
