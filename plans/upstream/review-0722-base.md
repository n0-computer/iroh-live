# Adversarial review — upstream BASE plans (B1-B5) vs moq HEAD 3a3e0ea8

Date: 2026-07-22. Reviewer pass cross-checked every cited moq `file:line` against
`/home/bit/Code/rust/moq`. Plans read: `0-overview.md`, `base/B1..B5`.

## Verdict

The base plans are **anchor-accurate but not yet buildable as written**. Every
load-bearing `file:line` I spot-checked is correct and says what the plan claims
(a genuine strength — see "Anchors verified"). The problems are in *buildability*
and *omitted implementation detail*: three blocking issues would stop a PR agent
cold (a non-existent `v4l2` feature, a circular dep in the "preferred" moq-frame
crate home, and a lost hardware/software tier in the B4 table conversion), plus a
cluster of substantive gaps (handle Clone, unsafe Send/Sync, DMA-BUF download,
workspace wiring). B2 is the cleanest and is essentially ready; B5 is sound. Fix
the three blockers and fill the substantive gaps before freezing the contract.

Counts: 3 blocking, 10 substantive, 3 nits. Anchors: all spot-checked citations
correct.

## Blocking

1. **B1 §Implementation step 2 / feature design — the `v4l2` feature does not
   exist.** moq-video's `Cargo.toml` (rs/moq-video/Cargo.toml:15-39) declares only
   `nvenc`, `nvdec`, `vaapi`, `pipewire`. V4L2 capture is *unconditional* (`v4l =
   "0.14"` at :101, and the `from_yuyv`/`from_rgb` paths gate on
   `target_os="linux"`, not a feature — frame.rs:167,187). So the proposed graph
   "`vaapi`, `pipewire`, and `v4l2` each enable `dmabuf`" references a feature that
   isn't there. Fix: either introduce a new `v4l2` feature (and decide what it
   gates, since V4L2 is currently always-compiled) or drop it from the graph and
   state how the always-on V4L2 path opts into `dmabuf`.

2. **B1 §Target 4 (moq-frame crate home) — circular dependency, unbuildable.** The
   public `DmaBuf` is specced to (a) hold "the exporting object (a VA surface or a
   dup source)", (b) `export(&self) -> Result<OwnedFd, Error>` with moq's `Error`,
   and (c) back `download_i420 -> I420`. The VA-surface exporter lives in
   moq-vaapi, and `Error`/`I420` live in moq-video (error.rs; frame.rs:80) — all
   *above* moq-frame. A standalone moq-frame holding any of them is a dependency
   cycle. Fix: for the crate home, abstract the exporter behind a closure/trait
   (`Arc<dyn Fn() -> Result<OwnedFd, ExportError>>`), give moq-frame its own error
   type (not moq-video's), and keep I420 download entirely in the moq-video-side
   `crate::frame::dmabuf::Frame` (step 3), never on the public type. Otherwise pick
   the module home. As written the "preferred" option cannot compile.

3. **B4 §Target 4 — the `OnceLock<Vec<Candidate>>` conversion loses the
   hardware/software tier.** `open()` derives tier purely from *which slice* a
   candidate sits in: encode iterates `HARDWARE` then `SOFTWARE` separately
   (encode/backend/mod.rs:109-121), decode likewise (decode/backend/mod.rs:120-128).
   The built-in `Candidate` carries no tier flag, and the proposed `Registration`
   /`DecodeRegistration` structs have only `name`/`codecs`(or `supports`)/`open` —
   no `hardware` bool. Folding both slices into one `Vec` makes
   `Kind::{Auto,Hardware,Software}` ordering unreconstructable. Fix: keep two Vecs,
   or add a `hardware: bool` (or tier enum) field to `Candidate` *and* to both
   registration structs, and say so in the frozen contract.

## Substantive (missing detail a PR agent needs)

4. **B1 §Target 4 — the public `Native::DmaBuf(DmaBuf)` arm gate contradicts its
   payload.** The arm is gated `target_os="linux"` unconditionally ("so a caller
   can name `Native::DmaBuf` on any Linux build"), but the `DmaBuf` exporter only
   exists under `dmabuf`/`vaapi`. A `--no-default-features` Linux build then names
   a type whose backing field type is absent → won't compile. Specify DmaBuf's
   field on a dmabuf-less Linux build (an uninhabited exporter / `PhantomData` +
   `unreachable`), or gate the arm on `feature="dmabuf"` and update the contract.

5. **B1 §Target 4 / B3 — `Native` needs `Clone` on the wrapped handles, which
   `Surface` and `Texture` lack.** `native(&self) -> Option<Native>` builds an
   owned `Native` from `&self`, so it must clone the inner handle. Only
   `cuda::Frame` derives `Clone` (frame.rs:508); `macos::Surface` (frame.rs:353)
   and `d3d11::Texture` (frame.rs:721) do NOT, despite docs saying clone is a cheap
   retain/AddRef. B1/B3 must add `Clone` (or a `retain()`), and the public newtype
   wrappers must clone through. Not mentioned.

6. **B1 §Tests — Send/Sync of the new GPU frame types is non-trivial, not a free
   compile check.** The `frame_and_consumer_are_thread_safe` test
   (decode/mod.rs:104-118) requires `Frame: Send+Sync`, and B1 says `Native` must
   be too. A VA surface (VADisplay raw pointer, VASurfaceID) and AHardwareBuffer
   are `!Send`/`!Sync` by default; they need explicit `unsafe impl Send + Sync`
   with a written safety argument, exactly as `macos::Surface` does
   (frame.rs:359-369). Call this out for `dmabuf::Frame`, `android::HwBuffer`, and
   every `Native` variant.

7. **B1 §Target 3 — `download_i420` for DmaBuf/HardwareBuffer is unspecified and
   genuinely hard.** The plan treats it as a trivial fallback. A PRIME fd under a
   tiled/compressed modifier (the DmaBuf carries the modifier for exactly this
   reason) cannot be plain-`mmap`'d into I420 — it needs a VA `vaDeriveImage`/
   `vaGetImage` readback or modifier-aware detiling; AHardwareBuffer needs
   `AHardwareBuffer_lock` + NV12 deinterleave. Name who implements each and the
   readback path, and note the linear-vs-tiled branch keyed on `modifier()`.

8. **B1 — workspace wiring for the new crate is omitted.** Adding `rs/moq-frame`
   requires editing the root `Cargo.toml` `members` and `default-members` lists
   (Cargo.toml:2-60, neither lists moq-frame) and adding a `[workspace.dependencies]
   moq-frame` entry so moq-video can take it as `moq-frame = { workspace = true }`.
   None of this is in the plan.

9. **B1 — the concrete Cargo.toml feature edits are not spelled out.** Add
   `dmabuf = []`, change `vaapi = ["dep:moq-vaapi"]` (Cargo.toml:34) to
   `vaapi = ["dmabuf", "dep:moq-vaapi"]`, and add `dmabuf` to `pipewire`
   (Cargo.toml:39). Also state the `use std::os::fd::OwnedFd;` import for `export`.
   The plan describes the graph in prose but a PR agent needs the exact stanzas.

10. **B4 §Target 1 / Adaptation — "additive-sealed" is a contradiction for a
    *registerable* trait.** Sealing forbids external impls; registration requires
    them. The trait becomes fully public and open. Reframe as "public, evolved only
    additively"; the real breaking risk is a new required method (the plan does note
    the defaulting mitigation in step 5 — keep that, drop "sealed").

11. **B4 §Target 5 — registration storage design is muddled.** A `OnceLock<Vec>`
    cannot be appended after its first read, yet step 5 offers "or append into the
    `OnceLock`'s `Vec`". The workable design is a separate `Mutex<Vec<Registration>>`
    (or `OnceLock<Mutex<Vec>>`) staging area that `open()` reads when it first
    builds the candidate list, plus the documented "register before first
    `Encoder::new`" contract. State the ordering/thread-safety explicitly.

12. **B4 §Target 3 — publishing `Decoded` forces `Frame`/`Native` public and a
    dep on B1 that the plan should make load-bearing in the build order.** `Decoded`
    (decode/backend/mod.rs:53-62) has `pub frame: Frame`; publishing it means the
    private `crate::frame::Frame` must be reachable as public vocabulary. The plan
    says this but does not resolve that `Frame` itself stays `pub(crate)` — only
    `native()`/`into_i420()` expose it. Clarify that `Decoded.frame` must become
    `decode::Frame` (the public wrapper), not the private enum, or the trait leaks a
    `pub(crate)` type in a `pub` signature (won't compile).

13. **B5 — the Error-variant plan is sound but inherits blocker 2.** `error.rs` is
    `#[non_exhaustive]` with exactly the variants B5 lists (`NoEncoder`,
    `NoDecoder`, `UnsupportedCodec`, `Unsupported`, `InvalidFramerate`,
    `BitrateUnsupported(&'static str)`, `Codec(#[from] anyhow::Error)`, `Mux`,
    `Net`, `TimeOverflow` — all verified). Adding `SurfaceExport`/`DmaBufImport` is
    clean. But B1's `DmaBuf::export` in the *crate* home cannot return this variant
    (finding 2), so B5's "export raises `SurfaceExport`" only holds for the module
    home. Note the coupling.

## Nits

14. **B2 §Target 6 — "five in-tree backends in one pass" but no single target
    compiles all five.** openh264 always; videotoolbox macOS; mediafoundation
    Windows; nvenc/vaapi Linux+feature (encode/backend/mod.rs:68-102). A PR must
    build on macOS, Windows, and Linux to exercise every arm; add that to the
    acceptance checklist. The semantics are otherwise correct: producer.rs:85-89
    documents each `Bytes` as "one whole access unit", so wrapping each in a
    `Packet` with its own timestamp is behavior-preserving — state this.

15. **B2 §Target 6 — anchor nuance.** producer.rs:387 `encoder.encode(frame,
    force_keyframe).await` is the async `Sink::encode` (sink.rs:123/187, owned
    `Frame`), not `Encoder::encode`/`Backend::encode`. Step 5 handles the Sink and
    channel correctly; just don't call the producer site a direct `Encoder` call.

16. **B1 — `frame/dmabuf.rs` submodule is fine (no restructuring needed).** A
    `frame/` directory already exists (frame.rs:452 `include_str!("frame/
    nv12_resize.ptx")`) and coexists with `frame.rs`; `mod dmabuf;` inside frame.rs
    works. The plan is correct here — flagging only to preempt a false alarm.

## Anchors verified (all correct)

frame.rs:23-36 (enum: Surface/Texture/Cuda/I420, cfg gates), :39-74 (width/height/
to_i420, explicit exhaustive arms, no wildcard — new arms are required and compile).
decode/mod.rs:36-46 (`pub(crate) inner`), :94-101 (into_i420, `#[allow(unreachable_
patterns)] other =>` — so it needs no new arm), :104-118 (thread-safety test).
encode/backend/mod.rs:37 (`pub(crate) trait Backend`), :40 (old `encode(&Frame,
bool) -> Vec<Bytes>`), :60-64 (Candidate), :68-102 (HARDWARE/SOFTWARE slices),
:106-134 (open). decode/backend/mod.rs:53-62 (Decoded), :67 (`pub(crate) trait`),
:71 (decode sig), :78 (`type Open = fn(Codec,&Config)`), :81-85 (Candidate w/
`supports`), :110-114 (single `const SOFTWARE`). lib.rs:35-44 (no-backend-type
rule), :57-58 (`pub use error::Error; pub use size::Size;`). encoder.rs:22/:35
(`#[non_exhaustive]` Codec + Kind, `Named(String)`), :189-272 (entry points +
encode_raw + finish). producer.rs:386-392 (stamping), :85-104 (publish, "one whole
access unit per packet"). sink.rs:38-45/:123/:187. moq_net::Timestamp is `#[derive(
Clone, Copy, PartialEq, Eq, Hash)]` (model/time.rs:150) with `from_micros`/`micros`
— so `Packet { timestamp }` and by-value echo are fine. error.rs is
`#[non_exhaustive]`.
