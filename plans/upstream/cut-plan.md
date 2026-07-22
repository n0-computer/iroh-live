# 7. Cut plan: what gets deleted, replaced by moq primitives, in what order

> Campaign: upstream | Kind: reference | Read `0-overview.md` first. This is the
> deletion ledger behind the pair-side counterpart plans in `counterpart/`.

This document is the standing deletion ledger of the upstream campaign: the
per-module verdicts, principles, staging, and risk register that the counterpart
plans (`counterpart/codec-remove.md`, `counterpart/capture-remove.md`,
`counterpart/render-adopt.md`) execute on the paired iroh-live `up/<name>`
branches (`../branches.md`). moq merged its long-lived rewrite branch into main
on 2026-07-21, so there is no dev line to target: every replacement named here is
on moq main, working tree `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`. It
consolidates the verdicts of [1-code-map.md](comparisons/iroh-live-code-map.md),
[2-moq-inventory.md](comparisons/moq-inventory.md), [3-compare-codecs.md](comparisons/codecs.md),
[3z-compare-zerocopy.md](comparisons/zerocopy.md), [3t-compare-traits-api.md](comparisons/traits-api.md),
[3u-moq-changes.md](comparisons/moq-changes.md), [4-compare-capture.md](comparisons/capture.md),
[5-compare-pubsub.md](comparisons/pubsub.md), [6-compare-audio.md](comparisons/audio.md),
and [9-room-layer.md](../align-to-moq/room-layer.md) into a single deletion
ledger and staged sequence. It invents no verdicts; every row cites the document
that established it. The denominator is the 41,564 LOC of the five core crates
(1-code-map.md section 3). Where a row here and the current decisions in
`0-overview.md` differ, the overview governs.

Two adoption scenarios frame the ledger: (A) adopt current moq main as-is, once
iroh-live bumps to a moq release that includes the merged stack, and (B) adopt
plus land our upstream contributions. The purely local wins (dead-code deletion,
the broken config mirror, the transport handshake, the room announce migration)
are the subset of Scenario A that needs no moq change, not a scenario of their
own.

## 1. Principles

**P1. Nothing is cut until its replacement is proven in-tree.** A module is
deleted only after a working example or e2e test passes on the new path in this
repository. The existing gates are `iroh-live/tests/e2e.rs`,
`iroh-live/tests/room.rs`, `moq-media/tests/pipeline_integration.rs`, the
rusty-codecs conformance harness (`rusty-codecs/src/codec/tests/`), and the
hardware-gated `moq-media/tests/zero_copy_pipeline.rs`. Section 5 lists the gaps
that must be closed before specific stages.

**P2. Zero-copy capability is never regressed.** Adopting moq's codec and
capture surface as-is would destroy every iroh-live zero-copy path except macOS
capture-to-encode (3z-compare-zerocopy.md, summary and section 2). Any cut that
would drop a zero-copy path therefore waits for the corresponding upstream
requirement U1 through U4 (3z section 5, drafted concretely in 3u section 5) to
land and release. Concretely: the VAAPI pair waits for U1 (public frame
vocabulary) plus U2 and U3 (VAAPI decode export and encoder GPU input, resting
on moq-vaapi growing surface export and VPP); the VideoToolbox decoder waits for
U1 and U2 (retain the CVPixelBuffer instead of downloading); Android waits for
U1 plus the Section 2 registration or in-tree decision of 3u; PipeWire DMA-BUF
capture is kept outright unless U3 lands with parity.

**P3. One release gate, labeled on every item.** There is a single moq codebase,
so there is no main-versus-dev split to track. Two kinds of cut remain. Local
cuts need no moq change and can
proceed against whatever release iroh-live pins today: dead-code deletion, the
already-broken catalog mirror (`config.rs` no longer compiles against hang
0.19.5, 3t section 4.1), transport-handshake delegation, `Timestamp` adoption,
and the announce-based room migration. Release cuts need the next moq release
that carries the merged stack, plus a version bump across our consumers: every
codec and capture replacement. The enabler register in 2-moq-inventory.md
summary table 2 lists these capabilities; every row it names is on moq main and
lands for us with the release we bump to, so it now reads as the checklist for
that one bump rather than a branch gamble. Every ledger row carries a `local` or
`release` label, plus any Ux or Dx prerequisite.

**P4. API-first.** Cuts that change our trait surface (codec traits, frame
model, timestamps, dispatch) wait for the decision list of 3t-compare-traits-api.md
section 8 (D1 through D12) to be settled with upstream, in particular D1 (public
frame model), D2 (backend extension mechanism), and D3 (PTS threading through
encode), which is the stated precondition for upstreaming the V4L2 and Android
encoders (3t section 5). The concrete moq-side API changes those decisions imply
are drafted in 3u-moq-changes.md. D4 and D5 are local decisions we can execute
immediately.

## 2. The cut ledger

Verdict vocabulary: **cut** (delete, replacement exists on moq main),
**cut-after-upstream** (delete only once the named upstream requirement lands
and releases), **keep** (stays, no upstream counterpart or deliberate
differentiator), **keep-and-upstream-copy** (ours is the stronger
implementation; it moves upstream, and the local copy is deleted only after
upstream acceptance and release), and **merge** (module survives but sheds an
identified portion). LOC figures are from 1-code-map.md section 2. In the
Prerequisites column, `local` means no moq change is needed, `release` means the
bump to a moq release carrying the merged stack.

### rusty-codecs (22,310 LOC)

| Module | LOC | Verdict | Replacement | Prerequisites | Evidence |
|---|---:|---|---|---|---|
| codec/h264/sps.rs | 586 | cut (dead code) | none; offer upstream later as an optional pass | local | 3-compare sec 7 |
| config.rs | 318 | cut | `hang::catalog` types directly | D5 (local decision); mirror already broken against hang 0.19.5 | 3t D5, sec 4.1 |
| codec/h264/ encoder+decoder (openh264) | ~906 | cut | moq-video openh264 encode/decode backends | release | 3-compare sec 1, verdict table |
| codec/h264/annexb.rs | 364 | cut | `moq_mux::codec` (Avcc parse, annexb, param-set injection); park `build_avcc` | release (the decode front end that hosts it) | 3-compare sec 7 |
| codec/vtb/encoder.rs | 895 | cut | moq-video videotoolbox encode (H.265, High profile, per-frame IDR) | release | 3-compare sec 1 |
| codec/vtb/decoder.rs (+mod) | ~599 | cut-after-upstream | moq-video VT decode retaining the CVPixelBuffer | U1, U2, release | 3-compare sec 1; 3z sec 5 U2 |
| codec/vaapi/ | 3,257 | keep-and-upstream-copy | our encoder+decoder move into moq-video/moq-vaapi (DMA-BUF import, VPP, PRIME export) | U1, U2, U3; D2(a); honest `set_bitrate` and forced-IDR plumbing (D7, D8); release | 3-compare sec 1; 3z sec 5; 3u sec 1 |
| codec/v4l2/ | 1,856 | keep-and-upstream-copy | upstream V4L2 M2M encode+decode | D3 (PTS through encode), D2(a), release | 3-compare sec 1; 3t D3; 3u sec 2 |
| codec/android/ | 1,528 | keep-and-upstream-copy | upstream MediaCodec backends plus HardwareBuffer variant | U1, D2(c) or in-tree, D3, release | 3-compare sec 1; 3u sec 4 |
| codec/av1/ | 936 | cut (local rip-out) | none this series; AV1 is deferred upstream (`codec/av1-software.md` stays a deferred plan) and the local backend is dropped with no moq replacement, re-added later only if a use case needs it | local; proof-before-deletion only | 3-compare sec 3; 0-overview Goal and scope |
| codec/opus/ | 804 | cut-after-upstream | moq-audio Encoder/Decoder | upstream runtime `set_bitrate`, lookahead pre-skip fix, and a channel-remap policy; D11 | 3-compare sec 5; 6-compare sec 1.5, 7 |
| codec/pcm/ | 559 | keep, and upstream (required) | `Codec::Pcm` in moq-audio plus the hang catalog PCM variant, same branch (`audio/pcm.md`, pair branch `up/pcm`); local codec stays until that release is pinned | none for the keep; the pcm leaf released for the eventual local cut | 3-compare sec 6; 6-compare 1.4; 0-overview audio leaves |
| codec.rs + codec/dynamic.rs | 522 | cut-after-upstream | moq-video Candidate/Kind dispatch | release, plus every kept backend admitted upstream (U2, U3, Android); `reset()`/`burst_size()` carried into their decode trait | 3-compare sec 8 |
| traits.rs | 410 | merge | codec traits fall away with adoption; device traits (`AudioSink`, `AudioSinkHandle`, `AudioStreamFactory`, `AudioSource`, `VideoSource`) stay local | D1-D3, D11, release | 3t sec 1, D12 |
| format.rs | 1,292 | merge (cut-after-upstream) | shared public frame model (D1, 3u section 1 `Native` sketch); `NativeFrameHandle`/`DmaBufInfo` are the U1 donors | D1, U1 | 3t sec 3; 3z sec 1; 3u sec 1 |
| processing/ | 1,086 | merge | resample.rs converges on `moq_audio::Resampler` plus our remix helper; scale.rs and convert.rs stay serving capture and render | none blocking; moq-audio Resampler on main | 6-compare sec 2 |
| render.rs + render/ | 3,463 | keep, then upstream into moq as the in-tree `moq-video-render` crate | no renderer exists upstream; the code moves to a new moq workspace member over U1 public handles (U4; `render/moq-video-render.md`), a normal workspace member with heavy graphics dependencies behind non-default features and both wgpu and GLES backends behind feature flags | U1 before any upstreaming | 3z sec 4, sec 5 U4; 3u sec 1b; 0-overview render leaves |
| test sources + conformance harness | 2,880 | keep, shrinks with cuts | adopted backends are covered by moq-video's own tests | tracks stages 2 and 3 | 1-code-map sec 2 |
| lib.rs | 8 | keep | | | |

### rusty-capture (5,507 LOC)

| Module | LOC | Verdict | Replacement | Prerequisites | Evidence |
|---|---:|---|---|---|---|
| platform/apple/screen.rs | 394 | cut | moq-video screencapture (app capture, NV12 surfaces, fail-fast TCC) | release | 4-compare sec 2, 5 |
| platform/apple/camera.rs | 81 | cut (stub) | moq-video avfoundation (working, zero-copy, TCC flow) | release | 4-compare sec 2, 5 |
| windows stubs (part of 134) | ~100 | cut (stubs) | moq-video mediafoundation + desktopduplication | release | 4-compare sec 2, 5 |
| platform/nokhwa_impl.rs | 246 | cut after stage 3 | their macOS camera and Windows backends remove its role as the only working path | release; stage 3 complete | 4-compare sec 2, 5 |
| platform/xcap_impl.rs | 175 | cut after stage 3 | same | release; stage 3 complete | 4-compare sec 2, 5 |
| lib.rs + types.rs | 1,107 | merge | selection cascade and backend enum shrink as the Apple and Windows arms migrate (estimate ~250 removed) | stage 3 | 4-compare sec 5 |
| platform/linux/pipewire.rs | 1,655 | keep | none (theirs is CPU-only); port their restore-token replay, static-screen re-pacing, and open-per-demand lifecycle; U3 upstream-later candidate | n/a | 4-compare sec 2, 5; 3z sec 5 U3 |
| platform/linux/v4l2.rs | 552 | keep | none; adopt their zune-jpeg MJPEG shortcut; implement or delete the EXPBUF claim | n/a | 4-compare sec 2, 5 |
| platform/linux/libcamera_h264.rs | 522 | keep | none anywhere upstream; strongest capture upstream candidate | n/a | 4-compare sec 2, 5; 3u sec 3 item 7 |
| platform/linux/x11.rs | 373 | keep | none (their portal-less Linux story is `Unsupported`) | n/a | 4-compare sec 2 |
| platform/linux/libcamera.rs | 268 | keep | none | n/a | 4-compare sec 2 |
| android stub (~34) | 34 | keep | our plan plus frame vocabulary; moq has nothing | n/a | 4-compare sec 2 |

### moq-media (11,441 LOC)

| Module | LOC | Verdict | Replacement | Prerequisites | Evidence |
|---|---:|---|---|---|---|
| publish.rs | 1,508 | merge | simulcast registry, `SharedVideoSource`, and leasing stay (no moq equivalent); `start_track` per-track wiring collapses onto `encode::Producer`; priming hack replaced by `Reserved` semantics | codec adoption (stage 2), D9 track naming, release `Reserved` | 5-compare sec 2, 10 |
| pipeline/ | 1,212 | merge | encode pipelines (~404 LOC) collapse onto `moq_video`/`moq_audio` `encode::Producer`; decode loops stay, internals swap to the sans-IO `moq_video::decode::Decoder` under our OS threads | codec adoption, release | 5-compare sec 2, 10; 3t sec 7 |
| subscribe.rs | 1,566 | keep (merge at the edges) | quality selection, hot-swap, and adaptation driver have no upstream counterpart; decoder-prep internals swap on adoption | release for `set_latency`, `discontinuity()` | 5-compare sec 3, 10 |
| transport.rs | 204 | merge | `MoqPacketSink` half replaced by `encode::Producer`; `MoqPacketSource` stays feeding the sans-IO decoder | codec adoption, release | 5-compare sec 2; 3t sec 7 |
| audio_backend.rs + aec.rs | 2,837 | keep-and-upstream-copy | no moq equivalent (playback, mixing, fades, AEC, recovery); the playback sink and AEC engine unify into moq-audio behind features (`audio/audio-device-unify.md`, pair branch `up/audio-device`); meanwhile adopt their capture surface (system audio, TCC, `format()` without open) onto our bounded buffers | the audio-device-unify leaf merged and released for the local cut | 6-compare sec 3, 7; 0-overview audio leaves |
| adaptive.rs + net.rs | 621 | keep, then upstream | the missing Rust subscriber-side ABR; policy first, toward moq-mux catalog or a small crate; builds on moq-mux `Metrics` bitrate/jitter | release only viable upstream | 5-compare sec 5, 10 |
| sync.rs + playout.rs | 512 | keep, then upstream | no Rust playout clock upstream; target moq-mux next to `container::Consumer`; read catalog `jitter` now populated by moq-mux `Metrics` | local fixes need nothing | 5-compare sec 6, 10 |
| stats.rs | 494 | keep | does not overlap moq-stats or moq-net session stats | n/a | 5-compare sec 7 |
| source_spec.rs | 499 | keep | CLI parsing, ours | n/a | 1-code-map |
| frame_channel.rs | 299 | keep | enables decoder hot-swap; no counterpart | n/a | 5-compare sec 3 |
| publish/controller.rs | 322 | keep | app-facing orchestration | n/a | 5-compare sec 2 |
| audio_file_* | 472 | keep | decoded-PCM sources and moq-mux importers are complementary | n/a | 6-compare sec 4 |
| chat.rs | 182 | keep | none upstream, Rust or JS | n/a | 5-compare sec 8 |
| catalog.rs | 75 | keep | already the sanctioned `CatalogExt` shape; the floor | n/a | 5-compare sec 4 |
| processing.rs + mjpg | 87 | keep | | | 1-code-map |
| lib/util/capture + test_util | 551 | keep | | | 1-code-map |

### iroh-moq (572 LOC)

| Module | LOC | Verdict | Replacement | Prerequisites | Evidence |
|---|---:|---|---|---|---|
| lib.rs handshake + ALPN (~200) | ~200 | cut | `moq_native::iroh` connect/accept plus `moq_net::{Client, Server}`; full `moq_net::ALPNS` list registered and offered | local; wire-visible, e2e re-run required | 9-room-layer sec 1.1, 3.1, phase 1 |
| lib.rs actor (dedup, origin fan-out, `ProtocolHandler`, incoming stream) | ~370 | keep | no moq-native equivalent; fan-out reshapes onto one shared `OriginProducer` | phase 1 | 9-room-layer sec 3.1, 6 |

Grouping note: 1-code-map, 2-moq-inventory, and 9-room-layer size the strictly
duplicated handshake at roughly 120 LOC and the actor core at roughly 200. The
two rows here instead partition the whole 572-line file: the cut row (~200) is
the duplicated handshake plus the ALPN constant and connect/accept glue that
moq-native delegation obsoletes; the keep row (~370) is the actor core plus the
public session/stream wrappers (`Moq`, `MoqSession`, `IncomingSession`) around
it. The scenario totals below use the ~200 partition figure.

### iroh-live (1,734 LOC)

| Module | LOC | Verdict | Replacement | Prerequisites | Evidence |
|---|---:|---|---|---|---|
| rooms.rs | 695 | merge | KV half (~200-250 LOC) replaced by scoped moq `announced()` streams; gossip retained for bootstrap (Variant A); event derivation added (~100-150 LOC); unannounce debounce added locally, deleted once we bump to the release carrying #2241 | phase 1 first; local suffices for phase 2 | 9-room-layer sec 3, 6 |
| util.rs | 185 | merge | `available_bps` cwnd math (~20 LOC) replaced by `moq_net::bandwidth` `recv_bandwidth`; PathStats kept for loss and congestion | release | 5-compare sec 7 |
| live.rs, call.rs, subscription.rs, rooms/publisher.rs, ticket.rs, lib/types | 854 | keep | public API layer, tickets, sugar | n/a | 9-room-layer sec 6 |

### Expected LOC removed, two scenarios

Sums are ledger-row estimates, cumulative, rounded; treat as +/-15%.

| Crate | Scenario A: adopt moq main as-is (bump) | Scenario B: adopt + our upstreams accepted |
|---|---:|---:|
| rusty-codecs | 3,069 (sps.rs, config.rs, openh264, annexb, VTB encoder) | ~15,100 (+VAAPI, V4L2, Android, AV1, Opus, VTB decoder, dispatch, trait and frame-model halves, ~1,200 test shrink) |
| rusty-capture | ~1,250 (Apple screen+camera, Windows stubs, nokhwa, xcap, cascade shrink) | ~1,250 |
| moq-media | ~110 (priming hack via Reserved, set_latency path, stale docs) | ~700 (+encode wiring, transport sink half) |
| iroh-moq | ~200 (handshake, ALPN) | ~200 |
| iroh-live | ~165 (rooms KV half, debounce, bandwidth math, net of additions) | ~165 |
| **Total** | **~4,800 (12%)** | **~17,400 (42%)** |

Reading of Scenario A. It is the ordinary outcome of a version bump, not a
branch gamble. Every codec and capture replacement it counts exists today on
moq main at `3a3e0ea8` and ships in the next release iroh-live can pin (3-compare
final section; 4-compare section 5). The purely local wins (dead code, the
broken config mirror, the transport handshake, the room announce migration) are
folded into this column rather than standing as a separate scenario, because
they are simply the subset of Scenario A that needs no moq change. Scenario B
leaves about 24,000 LOC in-tree, dominated by the deliberate keeps of section 4
(the audio engine, Linux capture, adaptive, sync, rooms, tests, and the render
stack, the last of which leaves the core crate for moq's in-tree
`moq-video-render` crate rather than staying local forever).

Churn and rework accounting. The scenario sums count deletions only; known
offsets in the other direction are: the bump to the merged-stack release forces
a migration across all our consumers (the module renames and `#[non_exhaustive]`
sweep touch most `moq_lite` paths); the stage 0 transport delegation
(9-room phase 1) builds on moq-native's iroh accept, and the merged main has
collapsed the older two-phase accept to one phase, so part of that work is known
rework at bump time; the phase 2 unannounce debounce is throwaway by design,
deleted in phase 3; and any stage 2 bridge code (see the stage 2 bridge-cost
paragraph; the recommended atomic variant avoids most of it) is written to be
deleted. Individually small, together these mean the net LOC and effort picture
is somewhat worse than the table suggests; in particular the 42% headline is a
gross figure, and the net saving subtracts whatever temporary bridge and flag
machinery the chosen stage 2 ordering incurs.

## 3. Ordering and dependency graph

Stages are sequential by default; stage 5 phase 2 can run in parallel with
stages 1 through 4 because rooms and media do not couple (9-room-layer sec 5,
migration sequencing). Each stage states its entry condition and what breaks if
started early. No stage is dev-dependent anymore; the codec, capture, and
pubsub stages are release-dependent on the single bump, and the rest are local.

**Stage 0: quick wins (local).**
Content: delegate the iroh-moq handshake to moq-native and register the full
ALPN list (9-room-layer phase 1); delete `sps.rs` dead code and the misleading
V4L2 EXPBUF doc claim (3-compare sec 7; 4-compare sec 2); fix the stale sync.rs
jitter-field claims and the stale `adaptation_task` doc comment, and read
catalog `jitter` into the clock (5-compare sec 6, 10); report the moq-net
dynamic-handler registration race and the catalog-priming sharp edge upstream
(5-compare sec 2; the priming hack itself is only properly replaced by
`Reserved`, so its removal is deferred to stage 4). Entry condition: none.
Breaks if done early: nothing; but the ALPN change is wire-visible, so
`e2e.rs` and `room.rs` must pass before and after (9-room-layer phase 1).

**Stage 1: type convergence (local, D4 and D5).**
Content: adopt `moq_net::Timestamp` end to end in place of `Duration`
(format.rs, pipelines, sync.rs; 3t D4), and delete the rusty-codecs catalog
mirror `config.rs` in favor of direct `hang::catalog` types (3t D5, and the
mirror no longer compiles against hang 0.19.5). Entry condition: D4/D5 confirmed
locally (they need no upstream buy-in; 3t sec 8). Breaks if done early: nothing
structural, but doing it after stage 2 would force every codec adoption diff to
carry conversion shims, so it goes first.

**Stage 2: codec adoption, per platform (release-dependent).**
Content, ordered per the atomic recommendation of the bridge-cost paragraph
below (Windows and other unheld platforms adopt immediately; platforms with
held backends flip whole): adopt moq-video openh264 encode/decode, the
VideoToolbox encoder, and the bitstream front end (annexb/Avcc), and gain
NVENC/NVDEC, Media Foundation, H.265, and `rate::Control` outright (3-compare
verdict table). Hold VAAPI, V4L2, and Android on our implementations until U2/U3,
D3, and the Android decision respectively land upstream (P2, P4); hold the VTB
decoder until U2; hold Opus until its three upstream items land (6-compare sec 7).
Entry condition: the moq release shipping the merged media stack and a version
bump, plus D1 through D3 settled with upstream for the held backends, plus the
platform verification gate of R-g for every platform whose backend is switched.
Breaks if done early: cutting before the release means building against a moving
git pin; cutting the held backends drops Linux Intel/AMD hardware decode, ARM
SoC support, Android entirely, and the decode-to-render zero-copy inputs (3z
sec 2, path-by-path). Gate: the conformance harness and `pipeline_integration.rs`
pass with the adopted decoders; latency tests do not regress.

Bridge cost. Under the item list above as written, the repository runs a mixed
codec stack for the duration of this stage: adopted backends use their frame and
config types, held backends (VAAPI, V4L2, Android, VTB decode, Opus) stay on
ours, and the pipelines convert at every crossing. From the 3t frame and config
mapping, that bridge is on the order of 300-600 LOC of temporary conversion
code, on top of the feature-flag machinery of section 6 and a doubled test
matrix while both paths exist; all of it is written to be deleted. The
alternative is atomic per-platform switchover: hold each platform entirely on
our stack until every backend for that platform can flip at once (macOS when the
VTB pair including U2 is ready, Linux non-NVIDIA when the VAAPI and V4L2 series
land). That trades a longer dual-stack period in-repo for never mixing frame
models within one platform, and makes each platform switch independently
revertible. This plan recommends the atomic variant for platforms with held
backends (Linux, macOS, Android), and immediate adoption only where a platform
has no held backend and no working local path today (Windows via NVENC, Media
Foundation, and H.265, which is pure gain with nothing to bridge): the
interleaved ordering's early adoptions mostly add capability we do not ship
today rather than deleting local code, so the bridge and doubled tests buy
little calendar time. Under the atomic variant parts of Scenario A shift later in
time; the end totals are unchanged.

**Stage 3: capture adoption (release-dependent).**
Content: adopt moq-video capture for macOS camera, macOS screen, and both
Windows backends; retire nokhwa, xcap, and the Windows stubs afterward; keep
the entire Linux column (PipeWire, V4L2, libcamera, libcamera_h264, X11) and
the Android plan (4-compare sec 5). Adopt the moq-audio capture surface (system
audio, TCC flow, `format()` without open, demand gating) onto bounded buffers,
explicitly not importing their unbounded realtime channel (6-compare sec 3.3, 7).
Entry condition: stage 2 on the same platform, because their capture emits their
frame model into their encoders; adopting capture before codecs would need a
throwaway conversion layer; and the platform verification gate of R-g for macOS
and Windows. Breaks if done early: macOS camera currently works only through
nokhwa, so removing it before the AVFoundation backend is proven leaves macOS
camera dead.

**Stage 4: pubsub and adaptive re-plumb (release-dependent).**
Content: collapse the encode pipelines and `start_track` wiring onto
`encode::Producer` (needs D9 deterministic track naming for simulcast); replace
the catalog priming hack with `Reserved` gating; adopt publish-side `Metrics`
so advertised bitrate and jitter are measured; adopt `set_latency` for runtime
`PlaybackPolicy` and `discontinuity()` for clean decoder flushes; replace the
`available_bps` cwnd math with `recv_bandwidth` (5-compare sec 2, 3, 7, 10).
Entry condition: stage 2 (the producers assume their codec layer), the release
bump, D9 answered. Breaks if done early: without D9 the simulcast rendition
registry cannot name tracks deterministically, and without `Reserved` early
subscribers race the catalog (5-compare sec 2).

**Stage 5: rooms (9-room-layer phases 2 and 3).**
Content: phase 2, announce-based rooms on the shared origin with gossip
retained for bootstrap (Variant A), smol-kv removed, metadata moved to catalog
extensions, and an unannounce debounce standing in for migration; phase 3, on
the release bump, delete the debounce in favor of #2241 migration and
`ROUTE_LINGER`, adopt `AnnounceOk` roster completion, `Publish` guards, and
optionally moq-stats (9-room-layer sec 6). Entry condition: stage 0 (shared
`OriginProducer` wiring) for phase 2; the release bump for phase 3. Breaks if
done early: phase 2 before phase 1 has no origin to scope; phase 3 before the
bump has no migration to lean on, so removing the debounce reintroduces
join/leave flapping on reconnect.

Dependency summary: stage 0 gates 5; stage 1 gates 2; stage 2 gates 3 and 4;
U1 gates U2 through U4, which gate the held cuts inside stages 2 and 3 (3z sec 5,
ordering note); D3 gates the V4L2 and Android upstreams and hence their eventual
local deletion (3t D3).

Merged dependency table. One vocabulary for the cut stages here, the
contribution waves (current wave ordering in `0-overview.md`; the C-numbered
catalog below is from `analysis/refactor-upstream-plan.md`), the zero-copy
requirements U1-U4 (3z sec 5, drafted in 3u sec 5), and the API decisions
(3t sec 8). Identical items are merged: C2 = U1 =
D1 (public frame vocabulary), C3 = D3 (PTS through encode). The single
release-dependent gate is the bump to a moq release carrying the merged stack.

| Item | Blocks | Blocked by |
|---|---|---|
| Stage 0 (quick wins) | stage 5 phase 2 | none |
| Stage 1 (Timestamp, config.rs; D4/D5) | stage 2 | none |
| Stage 2 (codec adoption) | stages 3, 4 | merged-stack release + bump; D1-D3 settled; R-g platform gates; wave 2 merges for the held cuts |
| Stage 3 (capture adoption) | nokhwa/xcap cuts | stage 2 per platform; R-g platform gates |
| Stage 4 (pubsub/adaptive re-plumb) | priming-hack cut | stage 2; release bump; D9 |
| Stage 5 phase 2 (announce rooms) | stage 5 phase 3 | stage 0 |
| Stage 5 phase 3 (release rooms) | none | release bump |
| Wave 1 (RFC + goodwill PRs) | wave 2 (velocity gate, analysis/refactor-upstream-plan.md sec 5) | none |
| Wave 2 (C2, C3, C1a-c, C4) | VAAPI and V4L2 local cuts | D1/D3 RFC accepted; wave-1 review velocity |
| Wave 3 (C5, C11, C12; C6 deferred with AV1) | Android cuts; capture-adjacent cuts (the AV1 cut is a local rip-out, unblocked) | C2, C3; D2 decision (C5) |
| Wave 4 (C8, C9, C10) | adaptive.rs/sync.rs shrink to wrappers | release bump |
| C2 = U1 = D1 (frame vocabulary) | U2-U4, C1, C5, C11, C13 | RFC acceptance |
| C3 = D3 (PTS through encode) | C4, C5; V4L2 and Android cuts | RFC acceptance |
| U2/U3 (moq-vaapi export, VPP, decode/encode GPU paths) | codec/vaapi/ cut | U1; C1a merge |
| U2 (VT decode keeps CVPixelBuffer) | codec/vtb/decoder.rs cut | U1 |
| U3 (PipeWire DMA-BUF upstream) | optional PipeWire capture cut | U1 |
| Android in-tree or D2(c) registration | codec/android/ cut | U1; D2 |
| U4 (in-tree moq-video-render workspace crate) | render/ leaves the core crate | U1 |
| D2 (backend extension mechanism) | Android in-tree vs registration | RFC discussion (3u sec 2, sec 4) |
| D9 (deterministic track naming) | stage 4 simulcast wiring | release discussion |

## 4. What is explicitly NOT cut

Each keep with its reason and long-term disposition.

| Kept | Why | Disposition |
|---|---|---|
| render.rs + dmabuf_import + gles + gles_dmabuf + metal_import (3,463) | moq has no renderer, no import machinery, no decode-to-render GPU handoff on any platform (3z sec 2b, 2d) | move into moq as the in-tree `moq-video-render` workspace crate over U1 public handles (U4; `render/moq-video-render.md`): a normal workspace member with heavy graphics dependencies behind non-default features, both wgpu and GLES backends behind feature flags; not kept in iroh-live forever |
| DMA-BUF / Metal / GLES / HardwareBuffer import paths specifically | our strongest asset; the only such paths in either codebase, on three platforms and two graphics APIs (3z sec 2b) | carried forward into the render crate; only preservable by us until U1 exists |
| audio_backend + AEC (2,837) | playback, mixing, declicker fades, metering, device switching, restart with backoff, and sonora AEC have zero moq counterpart (6-compare sec 3.4, 7) | in scope of this campaign: the playback sink and AEC engine unify into moq-audio behind features (`audio/audio-device-unify.md`, pair branch `up/audio-device`); the local copy is cut only after that leaf merges and releases |
| adaptive.rs + net.rs (621) | no subscriber-side ABR exists in moq Rust, JS only (5-compare sec 5) | keep now, upstream the pure policy building on moq-mux `Metrics` bitrate/jitter |
| sync.rs + playout.rs (512) | no Rust playout clock upstream; moq-audio defers `latency_min` (5-compare sec 6) | keep now, upstream toward moq-mux next to `container::Consumer`; the producing half (catalog jitter) is already populated by moq-mux `Metrics` |
| PCM codec (559) | required capability: uncompressed path for latency measurement, tests, and diagnostics (3-compare sec 6; 0-overview audio leaves) | upstreamed as required: `Codec::Pcm` in moq-audio plus the hang catalog PCM variant in the same branch (`audio/pcm.md`); the local codec stays until that release is pinned, so the capability is never lost |
| libcamera + libcamera_h264 (790) | the Raspberry Pi story; the pre-encoded H.264 source is unique on either side (4-compare sec 2) | keep; upstreamed as `capture/libcamera-preencoded.md` (required; the open question on the `publish_preencoded` shape is discussed there, current proposal: mirror `publish_capture` minus `encode::Options`) |
| PipeWire capture (1,655) | theirs is CPU-only; ours delivers DMA-BUF into VAAPI (4-compare sec 2) | keep; DMA-BUF delivery is upstream candidate U3 |
| V4L2 + X11 capture (925) | enumeration, negotiation, NV12 passthrough, portal-less coverage (4-compare sec 2) | stay |
| stats sampling (stats.rs, util.rs PathStats) | does not overlap moq-stats; loss and congestion signals have no upstream source (5-compare sec 7) | stays; ask upstream for loss/congestion beside `recv_bandwidth` |
| chat + IrohLiveExt catalog extension (257) | no chat anywhere upstream; `CatalogExt` is the sanctioned mechanism (5-compare sec 4, 8) | stays forever; do not upstream |
| room actor, live, call, tickets (~1,220 after stage 5) | genuinely iroh-specific (Router handler, dedup, gossip bootstrap, tickets) (9-room-layer sec 6) | stays forever |
| frame_channel, SharedVideoSource, simulcast registry, controller | enable hot-swap and simulcast that moq lacks (5-compare sec 2, 3) | keep; "N producers over one shared source" is an upstream candidate |

## 5. Risk register

**R-a. Release timing.** The whole enabler stack is on moq main at `3a3e0ea8`
(dev merged into main on 2026-07-21), so adoption depends on a single ordinary
release plus a version bump rather than on a branch ever merging. What remains
is only the normal wait
for the next release of moq-net, moq-mux, hang, moq-video, and moq-audio, and
release-plz drives those on a predictable cadence (2-moq-inventory summary
table 2, now a bump checklist rather than a gamble). Mitigation: stages 0, 1,
and 5 phase 2 are local and proceed regardless of the release.

Slip scenario, stated as the floor: if the next release slips, the plan still
yields the stage 0 and 1 local cuts, rooms phases 1 and 2, the wave-1 goodwill
PRs, and the D1/D3 RFC conversation. Everything past it is contingency-scheduled
on the one release, not on a branch merge. A hedge branch that continuously
compiles our stage-2 adoption shims against moq main HEAD converts silent plan
rot into visible build breaks, and it can merge as soon as the release ships,
since it tracks main rather than an unmergeable dev line.

**R-b. moq API churn between `3a3e0ea8` and the release.** The merge landed a
large rewrite, and post-merge main is still settling (module boundaries,
`#[non_exhaustive]` sweep, wire-version constants), and the `to_json` rename
class of change has bitten us before (5-compare sec 4; project memory, migration
gotchas). Mitigation: do not start stage 2 against a git pin; treat all
citations pinned to `3a3e0ea8` as direction, not API contract.

Plan freshness protocol. The analysis is pinned to moq main `3a3e0ea8`
(2026-07-21). Signature-level drift by release (renames, field and trait
changes) is expected; structural drift (frame model, candidate tables,
Producer/Consumer shapes) is less likely now that the rewrite has landed but
still possible. Before any stage here, and before any wave of the upstream
campaign (`0-overview.md`, Wave ordering), starts: re-diff `3a3e0ea8` against
the then-current main or the eventual release,
re-validate the enabler register of 2-moq-inventory section summary table 2, and
re-confirm the affected ledger rows and sketches. No citation in this plan set
is an API contract until that re-check passes.

**R-c. Upstream acceptance of U1 through U4 and D1 through D3.** U1 is the
keystone; U2 and U3 additionally require moq-vaapi to grow surface export and
VPP, "the largest single piece of upstream work and the one most likely to meet
resistance" (3z sec 5, 3u sec 4); Android is a plausible decline that pushes it
onto the Section 2 registration path (3u sec 4). Mitigation: the held backends
stay in-tree indefinitely under keep-and-upstream-copy. Scenario A is an
acceptable waypoint and a comfortable one, since it is a plain version bump; it
is only Scenario B that depends on acceptance. If declined contributions are
then kept as local forks of moq's vocabulary, the result is the mixed-stack
world the fallback section of `analysis/refactor-upstream-plan.md` warns about,
which is why the wave-1 velocity gate and the pilot exist.

**R-d. The rav1d git-fork pin.** moq pins crates.io versions and runs
release-plz; a git dependency will not be accepted, so any AV1 upstream is gated
on publishing the pin, moving to a released rav1d, or vendoring (3-compare
sec 3; 3u sec 4). AV1 is deferred upstream this series and the local backend is
ripped out with no moq replacement (`counterpart/codec-remove.md`); resolving
the pin is a precondition for the deferred `codec/av1-software.md` plan, not a
gate for any local deletion.

**R-e. The cpal git pin.** moq-media's audio_backend uses a git-pinned cpal
while moq-audio uses the same crate from crates.io (1-code-map sec 4;
6-compare header). Any shared audio-capture layering must reconcile the pin or
the two cpal versions will conflict in one dependency graph.

**R-f. Behavioral differences discovered during adoption.** Known deltas to
verify at each gate: their openh264 output is Annex-B avc3 only, no avcC
(3-compare sec 1); their Opus uses `OPUS_APPLICATION_AUDIO` versus our VOIP and
a zero pre-skip OpusHead (6-compare sec 1.1); their VT encoder uses High profile
versus our Baseline (3-compare sec 1); their video rendition registers in the
catalog only after the first SPS versus our register-up-front (3t sec 1); their
capture drops oldest at depth 4 versus our backpressure at depth 2 (4-compare
sec 1); their mic capture uses an unbounded channel we must not inherit
(6-compare sec 3.3).

**R-g. Test coverage gaps.** What exists: `e2e.rs` (4 tests) and `room.rs`
(6 tests) in iroh-live, `pipeline_integration.rs` (47 fns) and the
hardware-gated, `--ignored` `zero_copy_pipeline.rs` in moq-media, `camera.rs`
and `pipewire_reopen.rs` in rusty-capture, and the conformance harness with
vectors, latency, and metrics tests inside rusty-codecs. Gaps against P1: no CI
coverage on macOS or Windows, so stage 2 VTB adoption and all of stage 3 have
no automated gate today; the zero-copy e2e runs only on Intel Linux hardware by
hand; there is no adaptive-switching integration test driving
`adaptation_task_v2` end to end for stage 4; and the ALPN change in stage 0
relies on `e2e.rs`/`room.rs` alone.

Platform verification gate (entry condition for stages 2 and 3). P1 is
unenforceable on a platform we cannot test, so macOS and Windows CI, or at
minimum scripted on-hardware verification runs (checked-in scripts with
recorded results per run), is an explicit prerequisite for every stage that
switches those platforms: before stage 2, for the VTB encoder swap and any
Windows backend; before stage 3, camera and screen open/close smoke tests per
adopted backend on the same footing. Before stage 4: an integration test
pinning rendition switching and catalog `Reserved` behavior. The same gap
weakens the hardware-validation offers of the campaign (the C1, C4, and C14e
items of `analysis/refactor-upstream-plan.md`):
since we cannot run those checks in anyone's CI, every validation report we
send upstream must carry reproducible scripts and exact environment versions
so the results can be re-run without us.

## 6. Commit strategy

Per the workspace workflow (conventional prefixes, `cargo make check-all`
before every commit, no doc-only commits, no push without an explicit ask):

- Each stage is a series of small compiling commits on a feature branch, one
  concern per commit (`refactor:` for delegation and type convergence,
  `feat:` for adopted capability, `chore:` for deletions), with the stage's
  gate tests green at every commit.
- Where old and new paths must coexist (stage 2 codecs, stage 3 capture), the
  transition is feature-flagged: a `moq-native-codecs` style cargo feature adds
  the new path first, tests run against both, the default flips in its own
  commit, and the old path is deleted in a final commit only after the gate of
  P1 passes on the new default. Deletion commits contain nothing else, so a
  revert restores the old path cleanly.
- Keep-and-upstream-copy modules never get a local deletion commit until the
  upstream release containing them is pinned in `Cargo.toml`; the deletion
  commit and the version bump travel together.
- Wire-visible changes (stage 0 ALPN, stage 5 announce paths) land as single
  commits with the e2e evidence noted in the commit message, since they are the
  revert points if interop breaks.
- Doc updates (stale sync.rs claims, EXPBUF) ride the code commits that touch
  the same area, never standalone.
