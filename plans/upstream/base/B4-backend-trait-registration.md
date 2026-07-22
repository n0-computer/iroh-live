# B4. Public registerable Backend trait + registration

> Campaign: upstream | Kind: base plan | Branch: up/b4-backend-registration (its
> own branch, off base; not folded into the base PR) | PR target: base branch,
> then moq main | Read ../0-overview.md first.

Depends on: B1 (the public `Frame`/`Native` vocabulary the trait traffics in), B2 (the `encode` timestamp signature and `Packet`), B3 (the public `decode::Frame` wrapper, required so `Decoded.frame` is a public type rather than a leaked `pub(crate)` enum)
Path: B only (external-backend path); BREAKING
Size: M (roughly 250 lines across both sides)

## Goal

Publish moq's encode and decode `Backend` traits as fully public and
implementable, and add a registration API so an out-of-tree crate can contribute a
codec backend without
editing moq's `const` candidate tables. Today both traits are `pub(crate)`
(`encode/backend/mod.rs:37`, `decode/backend/mod.rs:67`), the candidate tables are
private `const` slices, and `Kind::Named(String)` routes only among the built-ins:
an out-of-tree backend is impossible without a source change to moq. B4 makes the
traits public (trafficking only in public vocabulary types so no codec internal is
exposed), folds the two per-tier candidate slices into one tier-tagged
`Vec<Candidate>` built at selection time from the built-ins plus an append-safe
`Mutex` staging area, and adds `register_encoder` and a non-mirror
`register_decoder`. This is the only breaking change in the whole program and the
only one exclusive to Path B; an in-tree backend needs none of it. It is
CONDITIONAL on the Android placement question. Open question: the Android
placement (in-tree with its NDK build cost, or external over the registration
API), discussed in `../codec/android-mediacodec.md` and coordination point 6 of
`../0-overview.md`; current proposal: external (Path B). Do not open B4 as a PR
until that question is settled upstream.

## Evidence

- The traits are deliberately `pub(crate)` and the tables private `const`:
  `pub(crate) trait Backend` (`encode/backend/mod.rs:37`, `decode/backend/mod.rs:67`),
  `struct Candidate` (`encode/backend/mod.rs:60-64`), `const HARDWARE` /
  `const SOFTWARE` (`encode/backend/mod.rs:68-102`; on the decode side `const
  HARDWARE` is a slice at `decode/backend/mod.rs:89-108` but `const SOFTWARE` is a
  single `Candidate`, not a slice, at `decode/backend/mod.rs:110-114`).
- The decode `Candidate` is not shaped like the encode one: it carries
  `supports: fn(Codec) -> bool` (`decode/backend/mod.rs:83`) instead of a `codecs`
  slice, and its opener is `type Open = fn(Codec, &Config) -> Result<Box<dyn
  Backend>, Error>` (`decode/backend/mod.rs:78`), so one backend serves several
  codecs the way NVDEC serves H.264, H.265, and AV1 (`decode/backend/mod.rs:105`).
  The registration API must respect that asymmetry, not mirror the encode side.
- The public-API rule moq will invoke: "no public type, signature, or error variant
  names a backend" (`rs/moq-video/src/lib.rs:37-44`).
- The tradeoff and the conditional framing are `comparisons/moq-changes.md`
  section 2 (change 7) and section 4 (the Path A versus Path B decision, with
  Android as the one backend that justifies opening this API). Verified against
  `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`.

## moq API consumed

- B1's `Frame`/`Native` vocabulary (the trait's input type) and B2's `Packet` and
  `encode` timestamp signature (the trait's output and shape). B4 publishes the
  trait that already carries these after B1 and B2; it adds no new frame or packet
  type.

## Source to port

Our side selects backends through public `VideoEncoder` / `VideoEncoderFactory`
traits with a `const ID` plus a `with_config` factory
(`rusty-codecs/src/traits.rs:311-377`) and a `VideoDecoder` trait
(`traits.rs:379-410`). B4 does not port that trait shape; moq keeps its concrete
`Encoder`/`Decoder` front ends and its `Candidate` table. What carries over is only
the idea of a public registration seam. The `const ID`/factory pattern is the
reference for `Registration.name`/`open`, adapted to moq's `fn`-pointer table rather
than a generic factory trait.

## Target in moq

1. **Publish the encode `Backend` trait** (`rs/moq-video/src/encode/backend/mod.rs:37`).
   Change `pub(crate) trait Backend` to `pub trait Backend`. Do not describe it as
   "sealed": a registerable trait is by definition implementable by external crates,
   so the accurate framing is that the trait is public and open to implementation,
   and its stability rests entirely on the vocabulary-only types it traffics in. It
   takes the public `Frame`/`Native` input and the public `Packet` output (B1, B2),
   so moq never exposes a codec-internal type, and the only genuinely breaking future
   move is adding a required method (avoidable by defaulting, per step 5). The method
   set is B2's:

   ```rust
   pub trait Backend: Send {
       fn encode(&mut self, frame: &Frame, timestamp: Timestamp, keyframe: bool)
           -> Result<Vec<Packet>, Error>;
       fn finish(&mut self) -> Result<Vec<Packet>, Error>;
       fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;
       fn name(&self) -> &str;
   }
   ```

2. **The encode `Registration` and `register_encoder`**
   (`rs/moq-video/src/encode/backend/mod.rs`). The registration mirrors the encode
   `Candidate` shape (`:60-64`: `codecs: &'static [Codec]`, `open: fn(&Config)`):

   ```rust
   /// A registerable encoder backend: a name, the codecs it emits, an opener, and
   /// its hardware/software tier.
   #[non_exhaustive]
   pub struct Registration {
       pub name: &'static str,
       pub codecs: &'static [Codec],
       pub open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
       /// `true` places this backend in the hardware tier (tried before software
       /// under `Kind::Auto`, and eligible under `Kind::Hardware`); `false` places
       /// it in the software tier. The built-in tables encode this tier implicitly
       /// by which `const` slice a `Candidate` sits in (`HARDWARE` vs `SOFTWARE`,
       /// `encode/backend/mod.rs:68-102`), so a registration must carry it
       /// explicitly once the two slices fold into one `Vec` (Target 4).
       pub hardware: bool,
   }

   /// Register an external encoder backend, consulted by `Kind::Auto` (after the
   /// built-in hardware candidates, before software), `Kind::Hardware`, and
   /// `Kind::Named`. Call once at startup, before the first `Encoder::new`.
   pub fn register_encoder(reg: Registration);
   ```

3. **The decode `Backend` trait and `DecodeRegistration`**
   (`rs/moq-video/src/decode/backend/mod.rs:67`). Publish `pub trait Backend`
   (method set unchanged: `decode(&mut self, access_unit: Bytes, timestamp:
   Timestamp, keyframe: bool) -> Result<Vec<Decoded>, Error>` and `name`,
   `:71-74`). The registration is NOT a mirror of the encode one, because the decode
   `Candidate` carries a `supports` predicate and a codec-taking opener
   (`:78-85`):

   ```rust
   /// A registerable decoder backend: a name, a codec-support predicate, and an
   /// opener that receives the concrete codec to open, so one backend can serve
   /// several codecs (as NVDEC serves H.264, H.265, and AV1).
   #[non_exhaustive]
   pub struct DecodeRegistration {
       pub name: &'static str,
       pub supports: fn(Codec) -> bool,
       pub open: fn(Codec, &Config) -> Result<Box<dyn Backend>, Error>,
       /// Hardware/software tier, same meaning as on the encode `Registration`.
       /// The built-in decode tables derive it from slice membership too: the
       /// hardware backends are a slice (`decode/backend/mod.rs:89-108`) and the
       /// single software backend is a standalone `const` (`:110-114`), and `open`
       /// chains `HARDWARE` then `SOFTWARE` to order the tiers (`:120-128`).
       pub hardware: bool,
   }

   /// Register an external decoder backend, consulted alongside the built-in decode
   /// candidates in priority order. Call once at startup, before the first
   /// `Decoder::new`.
   pub fn register_decoder(reg: DecodeRegistration);
   ```

   Publishing the decode `Backend` also requires publishing `Codec`
   (`decode/backend/mod.rs:36-41`, currently `pub(crate)`) and `Decoded`
   (`:53-62`, currently `pub(crate)`), since they appear in the trait's signature
   and the registration. This is where B3 and B1 become hard prerequisites, not
   optional follow-ons. `Decoded` today is `pub(crate)` with `pub frame:
   crate::frame::Frame` (`:61`), and `crate::frame::Frame` stays `pub(crate)`
   after B1: B1 only exposes it indirectly through `decode::Frame`'s `native()` /
   `into_i420()` accessors. A `pub struct Decoded` whose `frame` field is the
   private `crate::frame::Frame` would leak a `pub(crate)` type through a `pub`
   signature, which does not compile. So `Decoded.frame` must be retyped to the
   public wrapper `decode::Frame` (the type B3 publishes with `size` and
   `timestamp` alongside the private inner), not the private enum. Put differently,
   B4 cannot publish `Decoded` until B1 lands the vocabulary and B3 publishes
   `decode::Frame`; the build order is B1, then B3, then B4. B4 must state this in
   its dependency list rather than treating B3 as a tiny follow-on.

4. **Fold the tables into one tier-tagged `Vec<Candidate>` per side.** Today the
   hardware/software tier is not a field on `Candidate`; it is derived purely from
   *which slice* a candidate sits in. Encode `open` iterates `HARDWARE` then
   `SOFTWARE` separately (`encode/backend/mod.rs:109-121`), and decode `open`
   chains `HARDWARE` then the single `SOFTWARE` const (`decode/backend/mod.rs:120-128`).
   Collapsing the two slices into one flat `Vec` therefore loses the tier that
   `Kind::{Auto,Hardware,Software}` selection depends on, unless the tier is made
   explicit. So the built-in `Candidate` structs (`encode/backend/mod.rs:60-64`,
   `decode/backend/mod.rs:81-85`) gain a `hardware: bool` field, set `true` on the
   entries seeded from `HARDWARE` and `false` on those from `SOFTWARE`. This mirrors
   the `hardware` flag added to `Registration`/`DecodeRegistration` in Targets 2 and
   3, so built-in and registered candidates carry the tier the same way.

   Selection then reconstructs the tiers from the flag rather than from slice
   membership. Encode `open` (`:106-134`) builds one candidate list, and:
   - `Kind::Auto` takes hardware candidates (`c.hardware`) first, then software
     (`!c.hardware`), preserving today's order (built-in hardware, then registered
     hardware, then software). Registration appends, so a registered hardware
     backend sorts after the built-in hardware ones and before software, matching
     the documented contract.
   - `Kind::Hardware` filters to `c.hardware`.
   - `Kind::Software` filters to `!c.hardware`.
   - `Kind::Named(name)` filters by `c.name == name` across both tiers. No change to
     the public `Kind` enum (`encode/encoder.rs:34-48`); `Named(String)` already
     routes by name.

   Decode `open(codec, config)` (`:119-146`) does the same, additionally filtering by
   each candidate's `supports` predicate exactly as it filters the built-ins today.
   The decode `SOFTWARE` is currently a single `const Candidate`, not a slice
   (`:110-114`), so the conversion normalizes it into the `Vec` (with `hardware:
   false`) rather than concatenating two slices.

   The candidate list itself is built at selection time from two sources: the
   built-in candidates (const data, or built once) and the registered candidates
   from the staging area in Target 5. It is not a bare `static
   OnceLock<Vec<Candidate>>` seeded once, because a `OnceLock` cannot accept
   registrations that arrive after its first read (see Target 5).

5. **Registration storage: a `Mutex` staging area, not a bare `OnceLock`.** A
   `OnceLock<Vec<...>>` is write-once: once `open` reads it, it cannot accept a
   later registration, so it is the wrong primitive for an append-after-startup
   API. The workable design is a staging area that stays appendable and is read at
   selection time:

   ```rust
   static ENCODER_REGISTRY: Mutex<Vec<Registration>> = Mutex::new(Vec::new());

   pub fn register_encoder(reg: Registration) {
       ENCODER_REGISTRY.lock().unwrap().push(reg);
   }
   ```

   `open` locks the registry, builds the tier-tagged candidate list from the
   built-ins plus a snapshot of the registered entries (mapping each `Registration`
   to a `Candidate` with its `hardware` flag), and selects. The decode side uses a
   parallel `DECODER_REGISTRY: Mutex<Vec<DecodeRegistration>>`. Registration is thus
   thread-safe and order-preserving (push order becomes tie-break order within a
   tier). If a `Mutex` lock in `open` is unwelcome on a hot path, an
   `OnceLock<Mutex<Vec<Registration>>>` or an arc-swap works the same way; the
   invariant is only that the storage stays appendable and is consulted on each
   `open`, never snapshotted permanently on first read. Document the "register once
   at startup, before the first `Encoder::new` / `Decoder::new`" contract: the
   contract is about avoiding a mid-run change of the selected backend, not a
   write-once limitation of the storage.

## Implementation steps

1. Publish the encode `Backend` trait, add the `hardware: bool` field to the
   built-in `Candidate`, and add `Registration` (with its own `hardware` flag) +
   `register_encoder` backed by the `Mutex<Vec<Registration>>` staging area; keep
   the built-in candidates tier-tagged and ordered first so priority order is
   unchanged when nothing is registered.
2. Publish the decode `Backend` trait, `Codec`, and the retyped `Decoded` (its
   `frame` field now `decode::Frame`, per Target 3); add the `hardware: bool` field
   to the decode `Candidate`, and add `DecodeRegistration` + `register_decoder` with
   its `supports`-predicate opener and its own `Mutex` staging area, normalizing the
   single `SOFTWARE` const into the tier-tagged `Vec`.
3. Rebuild the candidate list in both `open` functions from the built-ins plus the
   staged registrations at selection time, reconstructing the tiers from the
   `hardware` flag so all `Kind` arms consult registered candidates, with no change
   to `Kind`.
4. Add crate-root re-exports for the newly public `Backend`, `Registration`,
   `DecodeRegistration`, `Packet`, `Codec`, `Decoded`, `register_encoder`, and
   `register_decoder`, and document the stability contract in the module docs and
   the `lib.rs` API-stability section (`lib.rs:35-44`): the trait is public and
   implementable, it traffics only in vocabulary types, and that is what keeps it
   stable. Do not call it "sealed".
5. Add a doc note that a new required trait method is the only genuinely breaking
   future move and is avoidable by defaulting, so additive evolution stays possible.

## Tests

- A registration round trip with a trivial in-test backend: register a fake encoder
  that emits a marker packet, open via `Kind::Named("fake")`, `Kind::Auto`, and
  `Kind::Hardware`, and assert it is selected in the documented priority relative to
  the built-ins. Same for a fake decoder via `register_decoder`, asserting the
  `supports` predicate gates codec selection. Hardware-free, ffmpeg-free, runs in CI.
- A priority-order test proving a registered candidate sorts after built-in hardware
  and before software under `Kind::Auto`, matching the documented contract.
- The existing `NoEncoder`/`NoDecoder` exhaustion tests still report the tried list
  including registered candidates.

## Adaptation notes

- The stability tradeoff, stated for moq: a public `Backend` trait plus
  registration turns the frame vocabulary, the `Packet` shape, and the
  `Registration`/`Kind` interaction into semver surface that review can no longer
  reshape freely (`comparisons/moq-changes.md` section 2, "The tradeoff, stated for
  moq"). This is not sealing: a registerable trait is open to external
  implementation by design, so the honest framing is a public, implementable trait
  whose stability rests on the vocabulary-only types it traffics in. The mitigation
  is that the trait only ever sees public vocabulary types, `Packet` and the
  registrations are `#[non_exhaustive]`, and new `Frame` variants are cfg-gated and
  additive, so the only genuinely breaking future move is a new required trait
  method, avoidable by defaulting.
- Conventional-commit the trait publication with `!` for the breaking change and the
  `moq-video` scope.
- Do not refactor the candidate tables beyond the tier-flag addition and the
  staging-area conversion; leaves add their own `const Candidate` (coordination
  point 2, now carrying the `hardware` flag) and a broader table redesign from B4
  would conflict with every in-tree leaf.

## Coordination

- Coordination point 6 (the B4 breaking change): this is the only breaking item and
  is worth it only if moq wants out-of-tree backends. Open question: the Android
  placement (in-tree with its NDK build cost, or external over registration),
  discussed in `../codec/android-mediacodec.md`; current proposal: external
  (Path B). Do NOT open B4 as a PR until that question is settled upstream.
  In-tree VAAPI, V4L2, and AV1 backends need only B1, B2, and B3 and add
  a `const Candidate`; they must not wait on B4.
- Coordination point 1 (base API freeze): the `Registration` and `DecodeRegistration`
  shapes are frozen contract for the android-mediacodec leaf, which is the sole
  consumer.

## Transcode and rate control (overview coordination point 7)

moq-transcode selects encoders by `Kind`, so `Kind::Named` and registered
backends are transcode-selectable: a registered external backend can also serve
as a transcode encoder for a rung. Registration therefore benefits the
per-segment transcoding path as well as live capture.

## Acceptance checklist

- [ ] Both `Backend` traits are `pub` and documented as public and implementable
      (not "sealed"), trafficking only in `Frame`/`Native`/`Packet`/`Codec`/`Decoded`
      public types; `Decoded.frame` is the public `decode::Frame`, not the private
      enum.
- [ ] `Registration`, `DecodeRegistration`, `register_encoder`, `register_decoder`
      match the frozen contract verbatim, each registration carrying a `hardware`
      tier flag; decode is the non-mirror `supports` + codec-taking-opener shape.
- [ ] The built-in `Candidate` structs carry a `hardware` flag; the two slices fold
      into one tier-tagged `Vec` per side; the single decode `SOFTWARE` const is
      normalized in with `hardware: false`; selection reconstructs the tiers from the
      flag; priority order is unchanged when nothing is registered.
- [ ] Registrations are held in an append-safe `Mutex` staging area consulted on
      each `open`, not a write-once `OnceLock` snapshotted on first read.
- [ ] `Kind::{Auto,Hardware,Named}` all consult registered candidates; the public
      `Kind` enum is unchanged.
- [ ] Registration and priority-order tests pass in CI.
- [ ] The PR is opened only after the Android placement decision; the breaking
      commit carries `!`.
