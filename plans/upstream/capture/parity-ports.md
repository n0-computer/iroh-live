# parity-ports. Our fixes ported into moq's adopted backends

> Campaign: upstream | Kind: leaf plan | Branch: up/parity-ports |
> PR target: moq monorepo | Read ../0-overview.md first.
Depends on: none (the one port item is independent of the base API); the
cross-referenced items are owned by their named sibling leaves
Path: A (in-tree)
Size: S

## Goal

`DISPOSITION.md` marks nine module groups adopt-theirs: openh264 encode and
decode, the VideoToolbox encoder, the bitstream tooling, the dispatch layer,
the catalog mirror, the resampler, macOS screen and camera capture, and the
Windows capture stubs, plus the moq-audio capture surface on the audio side.
This leaf closes the loss channel those adoptions open: before any adopt-theirs
module is deleted, everything our implementation does that moq's winning
implementation does not must be either ported upstream as a small independent
PR (an item here), owned by a named sibling leaf, or explicitly parked with a
reason. The mining result, stated up front: the adopt-theirs backends carry
almost nothing of ours to port, because moq won exactly where our
implementations were weak or absent (stubs on Windows and the macOS camera, no
retune on our openh264, Baseline-only on our VideoToolbox encoder, nothing at
all opposite NVENC, NVDEC, and Media Foundation). One real port item survives
(the moq-audio capture channel discipline), two capability groups are parked
with reasons, five are owned by sibling leaves, and the rest are explicit
nothing-to-port verdicts. The nothing-to-port rows are as much the deliverable
as the port: they are the evidence the counterpart cuts cite.

## Evidence

- Per-backend capture verdicts: `../comparisons/capture.md` section 2 (the
  verdict table and per-backend prose) and section 5 (the final per-backend
  list).
- Per-codec verdicts: `../comparisons/codecs.md` sections 1, 7, 8, and the
  section 10 verdict table.
- The audio capture channel finding: `../comparisons/audio.md` section 3.3
  (their unbounded realtime-to-async channel against our bounded-buffer
  discipline).
- Coverage: the campaign's coverage audit confirmed every ours-ahead
  capability is either upstreamed by a leaf or accounted for below.

## moq API consumed

None of the frozen base contract. The single port item (P1) changes moq-audio
capture internals only; no public signature moves.

## Source to port

### Items this leaf ports (small independent PRs)

| Id | Fix | Ours (verified) | Target in moq | Size |
|---|---|---|---|---|
| P1 | Bounded capture channels: moq-audio forwards realtime audio buffers over unbounded tokio channels, so a stalled consumer grows memory without bound, and the microphone path allocates a `Vec<f32>` per callback on the realtime thread. Our engine moves samples through preallocated lock-free ring buffers and bounds every control channel. | Discipline to port: `moq-media/src/audio_backend.rs:1785` and `:1812` (preallocated `resampling_channel` endpoints), `:168` and `:1448` (bounded command channels) | `rs/moq-audio/src/capture.rs:140` (the `UnboundedReceiver` field), `:162` (`mpsc::unbounded_channel::<Vec<f32>>`), `:230` (the `forward` sender), and `rs/moq-audio/src/capture/screencapture.rs:100` (the system-audio path's unbounded channel) | S (~40 LOC plus a test per path) |

P1 is the only place in the adopt-theirs surface where our implementation
carries a robustness property theirs lacks and no sibling leaf owns the fix.
moq-video already solves the same problem for video frames with a bounded
drop-oldest channel (`rs/moq-video/src/capture/channel.rs:19` and `:43-51`,
DEPTH 4, oldest dropped to favor latency, pinned by the `drops_oldest_when_full`
test), so the PR brings moq-audio capture to moq's own established discipline
rather than importing ours wholesale.

### Ours-ahead capabilities owned by sibling leaves (not this plan)

| Capability | Ours (verified) | Owning leaf |
|---|---|---|
| GPU-resident VideoToolbox and Media Foundation decode output (theirs downloads every decoded frame to CPU I420) | `rusty-codecs/src/codec/vtb/decoder.rs:47-56` (retained `CVPixelBuffer` queue) | `../codec/vtb-mf-decode-surface.md` |
| Opus runtime `set_bitrate`, lookahead-derived pre-skip, FEC/DTX ctl plumbing, decoder channel remix | `rusty-codecs/src/codec/opus/encoder.rs:206-219`, `:91-108`; `opus/decoder.rs:136-186` | `../audio/opus-improvements.md` |
| SPS VUI low-latency patcher | `rusty-codecs/src/codec/h264/sps.rs:1-13` | `../codec/bitstream-sps-vui.md` |
| Decoder `reset()` and `burst_size()` needs carried into moq's decode trait | `rusty-codecs/src/codec/vaapi/decoder.rs:731`, `:616` | `../codec/vaapi-decode.md` |
| Playback sink, mixing, and AEC engine beside the adopted capture surface | `moq-media/src/audio_backend.rs`, `audio_backend/aec.rs` | `../audio/audio-device-unify.md` |

### Parked, recorded here so the loss is deliberate

| Capability | Ours (verified) | Why parked |
|---|---|---|
| avcC (avc1) production: the record builder, the openh264 priming-frame extraction, and the VideoToolbox callback assembly | `rusty-codecs/src/codec/h264/annexb.rs:89-111` (`build_avcc`); `codec/h264/encoder.rs:128-142` and `:274-284`; `codec/vtb/encoder.rs:609-612` and `:701-727` | moq emits Annex-B in-band (avc3) only and its container export needs are covered by moq-mux; an avc1 output mode has no upstream consumer today (`../comparisons/codecs.md` sections 1 and 7). Revisit only if moq wants avc1 output. |
| Decoder-side presentation conveniences: RGBA/BGRA conversion with a reused buffer and `set_viewport` post-scale | `rusty-codecs/src/codec/h264/decoder.rs:113-147`, `:83-85` | Presentation logic, not decode. moq's `Config::resize` and the render crate (`../render/moq-video-render.md`) are the right homes (`../comparisons/codecs.md` section 8). |

### Nothing to port, made explicit

Each row below is an adopt-theirs surface where the comparison found no fix,
robustness improvement, or capability of ours worth carrying upstream. The
citation is the verdict that establishes it.

| Module | Why nothing ports | Evidence |
|---|---|---|
| openh264 encode | Theirs adds tested live retune (deferred-application FFI) and per-frame forced IDR; our only extras are the parked avcC mode and embedded scaling, which moq deliberately leaves to the caller. | `../comparisons/codecs.md` section 1, openh264 verdict ("Nothing needs upstreaming on this backend") |
| openh264 decode | Parity on decode itself; their front-end avc1 layering (one conversion for all backends) beats our per-backend handling; our extras are the parked presentation pieces. | `../comparisons/codecs.md` section 1 |
| VideoToolbox encode | Theirs wins on H.265, High profile, per-frame IDR, and ExpectedFrameRate; ours has no encode-side advantage beyond the parked avcC mode. | `../comparisons/codecs.md` section 1, VideoToolbox verdict |
| NVENC | Theirs only; we have no NVENC code. | `../comparisons/codecs.md` section 1 |
| NVDEC | Theirs only. | `../comparisons/codecs.md` section 1 |
| Media Foundation encode | Theirs only; our `media-foundation` feature is an empty placeholder. | `../comparisons/codecs.md` section 1 |
| Media Foundation decode | Theirs only; the surface-retention improvement is owned by `../codec/vtb-mf-decode-surface.md`, not this plan. | `../comparisons/codecs.md` section 1 |
| Dispatch and selection | Their `Candidate`/`Kind` table wins outright; our `reset()`/`burst_size()` needs travel with `../codec/vaapi-decode.md`. | `../comparisons/codecs.md` section 8 |
| Catalog config mirror | Replaced by direct `hang::catalog` types; the mirror no longer compiles against hang 0.19.5 and carries nothing upstream. | `../comparisons/codecs.md` final section |
| Resampler | Their wrapper is leaner and handles partial input with preallocated scratch; our remix helper travels with `../audio/opus-improvements.md`. | `../comparisons/audio.md` section 2 |
| macOS screen (ScreenCaptureKit) | Theirs wins narrowly, but the entire delta is in their favor: app capture, NV12 surfaces (encoder-native layout), layer-0 window filtering, fail-fast TCC. Ours is "functional but strictly a subset plus BGRA". | `../comparisons/capture.md` section 2, macOS screen verdict |
| macOS camera (AVFoundation) | Ours is an 81-line stub that bails in `new()`; theirs is a complete zero-copy backend with TCC handling. | `../comparisons/capture.md` section 2, macOS camera verdict |
| Windows camera (Media Foundation) | Ours is a documentation stub; theirs is a working D3D11 NV12 zero-copy backend. | `../comparisons/capture.md` section 2, Windows verdict |
| Windows screen (Desktop Duplication) | Ours is a documentation stub; theirs works, with paced re-emission for static screens. | `../comparisons/capture.md` section 2, Windows verdict |
| moq-audio capture surface (beyond P1) | Their surface (system audio, TCC prompt flow, `format()` without open, demand gating) is ahead of ours on every axis except channel discipline, which P1 fixes. | `../comparisons/audio.md` sections 3.2, 3.3 |

## Target in moq

- `rs/moq-audio/src/capture.rs` (the `Microphone` open path and `forward`).
- `rs/moq-audio/src/capture/screencapture.rs` (the system-audio buffer
  channel).

No other moq file changes under this plan.

## Implementation steps

1. Replace the microphone path's `mpsc::unbounded_channel::<Vec<f32>>`
   (`capture.rs:162`) with a bounded channel sized to roughly 500 ms of
   callback buffers, and change `forward` (`capture.rs:230`) to `try_send`,
   dropping the newest buffer on overflow and counting drops. Dropping beats
   blocking here: the sender is cpal's realtime thread and must never park.
   Log the drop count throttled, in moq's tracing style.
2. Apply the same change to the system-audio path
   (`capture/screencapture.rs:100`).
3. Keep the per-callback `Vec` allocation as is for this PR; the callback
   comment already commits to "allocation-light", and removing the allocation
   entirely needs a buffer pool that is out of proportion for a parity fix.
   Note the follow-up in the PR description instead.
4. Write the PR description stating the failure mode being closed (unbounded
   memory growth under a stalled consumer, allocation-plus-send on the
   realtime thread) and pointing at moq-video's own bounded `FrameChannel` as
   the precedent.

## Tests

- A unit test per path asserting that a full channel drops rather than grows:
  push more buffers than the capacity with no reader and assert the receiver
  yields at most the capacity, mirroring moq-video's `drops_oldest_when_full`
  test (`rs/moq-video/src/capture/channel.rs:110`).
- No hardware gating; the tests run against the channel logic without opening
  a device.

## Adaptation notes

- The overflow policy is drop-newest with a counter rather than moq-video's
  drop-oldest, because audio buffers are consumed in order by an encoder and
  reordering the queue on overflow buys nothing; either policy satisfies the
  boundedness requirement, and the reviewer may prefer drop-oldest for
  symmetry with video. State the choice in the PR and defer to their
  preference; this is a one-line difference.
- Do not import our `fixed_resample` ring-buffer machinery; it exists for a
  duplex device engine and is owned by `../audio/audio-device-unify.md`. P1 is
  the minimal boundedness fix in moq's own idiom.

## Counterpart

No cut of its own. This leaf gates the adopt-theirs deletions instead: the
counterpart plans (`../counterpart/codec-remove.md`,
`../counterpart/capture-remove.md`) cite this document as the proof that no
fix of ours is lost when an adopt-theirs module is deleted, and
`DISPOSITION.md` links here from every adopt-theirs row.

## Coordination

- Shared file with `../audio/audio-device-unify.md`: both touch
  `rs/moq-audio/src/capture.rs`. P1 is a small self-contained PR and should
  land first; if the unify leaf reaches that file earlier, P1 folds into it
  and this plan's item table is updated to point there.
- Wave 2 per `../0-overview.md`; P1 is independent of the base API and can
  slot earlier as a goodwill PR alongside opus-improvements.

## Acceptance checklist

- [ ] P1 landed upstream (or folded into audio-device-unify with this table
      updated): no unbounded channel remains in moq-audio capture, verified by
      `grep -rn unbounded rs/moq-audio/src/`.
- [ ] The drop-on-overflow tests pass in moq CI.
- [ ] Every adopt-theirs row in `DISPOSITION.md` resolves to exactly one of:
      an item here, a named sibling leaf, a parked entry here, or a
      nothing-to-port row here.
- [ ] The counterpart cut plans reference this document for their
      adopt-theirs deletions.
