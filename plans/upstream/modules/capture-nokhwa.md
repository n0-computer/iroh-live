# nokhwa Cross-Platform Camera Fallback

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path.

## What it is

nokhwa is iroh-live's cross-platform CPU camera fallback
(`rusty-capture/src/platform/nokhwa_impl.rs`, 246 LOC), delivering RGBA frames
through the `nokhwa` 0.10 crate. It exists only to stand in for the backends
iroh-live never finished: the macOS AVFoundation camera (a stub) and the Windows
Media Foundation camera (a stub). Once those platforms adopt moq's native
backends, nokhwa has no remaining role, so it is deleted. This is a drop of a
redundant fallback; it is framed as "use moq version, remove iroh-live version"
because moq's native macOS and Windows camera backends supersede it, and moq
removed nokhwa on its own side on purpose.

## iroh-live vs moq

iroh-live's `nokhwa_impl.rs` uses `nokhwa` 0.10 with `input-native` plus
`camera-sync-impl` (making `Camera: Send`, so `pop_frame` runs on the caller
thread), producing CPU RGBA only. It maps nokhwa `FrameFormat` to
`CapturePixelFormat` and enumerates via `nokhwa::query(ApiBackend::Auto)`. moq
has no nokhwa backend; it removed nokhwa deliberately in favor of native
backends ("replacing nokhwa", their `v4l2.rs:1`). moq's native macOS
AVFoundation and Windows Media Foundation camera backends cover exactly the
platforms nokhwa was standing in for, and both are stronger (zero-copy, TCC and
D3D11 handling), so nokhwa is redundant once they are adopted.

## What to do

Delete `rusty-capture/src/platform/nokhwa_impl.rs` (246 LOC) once the adopted
macOS camera and Windows backends are proven. There is no re-entry condition of
consequence: the reason the deletion loses nothing we need is that Linux cameras
are covered by V4L2, PipeWire, and libcamera, and macOS and Windows are covered
by moq's adopted native backends.

1. Do not touch nokhwa until both the moq macOS AVFoundation camera backend
   (`capture-macos-camera.md`) and the moq Windows Media Foundation camera
   backend (`capture-windows.md`) have passed their R-g on-hardware smoke tests,
   because nokhwa is today the only working macOS camera and Windows camera path.
2. Once both are proven, delete `nokhwa_impl.rs` in a deletion-only commit.
3. Remove nokhwa from the `CameraCapturer` selection cascade and the
   `CaptureBackend` enum in `lib.rs` and `types.rs`, and drop the `nokhwa`
   feature and dependency from `Cargo.toml`. The macOS camera cascade currently
   orders nokhwa before AVFoundation (`lib.rs:198-203`); that ordering is
   removed with the adoption of moq's AVFoundation backend.

## Tests

There is no nokhwa-specific hardware gate to preserve; the gating that matters is
the R-g on-hardware smoke tests of the moq backends that replace it, tracked in
`capture-macos-camera.md` and `capture-windows.md`. CI on Linux hosts continues
to exercise the kept Linux camera backends (V4L2, PipeWire, libcamera), which is
why the drop loses no working coverage. Deletion is a deletion-only commit that
keeps `cargo make check-all` green.

## Evidence

- ../comparison/capture.md, section 2 "Backends only we have" (nokhwa and xcap
  are CPU cross-platform fallbacks; "moq removed nokhwa on purpose", their
  `v4l2.rs:1`; for us they are the only working path for macOS camera and all of
  Windows, "a symptom, not a feature").
- ../comparison/capture.md, section 5 (X11, nokhwa, xcap "keep as fallbacks;
  they cover portal-less Linux and are currently the only working path for macOS
  camera and Windows, which the next two items should eliminate").
- ../comparison/maps/rusty-capture.md, section 2, the nokhwa row
  (`platform/nokhwa_impl.rs`, 246 lines, CPU RGBA).
- The removal ledger is in `capture-remove.md` (the nokhwa fallback row, "cut
  after macOS+Windows adopt") and the DISPOSITION register (drop,
  `platform/nokhwa_impl.rs`, "Acceptable because Linux cameras are covered by
  V4L2, PipeWire, and libcamera").

## Coordination

- The release gate: nokhwa's removal waits on the release bump carrying moq's
  merged capture stack, because its replacements are moq's macOS camera and
  Windows camera backends.
- Adopt-first ordering: nokhwa is deletable only after both
  `capture-macos-camera.md` and `capture-windows.md` pass their R-g gates, since
  nokhwa is the sole working path for those platforms until then.
- No base plan or upstream contribution is needed; this is a local drop with no
  moq-side change.
