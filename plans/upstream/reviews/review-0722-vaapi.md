# Adversarial review: VAAPI + zero-copy upstreaming plans (2026-07-22)

> Campaign: upstream | Kind: review | Read ../0-overview.md first.

Reviewer pass over `../codec/{vaapi-decode,vaapi-encode,vtb-mf-decode-surface}.md`,
`../render/moq-video-render.md`, and `../comparisons/{zerocopy.md, moq-changes.md §1a/§1b}`,
cross-checked against moq HEAD `3a3e0ea8` (`/home/bit/Code/rust/moq`), iroh-live working
tree, and the actual `moq-vaapi 0.0.2` crate source
(`~/.cargo/registry/src/.../moq-vaapi-0.0.2`).

## Verdict

The iroh-live-side anchors are almost entirely accurate: every decoder.rs, encoder.rs,
render.rs, and dmabuf_import.rs line cite I checked resolved to the claimed code. The
plans are trustworthy about *our* tree. They are **not** trustworthy about the moq-vaapi
reality, which is the single highest-risk area and where the plans rest on two materially
false premises and one unresolved dependency strategy. As written the VAAPI decode/encode
work is not executable without first re-deciding the moq-vaapi contribution model. The
render and vtb-mf plans are sound in their anchors but each hides one real API gap.

Counts: **3 blocking, 6 substantive, 4 nits.**

The moq-vaapi strategy finding in one line: `moq-vaapi` is a *separate external repo*
(`github.com/moq-dev/vaapi`, crates.io `0.0.2`, maintainer's own org), it is a **bindgen
vendored + AI-regenerated** trim of cros-libva/cros-codecs with its **own** diverged types
(it does *not* depend on the `cros-codecs` crate our code is written against), and it
**already has** `vaExportSurfaceHandle` surface export and a `VAProcPipelineParameterBuffer`
VPP wrapper — so the plans' "no surface export at all" premise is false and "our
cros-codecs decoder drops in" is a rewrite, not a port.

---

## Blocking

### B-1 — "Path: A (in-tree)" is wrong for the moq-vaapi half; we cannot PR it into moq's tree
plan: vaapi-decode §header/§Target/§Coordination; vaapi-encode §Coordination; moq-changes §1b.2/U2

Issue: `moq-vaapi` is not in the moq monorepo. `moq/Cargo.toml:95` pins
`moq-vaapi = "0.0.2"` from crates.io; `rs/moq-video/Cargo.toml:34,95` depends on it as an
optional external crate; `find` over the moq tree returns only the *consumer*
(`rs/moq-video/src/encode/backend/vaapi.rs`). The crate's manifest gives
`repository = "https://github.com/moq-dev/vaapi"`, authors "ChromiumOS Authors, Discord,
Luke Curley <kixelated>". So growing moq-vaapi is a change to a *different repository we do
not control publish rights to*, followed by the maintainer cutting a crates.io release,
followed by a version bump in moq-video. The plans label this "Path: A (in-tree)" and
speak of it as "the largest single piece of the whole program" that this branch "owns" and
"grows." vaapi-decode step 2 does admit "this is a version bump of that crate, not an
in-tree moq-video edit," which directly contradicts the file's own `Path: A` header and the
framing everywhere else.

Evidence: `moq/Cargo.toml:95`; `rs/moq-video/Cargo.toml:31,34,89,95`;
`moq-vaapi-0.0.2/Cargo.toml` (`repository`, `authors`); `moq/Cargo.lock:4649`
(`source = registry+https://.../crates.io-index`).

Fix / detail to add: relabel the moq-vaapi portion as a separate-repo contribution (not
Path A). Specify the mechanics the review asks for: (a) PR to `moq-dev/vaapi` and block on
a maintainer 0.0.3 release, (b) fork + `[patch]`/path-dep during development, or (c) vendor.
Note we cannot self-publish under the maintainer's crate name; the critical path runs
through *their* release cadence, not ours.

### B-2 — Our cros-codecs decoder cannot "drop into" moq-vaapi; it is a re-vendor/rewrite
plan: vaapi-decode §Source to port ("the export body ports, the type it fills in does not"), §Implementation step 2

Issue: our `rusty-codecs/src/codec/vaapi/decoder.rs:79` is typed
`StatelessDecoder<H264, VaapiBackend<VaapiFrame>>` and imports `cros_codecs::{...,
libva::Image, stateless::...}` — it is written against the **crates.io `cros-codecs 0.0.6`**
crate (`rusty-codecs/Cargo.toml:39,68`). `moq-vaapi` does **not** depend on cros-codecs at
all; its deps are `bindgen`, `pkg-config`, `regex`, `bitflags`, `anyhow`, `thiserror`
(`moq/Cargo.lock:4649`). It is a hand-vendored, self-described "(AI GENERATED)" trim with
its *own* types (`moq_vaapi::surface::Surface`, its own `src/codec/h264/{parser,dpb,...}`,
its own bindgen `bindings.rs`). Our `StatelessDecoder`/`VaapiBackend`/`DecodedHandle` stack
has **no counterpart** in moq-vaapi (grep for `StatelessDecoder|DecodedHandle|fn decode`
over its `src/` returns nothing — it is encode-only). Porting decode is therefore a
re-vendor of cros-codecs' *decode half* adapted to moq-vaapi's diverged internal API, not
"the export body ports." This is the single biggest under-estimate in the program.

Evidence: `rusty-codecs/src/codec/vaapi/decoder.rs:11-16,79`; `rusty-codecs/Cargo.toml:39,68`;
`moq-vaapi-0.0.2/Cargo.toml` deps + `description`; grep of `moq-vaapi-0.0.2/src/`.

Fix / detail to add: make the cros-codecs dependency decision explicit (review item 5):
(a) add real `cros-codecs` as a dependency to moq-vaapi and retire the vendored fork
(large, needs maintainer buy-in), (b) re-vendor the decode half into moq-vaapi's diverged
tree, or (c) have moq-video depend on `cros-codecs` directly and bypass moq-vaapi for the
new paths. Note licensing is fine either way (BSD-3-Clause), but the "AI GENERATED"
divergence is itself a maintenance/trust risk to price in.

### B-3 — vaapi-encode's dependency strategy is internally incoherent
plan: vaapi-encode §Implementation step 2 vs §Target/§Coordination-3

Issue: step 2 says "replace the backend struct's `moq_vaapi::encode::Encoder` field with
the cros-codecs `StatelessEncoder` setup," while the same plan says the moq-vaapi
surface-export and VPP additions are "consumed, not authored." These are two incompatible
worlds. Today `rs/moq-video/src/encode/backend/vaapi.rs:24` uses the *high-level*
`moq_vaapi::encode::{Config, Encoder}` wrapper (`encode_nv12(&nv12, keyframe)`), which is
nothing like cros-codecs' `H264StatelessEncoder` API our `encoder.rs:740` drives directly.
Either moq-video/encode gains a *direct* `cros-codecs` dependency (making moq-vaapi
redundant for the encode path), or it stays on moq-vaapi and moq-vaapi must grow the entire
DMA-BUF-import + VPP + StatelessEncoder-tunings API. The plan asserts both and resolves
neither.

Evidence: `rs/moq-video/src/encode/backend/vaapi.rs:24,34,46,58-72`;
`rusty-codecs/src/codec/vaapi/encoder.rs:11,740`.

Fix / detail to add: pick one dependency spine for both decode and encode (see B-2 options)
and make vaapi-encode consistent with it. If moq-vaapi stays the spine, the encode rewrite
depends on the *same* moq-vaapi growth as decode and must be sequenced behind it, not
"builds on."

---

## Substantive

### S-1 — False premise: "moq-vaapi has no surface export at all"
plan: vaapi-decode §Goal (line 23-24), §Evidence; zerocopy U2 (:452-453); moq-changes §1b.2 (:232-234)

Issue: moq-vaapi **already exports surfaces**. `moq-vaapi-0.0.2/src/surface.rs:341` is
`pub fn export_prime(&self) -> Result<DrmPrimeSurfaceDescriptor, VaError>` calling
`bindings::vaExportSurfaceHandle`, and `src/lib.rs:33-35` re-exports the
`_VADRMPRIMESurfaceDescriptor*` bindgen types. The genuinely missing piece is the *decode*
path (confirmed absent), not export infrastructure. The plans repeatedly frame surface
export as the hard, absent, "most resistance-prone" work; that inflates both the estimate
and the risk narrative.

Evidence: `moq-vaapi-0.0.2/src/surface.rs:341-365`; `moq-vaapi-0.0.2/src/lib.rs:33-35`.

Fix: restate the moq-vaapi gap accurately — export exists; the decode `StatelessDecoder`
stack and a full VPP *execution* path are what's missing. Re-size accordingly.

### S-2 — VPP-in-moq-vaapi overstated; the parameter-buffer wrapper already exists
plan: vaapi-decode §Target ("the VPP plumbing"), moq-changes U3 ("raw FFI ... cros-libva does not wrap VPP")

Issue: moq-vaapi already carries `moq-vaapi-0.0.2/src/buffer/proc_pipeline.rs` — a wrapper
over `VAProcPipelineParameterBuffer` (bindgen-generated, so the raw FFI type is present).
The "cros-libva does not wrap VPP, so raw `VaProcPipelineParameterBuffer` FFI is carried by
hand" statement is true only of the **out-of-tree render crate**, which uses cros-libva/ash
and defines the struct manually at `rusty-codecs/src/render/dmabuf_import.rs:1026`. It is
**not** true of moq-vaapi. What moq-vaapi may lack is a VPP *scale/csc execution* path
(context + caps + submit), not the parameter buffer.

Evidence: `moq-vaapi-0.0.2/src/buffer/proc_pipeline.rs:5,69-181`;
`rusty-codecs/src/render/dmabuf_import.rs:1026`.

Fix: separate the two VPP claims — render-crate raw FFI (real) vs moq-vaapi VPP execution
(the actual, smaller gap; the buffer wrapper is done).

### S-3 — `into_i420(self)` consumes the frame, but `render(&mut self, frame: &Frame)` borrows it
plan: moq-video-render §Public API / §Per-platform mapping (`render_i420(frame)`)

Issue: `decode::Frame::into_i420(self)` **consumes** the frame
(`rs/moq-video/src/decode/mod.rs:92`). The render crate's `render(&mut self, frame: &Frame)`
and its fallback `self.render_i420(frame)` only *borrow* the frame, so they cannot call
`into_i420`. There is no public borrowing CPU-download path (`to_i420` is `pub(crate)` on
the inner enum), and `decode::Frame` is not shown to be `Clone`. The plan hand-waves "or
clone the frame before attempting import," but that is an unmet requirement, not a
resolution.

Evidence: `rs/moq-video/src/decode/mod.rs:92-101` (`pub fn into_i420(self)`); no public
`to_i420(&self)`; `moq-video-render.md:76-79,187-216`.

Fix / detail to add: file a B1/B3 gap — either `render()` must take `Frame` by value, or
moq must add a public `to_i420(&self)`/`fn native(&self)` + a borrowing download, or make
`decode::Frame: Clone` (a CVPixelBuffer retain / D3D11 AddRef / Arc clone, all cheap).

### S-4 — vtb-mf-decode-surface silently invalidates the documented `Sync` safety invariant
plan: vtb-mf-decode-surface §Target (macOS), §Adaptation notes

Issue: `macos::Surface` is `unsafe impl Sync` (`rs/moq-video/src/frame.rs:369`), and its
SAFETY comment explicitly justifies `Sync` on the premise that "macOS decoded frames are
**always downloaded to I420 before they reach that fanout**" (moq-transcode's
`Arc<decode::Frame>`) (`frame.rs:360-367`). This plan's whole point is to stop downloading
and retain the `CVPixelBuffer` through the decoded frame, which makes that documented
invariant false and lets moq-transcode share a *live* CVPixelBuffer across fanout threads
(each doing a `CVMetalTextureCache` import). The `Sync` impl likely stays *sound* (all
access is read-only lock), but the plan neither updates the stale safety comment nor
confirms concurrent-read-lock safety across the fanout.

Evidence: `rs/moq-video/src/frame.rs:360-369`; `vtb-mf-decode-surface.md:41-46,131-139`.

Fix: add a step to rewrite the `Surface` `Sync` safety comment and verify that concurrent
read-only imports/`download_i420` across the transcode fanout are safe (they are read-only,
but say so).

### S-5 — The base (B1/B2/B3) API these plans consume does not exist in moq yet; encode Backend still returns `Vec<Bytes>`
plan: all four (each "Depends on B1/B2/B3", "moq API consumed")

Issue: none of `fn native`, `enum Native`, `Frame::DmaBuf`, or the `Packet`/timestamp
encode signature exist in moq at `3a3e0ea8`. Verified: grep for `fn native`/`enum Native`
across `rs/moq-video/src` returns nothing; `encode/backend/mod.rs:40` is still
`fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>` (no
timestamp, no `Packet`). So vaapi-encode's entire signature (`-> Vec<Packet>` with
per-packet timestamp) and every "moq API consumed" anchor into `Native`/`DmaBuf` is
aspirational. (The decode side *does* already carry `timestamp` on `decode` and on
`Decoded`, so that half of the plans' claim is correct — `decode/backend/mod.rs`.)

Evidence: `rs/moq-video/src/encode/backend/mod.rs:37-56`; grep of `rs/moq-video/src` for
`Native`/`native`/`DmaBuf`.

Fix: state explicitly that these plans are hard-blocked on the unbuilt B1/B2/B3 base (not
in the reviewed set), and that their moq-side anchors cannot be verified until it lands.
Sequence the base branch as a gate, not a parallel dependency.

### S-6 — Encoder session-reuse/`reset()` for transcode is unscoped, and non-trivial
plan: vaapi-encode §Transcode (line 182-186); vaapi-decode §Transcode

Issue: the transcode note now *requires* "an encoder-session-reuse path so a per-group
transcode loop does not pay a fresh VA-context open per group," and correctly observes our
encoder "constructs via `new` and has no `reset`." Confirmed: `grep 'fn reset'` over
`rusty-codecs/src/codec/vaapi/encoder.rs` returns nothing (only the *decoder* has
`reset()@731`). But the plan does not scope what adding one entails: cros-codecs'
`H264StatelessEncoder` has no in-place resolution/GOP reconfigure, so a `reset` that reuses
the VA `Display`/context while re-arming rate control and forcing a fresh IDR either needs a
new cros-codecs API or a controlled teardown/rebuild of the `StatelessEncoder` over a
retained `Display`. This is a real design task, not a one-liner.

Evidence: `rusty-codecs/src/codec/vaapi/encoder.rs` (no `fn reset`; `new`-only
construction); decoder `reset()@731` for contrast.

Fix: scope the reset path — retain `Display`, rebuild encoder-session, re-issue IDR — and
flag it as new work on cros-codecs or on moq-vaapi, coordinated with the maintainer's
per-segment-transcode goal.

---

## Nits

### N-1 — `build_prime_descriptor` line cite is a few lines high
plan: vaapi-encode §Source to port (`encoder.rs:87-124`)

The `fn build_prime_descriptor` begins at `encoder.rs:91` (line 87 is the preceding
comment). zerocopy elsewhere cites `:87-118` and `:87-119` for the same function
(inconsistent end line). Tighten to `:91`.

### N-2 — moq-vaapi provenance/licensing worth surfacing for the dependency decision
plan: missing detail (review item 5)

The crate self-describes as "(AI GENERATED) VA-API H.264 hardware encoder, derived from
discord/cros-libva + discord/cros-codecs," license BSD-3-Clause, authors ChromiumOS +
Discord + Luke Curley. License is compatible; the AI-generated divergence from upstream
cros-codecs is the risk. The plans' "cros-codecs/cros-libva dependency + licensing" detail
should record this explicitly.

### N-3 — Feature-gate story for a new decode path is unstated
plan: vaapi-decode §Target, moq-changes §1a note ("or a new `dmabuf` feature")

moq's `vaapi` feature exists (`rs/moq-video/Cargo.toml:34`, `vaapi = ["dep:moq-vaapi"]`),
but B1's `Frame::DmaBuf` is gated on it while moq-changes floats "or a new `dmabuf`
feature." Decide up front whether decode + DmaBuf ride the existing `vaapi` feature or a new
`dmabuf` one, since it affects the candidate-table cfg and every downstream cite.

### N-4 — Hardware-CI limitation should be stated, not just implied
plan: all four §Tests

The plans mark hardware tests `#[ignore]` when no Intel/AMD VA device is present, which is
correct, but should state plainly that **moq's own CI cannot run any of these** (moq CI is
GPU-optional by design) and that validation lives only on our Meteor Lake hardware / the
iroh-live device runner. This is the realistic bar the maintainer will ask about.

---

## Anchors verified accurate (for the record)

- decoder.rs: `:37` baseline-patch doc, `:79` `StatelessDecoder<H264,...>`, `:85` +
  `:104-113` `CachedDmaBufExport`/`OnceCell`, `:228` `export.fd.try_clone()`, `:247-311`
  `extract_dma_buf_info` with `surface.sync()`@`:272-275` before `export_prime()`@`:277`,
  `:616` `burst_size`, `:731` `reset`.
- encoder.rs: `:91` `build_prime_descriptor`, `:740` `H264StatelessEncoder`, `:928`
  `force_keyframe: true` (priming), `:1009` `vpp_convert_or_cpu`, `:1069` `vpp_scale_or_cpu`,
  `:1268` `push_frame`, `:1352` `force_keyframe: false`; no `reset()`.
- render.rs: `:46` `WgpuVideoRenderer`, `:72` `RenderPath` (CpuRgba/CpuNv12/DmaBuf/
  MetalZeroCopy/GpuDownload). dmabuf_import.rs: `:232` `import_nv12`, `:1026`
  `VaProcPipelineParameterBuffer`, `:1057` `VppRetiler`, `:1136` `retile`, `:1413`
  `create_device_with_dmabuf_extensions`. Total render tree 3463 LOC (~3500 as cited).
- moq placeholder encode/backend/vaapi.rs: 111 lines, "NOT YET VALIDATED ON HARDWARE"@`:19-21`,
  `i420_to_nv12`@`:97-111`, `set_bitrate -> Error::BitrateUnsupported(NAME)`@`:80-88`.
- moq decode: `videotoolbox.rs` `Sink { frames: Vec<I420> }`@`:57-58`, downloads
  @`:274-276`, `Frame::I420`@`:239`; `mediafoundation.rs` `download_i420`@`:339`,
  `Frame::I420`@`:393`. `macos::Surface` is `Send + Sync`@`:368-369`; decode `Frame`
  Send+Sync pin @`decode/mod.rs:113-117`. No VAAPI row in `decode/backend/mod.rs` HARDWARE
  slice (macOS/Windows/nvdec only) — vaapi-decode's "adds a new row" claim is correct.
