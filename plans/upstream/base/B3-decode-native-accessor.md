# B3. decode::Frame::native() accessor

Branch: moq-upstream/base (lands after B1 in the base series)          PR target: base branch, then moq main
Depends on: B1 (the `Native` vocabulary this accessor returns)
Path: Both (needed for Path A and Path B)
Size: S (roughly 40 lines)

## Goal

Give a decoded GPU frame a public exit that is not a CPU download. Today
`decode::Frame` (`rs/moq-video/src/decode/mod.rs:36-46`) leaves the crate only via
`resize` (`:57-85`) or `into_i420` (`:94-101`, a CPU copy); a GPU-resident frame is
downloaded to system memory even though the surface is right there. B3 adds
`decode::Frame::native() -> Option<Native>`, which returns the platform GPU handle
when the frame is GPU-resident and `None` when it is CPU I420, so a renderer or a
zero-copy consumer can import the surface directly. `into_i420` stays as the
universal CPU fallback. This is the one accessor the out-of-tree renderer (render
leaf, Option B) and the decode-to-render leaves need, and it is what gives the
`Native` vocabulary something to return on Linux and Apple.

## Evidence

- `decode::Frame` holds the private union already: `pub(crate) inner:
  crate::frame::Frame` (`decode/mod.rs:45`), so `native()` is a match producing the
  public handle, not new plumbing.
- The existing exits are a CPU copy: `into_i420` matches `inner` and downloads any
  non-I420 variant (`decode/mod.rs:94-101`), and `resize` likewise
  (`decode/mod.rs:57-85`). There is no handle exit today.
- moq downloads on decode everywhere except NVDEC, and even NVDEC's `Frame::Cuda` is
  `pub(crate)` and produced only to feed NVENC (`comparisons/moq-changes.md`
  section 1b, problem one). B3 is the accessor; the decode backends that keep their
  surface instead of downloading (VAAPI decode, VideoToolbox and Media Foundation
  retain-surface) are separate leaves that depend on B3.
- The design is `comparisons/moq-changes.md` section 1b change 1 and
  `comparisons/traits-api.md` section 2.2 (the "no decode-to-render GPU handoff"
  gap). Verified against `/home/bit/Code/rust/moq` at HEAD `3a3e0ea8`.

## moq API consumed

- `Native` from B1 (the return type). B3 is the thin public wrapper over the
  `pub(crate)` conversion B1 lands on `crate::frame::Frame`.

## Source to port

Our reference is `GpuFrameInner::native_handle` (`rusty-codecs/src/format.rs:461-466,
495-502`), which computes the handle on demand and returns `Option<NativeFrameHandle>`,
`None` for a CPU frame. B3 mirrors exactly that contract on `decode::Frame`. No code
body is copied; the mapping is a match over moq's own `inner`.

## Target in moq

`rs/moq-video/src/decode/mod.rs`, a new method in the `impl Frame` block beside
`into_i420` (`:94-101`):

```rust
impl Frame {
    /// The platform GPU handle when the frame is GPU-resident; `None` for a CPU
    /// I420 frame. Lets a renderer or a re-encoder import the surface without a CPU
    /// round trip. Use [`into_i420`](Self::into_i420) for the universal CPU path.
    pub fn native(&self) -> Option<Native> {
        self.inner.native()   // the pub(crate) conversion B1 lands on crate::frame::Frame
    }
}
```

The `crate::frame::Frame::native(&self) -> Option<Native>` conversion is B1's step 5
(the private plumbing), so B3 is just the one public method plus a `use` of `Native`.
If B1 and B3 land in one base PR, fold the conversion into B3; the split exists so B1
can land the vocabulary without waiting on B3.

## Implementation steps

1. Import `Native` into `decode/mod.rs` (from the crate root re-export or the
   `moq-frame` crate, per B1's home decision).
2. Add the `native()` method returning `self.inner.native()`.
3. If B1 did not already land the `pub(crate)` conversion on `crate::frame::Frame`,
   land it here: a match mapping each GPU variant to its `Native` arm and `I420` to
   `None`, cfg-gated to mirror the variants.
4. Document that a CPU frame returns `None` and that `into_i420` remains the
   fallback, so a caller can branch on the handle and degrade cleanly.

## Tests

- A hardware-free shape test: build a `decode::Frame` wrapping a CPU `I420` (the
  decode tests already construct these) and assert `native()` is `None` and
  `into_i420()` still works.
- The GPU arms returning `Some(Native::…)` are exercised by the decode-backend
  leaves' hardware-gated round trips (vaapi-decode, vtb-mf-decode-surface); B3 ships
  only the `None`-for-CPU assertion, which runs in CI everywhere.
- The `frame_and_consumer_are_thread_safe` compile check (`decode/mod.rs:104-118`)
  stays green; `Native` crossing the accessor must be `Send + Sync`.

## Adaptation notes

- `native()` returns an owned `Option<Native>` computed on demand, mirroring our
  `native_handle` contract: no fd or surface is held per frame beyond what the
  `inner` variant already retains, and `DmaBuf::export` (B1) dups fresh on access.
- `into_i420` is unchanged and remains the documented universal fallback, so a
  consumer that cannot use a given handle (an unsupported modifier, a missing
  importer) always has the CPU path (`comparisons/moq-changes.md` section 1b,
  Option B: "falls back to `into_i420()` when a zero-copy path fails").
- No backend name appears: `native()` returns `Native`, a vocabulary type, upholding
  the `lib.rs:37-44` rule.

## Coordination

- Depends on B1 (coordination point 1, base API freeze): B3 cannot land before the
  `Native` shape is agreed, because it returns it. In the base series B1 precedes B3.
- No shared-file conflict; B3 touches only `decode/mod.rs`.

## Acceptance checklist

- [ ] `decode::Frame::native(&self) -> Option<Native>` is public and matches the
      frozen contract verbatim.
- [ ] Returns `None` for a CPU I420 frame and the matching `Native` arm for each GPU
      variant.
- [ ] `into_i420` and `resize` are unchanged.
- [ ] The `None`-for-CPU shape test passes in CI on every platform.
- [ ] The thread-safety compile check still passes.
