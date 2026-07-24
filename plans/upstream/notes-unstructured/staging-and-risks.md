# Staging, atomic-per-platform ordering, and the risk register

> Campaign: upstream (media stack) | Kind: cross-cutting note | Read
> `../overview.md` first. The per-module removal detail lives in each module doc;
> this note carries the cross-cutting staging and risk that no single module
> owns. moq main is `3a3e0ea8`; adoption needs an ordinary moq release plus a
> version bump.

## Staging

Stages are sequential per platform. Nothing is cut until its replacement is
proven in-tree (the proof-before-deletion rule), and no cut regresses a
zero-copy path (the zero-copy rule); those two rules order everything here.

**Stage M0: type convergence (local).** Adopt `moq_net::Timestamp` in place of
`Duration` through `format.rs` and the pipelines, and delete the broken catalog
mirror `config.rs` in favor of direct `hang::catalog` types
(`../modules/codec-config-mirror.md`). Also the local drops: `sps.rs` dead code
(`../modules/codec-bitstream-sps-vui.md`), the misleading V4L2 EXPBUF doc claim,
and the AV1 rip-out (`../modules/codec-av1.md`), each with proof before
deletion. Entry condition: none. Doing M0 after M1 would force every adoption
diff to carry conversion shims, so it goes first.

**Stage M1: codec adoption, atomic per platform (release-gated).** Adopt
moq-video openh264 encode and decode (`../modules/codec-openh264.md`), the
VideoToolbox encoder (`../modules/codec-videotoolbox-encode.md`), and the
bitstream front end (`../modules/codec-annexb.md`), gaining NVENC, NVDEC, Media
Foundation, H.265, and `rate::Control` outright. Hold VAAPI, V4L2, Android, the
VideoToolbox and Media Foundation decoders, and Opus on our implementations
until their modules land and release. Platforms flip whole: Windows adopts
immediately (pure gain, nothing held); macOS flips when the decode-surface
retention (`../modules/codec-decode-surface.md`) is ready alongside the VT
encoder; Linux non-NVIDIA flips when the VAAPI and V4L2 series land. Atomic
switchover avoids mixing two frame models within one platform: the mixed-stack
bridge would cost 300 to 600 LOC of temporary conversion plus a doubled test
matrix, and buys little, because the early adoptions mostly add capability we do
not ship today. Entry condition: the release bump, stage M0, and the platform
verification gate R-g for every platform being switched. Gate: the conformance
harness and `pipeline_integration.rs` pass with the adopted decoders; latency
tests do not regress.

**Stage M2: capture adoption (release-gated).** Adopt moq-video capture for
macOS camera, macOS screen, and both Windows backends
(`../modules/capture-macos-camera.md`, `../modules/capture-macos-screen.md`,
`../modules/capture-windows.md`); drop nokhwa, xcap, and the Windows stubs
afterward (`../modules/capture-nokhwa.md`, `../modules/capture-xcap.md`); the
Linux column follows its own modules rather than being adopted. Adopt the
moq-audio capture surface (system audio, TCC flow, `format()` without open,
demand gating) onto bounded buffers, explicitly not their unbounded realtime
channel (`parity-ports.md`, item P1; `../modules/audio-backend.md`). Entry
condition: stage M1 on the same platform (their capture emits their frame model
into their encoders), plus R-g for macOS and Windows. macOS camera currently
works only through nokhwa, so removing it before the AVFoundation backend is
proven leaves macOS camera dead.

**Upstream-gated cuts.** Each move-from-iroh-live module is cut on its paired
`up/<name>` branch only once the release carrying its contribution is pinned:
the VAAPI, V4L2, and Android backends and the dispatch collapse; the Linux
capture column; the renderer; and the audio device layer. Dependency spine: B1
gates the decode-surface, VAAPI, PipeWire, and render modules; B2 gates the
V4L2 and Android encoders; those modules gate their local cuts. The renderer and
the decode-surface retention land before or with the decode deletions, or the
decode-to-render zero-copy path breaks (`../zerocopy.md`).

## Risk register

**R-a. Release timing.** The adopt-theirs stack is on moq main already; adoption
waits only for an ordinary release plus a version bump. Mitigation: stage M0 is
local and proceeds regardless; if the release slips, the plan still yields M0
and the module authoring, which targets moq main directly.

**R-b. moq API churn and the plan-freshness protocol.** Post-merge main is still
settling (module boundaries, the `#[non_exhaustive]` sweep, wire-version
constants). Do not start M1 against a git pin; treat citations pinned to
`3a3e0ea8` as direction, not API contract. Before any stage or wave starts,
re-diff `3a3e0ea8` against the then-current main, re-validate the enabler
register in `../comparison/moq-inventory.md`, and re-confirm the affected
module rows.

**R-c. Upstream acceptance.** B1 is the keystone; the moq-vaapi growth is the
largest single piece and the one most likely to meet resistance; Android is a
plausible decline that pushes it onto the B4 registration path
(`coordination.md`). Mitigation: every move-from-iroh-live module stays local
and supported until its contribution releases, and stays permanently if
declined.

**R-d. The rav1d git-fork pin.** moq accepts crates.io dependencies only, so any
AV1 upstream is gated on a released rav1d or vendoring. AV1 is deferred and the
local backend dropped; resolving the pin is a precondition for a future AV1
module, not for any local deletion (`../modules/codec-av1.md`).

**R-e. The cpal git pin.** `audio_backend` uses a git-pinned cpal while
moq-audio uses crates.io cpal; the audio device module treats resolving the pin
as a hard prerequisite, since two cpal versions cannot share one dependency
graph (`../modules/audio-backend.md`).

**R-f. Behavioral differences to verify at each gate.** moq's openh264 output is
Annex-B avc3 only, no avcC; its Opus uses `OPUS_APPLICATION_AUDIO` versus our
VOIP and a zero pre-skip OpusHead; its VideoToolbox encoder uses High profile
versus our Baseline; its video rendition registers in the catalog only after the
first SPS versus our register-up-front; its capture drops oldest at depth 4
versus our backpressure at depth 2; its mic capture uses an unbounded channel we
must not inherit (fixed upstream by `parity-ports.md`, P1). Each delta is cited
in the relevant module and in `../comparison/codecs.md`, `../comparison/audio.md`,
and `../comparison/capture.md`.

**R-g. Platform verification gate.** The proof-before-deletion rule is
unenforceable on a platform we cannot test: there is no macOS or Windows CI
today, and the zero-copy end-to-end test runs only on Intel Linux hardware by
hand. macOS and Windows CI, or at minimum scripted on-hardware verification runs
with recorded results, is an explicit prerequisite for every stage that switches
those platforms (M1 for the VT swap and Windows, M2 for camera and screen smoke
tests per adopted backend). See also the CI hardware gating in
`coordination.md`.
