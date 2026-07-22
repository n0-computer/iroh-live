# B4. Public registerable Backend trait + registration

Branch: moq-upstream/b4-backend-registration (its own branch, off base; not folded into the base PR)          PR target: base branch, then moq main
Depends on: B1 (the public `Frame`/`Native` vocabulary the trait traffics in), B2 (the `encode` timestamp signature and `Packet`)
Path: B only (external-backend path); BREAKING
Size: M (roughly 250 lines across both sides)

## Goal

Publish moq's encode and decode `Backend` traits as additive-sealed, and add a
registration API so an out-of-tree crate can contribute a codec backend without
editing moq's `const` candidate tables. Today both traits are `pub(crate)`
(`encode/backend/mod.rs:37`, `decode/backend/mod.rs:67`), the candidate tables are
private `const` slices, and `Kind::Named(String)` routes only among the built-ins:
an out-of-tree backend is impossible without a source change to moq. B4 makes the
traits public (trafficking only in public vocabulary types so no codec internal is
exposed), converts the tables to a `OnceLock<Vec<Candidate>>` seeded from the
built-ins plus a registered slice, and adds `register_encoder` and a non-mirror
`register_decoder`. This is the only breaking change in the whole program and the
only one exclusive to Path B; an in-tree backend needs none of it. It is
CONDITIONAL on the Android placement decision and must not be opened as a PR until
that decision is made with the maintainer.

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
   Change `pub(crate) trait Backend` to `pub trait Backend`, documented as
   additive-sealed: external crates implement it, but only through the public
   `Frame`/`Native` input and the public `Packet` output (B1, B2), so moq never
   exposes a codec-internal type. The method set is B2's:

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
   /// A registerable encoder backend: a name, the codecs it emits, and an opener.
   #[non_exhaustive]
   pub struct Registration {
       pub name: &'static str,
       pub codecs: &'static [Codec],
       pub open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
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
   }

   /// Register an external decoder backend, consulted alongside the built-in decode
   /// candidates in priority order. Call once at startup, before the first
   /// `Decoder::new`.
   pub fn register_decoder(reg: DecodeRegistration);
   ```

   Publishing the decode `Backend` also requires publishing `Codec`
   (`decode/backend/mod.rs:36-41`, currently `pub(crate)`) and `Decoded`
   (`:53-62`, currently `pub(crate)`), since they appear in the trait's signature
   and the registration. `Decoded` becoming public means its `frame:
   crate::frame::Frame` field must be reachable as public `Frame`/`Native`, so B1's
   vocabulary is a hard prerequisite here.

4. **Convert the tables to `OnceLock<Vec<Candidate>>`.** On the encode side the
   `const HARDWARE` and `const SOFTWARE` slices (`:68-102`) become a
   `static REGISTERED: OnceLock<Vec<Candidate>>` seeded lazily from the built-in
   slices plus whatever `register_encoder` pushed. `open` (`:106-134`) chains the
   registered candidates: `Kind::Auto` sees them after built-in hardware and before
   software, `Kind::Hardware` includes those a registrant flags as hardware, and
   `Kind::Named(name)` can select one by name. No change to the public `Kind` enum
   (`encode/encoder.rs:34-48`); `Named(String)` already routes by name.

   On the decode side the `const HARDWARE` slice and the single `const SOFTWARE`
   `Candidate` (`:89-114`) fold into the same `OnceLock<Vec<Candidate>>` pattern;
   `open(codec, config)` (`:119-146`) chains the registered slice onto its scan and
   filters by each candidate's `supports` predicate, exactly as it filters the
   built-ins today. Note the decode `SOFTWARE` is currently a single `const
   Candidate`, not a slice, so the conversion normalizes it into the `Vec` rather
   than concatenating two slices.

5. **Registration storage.** `register_encoder`/`register_decoder` push onto a
   `Mutex<Vec<Registration>>` (or append into the `OnceLock`'s `Vec` before it is
   first read). Document the "call before first `Encoder::new`/`Decoder::new`"
   contract, since the `OnceLock` snapshots the candidate list on first `open`.

## Implementation steps

1. Publish the encode `Backend` trait and add `Registration` + `register_encoder`
   with the `OnceLock<Vec<Candidate>>` conversion; keep the built-in candidates
   seeded first so priority order is unchanged when nothing is registered.
2. Publish the decode `Backend` trait, `Codec`, and `Decoded`; add
   `DecodeRegistration` + `register_decoder` with its `supports`-predicate opener
   and the same `OnceLock` conversion, normalizing the single `SOFTWARE` const into
   the `Vec`.
3. Thread the registered slice into both `open` functions so all `Kind` arms consult
   it, with no change to `Kind`.
4. Add crate-root re-exports for the newly public `Backend`, `Registration`,
   `DecodeRegistration`, `Packet`, `Codec`, `Decoded`, `register_encoder`, and
   `register_decoder`, and document the additive-sealed contract in the module docs
   and the `lib.rs` API-stability section (`lib.rs:35-44`), stating that the trait
   traffics only in vocabulary types.
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

- The sealing tradeoff, stated for moq: a public `Backend` trait plus registration
  turns the frame vocabulary, the `Packet` shape, and the `Registration`/`Kind`
  interaction into semver surface that review can no longer reshape freely
  (`comparisons/moq-changes.md` section 2, "The tradeoff, stated for moq"). The
  mitigation is that the trait only ever sees public vocabulary types, `Packet` and
  the registrations are `#[non_exhaustive]`, and new `Frame` variants are cfg-gated
  and additive, so the only genuinely breaking future move is a new required trait
  method, avoidable by defaulting.
- Conventional-commit the trait publication with `!` for the breaking change and the
  `moq-video` scope.
- Do not refactor the candidate tables beyond the `OnceLock` conversion; leaves add
  their own `const Candidate` (coordination point 2) and a table redesign from B4
  would conflict with every in-tree leaf.

## Coordination

- Coordination point 6 (the B4 breaking change): this is the only breaking item and
  is worth it only if moq wants out-of-tree backends. It gates on the Android
  placement decision (in-tree with its NDK build cost, or external over
  registration). Do NOT open B4 as a PR until that decision is made with the
  maintainer. In-tree VAAPI, V4L2, and AV1 backends need only B1, B2, and B3 and add
  a `const Candidate`; they must not wait on B4.
- Coordination point 1 (base API freeze): the `Registration` and `DecodeRegistration`
  shapes are frozen contract for the android-mediacodec leaf, which is the sole
  consumer.

## Acceptance checklist

- [ ] Both `Backend` traits are `pub` and documented additive-sealed, trafficking
      only in `Frame`/`Native`/`Packet`/`Codec`/`Decoded` public types.
- [ ] `Registration`, `DecodeRegistration`, `register_encoder`, `register_decoder`
      match the frozen contract verbatim; decode is the non-mirror `supports` +
      codec-taking-opener shape.
- [ ] The candidate tables are `OnceLock<Vec<Candidate>>` seeded from the built-ins;
      priority order is unchanged when nothing is registered; the single decode
      `SOFTWARE` const is normalized into the `Vec`.
- [ ] `Kind::{Auto,Hardware,Named}` all consult registered candidates; the public
      `Kind` enum is unchanged.
- [ ] Registration and priority-order tests pass in CI.
- [ ] The PR is opened only after the Android placement decision; the breaking
      commit carries `!`.
