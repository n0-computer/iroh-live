# xcap Cross-Platform Screen Fallback

VERDICT: use moq version, remove iroh-live version

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: Not on a zero-copy path.

## What it is

xcap is iroh-live's cross-platform CPU screen-capture fallback
(`rusty-capture/src/platform/xcap_impl.rs`, 175 LOC), delivering RGBA
screenshots through the `xcap` 0.9 crate with a sleep-based frame-rate limiter.
It stands in for the screen backends iroh-live never finished on the non-Linux
platforms, and moq's Windows Desktop Duplication and macOS ScreenCaptureKit
backends supersede it. Once macOS and Windows adopt moq's native backends, xcap
has no remaining role and is deleted on the same gate as nokhwa. This is a drop
of a redundant fallback, framed as "use moq version, remove iroh-live version"
because moq's native backends supersede it.

## iroh-live vs moq

iroh-live's `xcap_impl.rs` uses `xcap` 0.9 (X11, Wayland-portal, macOS, Windows).
`XcapScreenCapturer` captures whole-monitor screenshots via
`Monitor::capture_image`, converts to RGBA, and rate-limits with a sleep-based
limiter. It is CPU only, with no zero-copy path. moq has no xcap backend; its
native screen backends (Windows `desktopduplication`, macOS `screencapture`)
cover the platforms xcap was standing in for. On Linux, X11 remains the
portal-less screen fallback that moq lacks entirely (moq answers Linux screen
without the `pipewire` feature with `Error::Unsupported`), so the Linux screen
fallback role passes to X11, not away from iroh-live; see `capture-x11.md`.

## What to do

Delete `rusty-capture/src/platform/xcap_impl.rs` (175 LOC) once the adopted
macOS and Windows screen backends are proven. The reason the deletion loses
nothing we need: X11 remains the portal-less Linux screen fallback, and moq's
adopted native backends cover macOS and Windows screen capture.

1. Do not touch xcap until moq's macOS `screencapture` backend
   (`capture-macos-screen.md`) and moq's Windows `desktopduplication` backend
   (`capture-windows.md`) have passed their R-g on-hardware smoke tests, because
   xcap is today the only working macOS and Windows screen fallback outside those
   native backends.
2. Once both are proven, delete `xcap_impl.rs` in a deletion-only commit.
3. Remove xcap from the `ScreenCapturer` selection cascade and the
   `CaptureBackend` enum in `lib.rs` and `types.rs`, and drop the `xcap` feature
   and dependency from `Cargo.toml`. The Linux screen cascade (PipeWire, then
   X11) is unchanged.

## Tests

There is no xcap-specific hardware gate to preserve; the gating that matters is
the R-g on-hardware smoke tests of the moq backends that replace it, tracked in
`capture-macos-screen.md` and `capture-windows.md`. CI on Linux hosts continues
to exercise the kept Linux screen backends (PipeWire and X11), so the drop loses
no working coverage. Deletion is a deletion-only commit that keeps
`cargo make check-all` green.

## Evidence

- ../comparison/capture.md, section 2 "Backends only we have" (nokhwa and xcap
  are CPU cross-platform fallbacks; for us they are the only working path for
  macOS camera and all of Windows, "a symptom, not a feature"), and the X11 note
  that moq's Linux screen story without the `pipewire` feature is
  `Error::Unsupported`.
- ../comparison/capture.md, section 5 (X11, nokhwa, xcap "keep as fallbacks;
  they cover portal-less Linux and are currently the only working path for macOS
  camera and Windows, which the next two items should eliminate").
- ../comparison/maps/rusty-capture.md, section 2, the xcap row
  (`platform/xcap_impl.rs`, 175 lines, CPU RGBA, sleep-based fps limiter).
- The removal ledger is in `capture-remove.md` (the xcap fallback row, "cut
  after macOS+Windows adopt") and the DISPOSITION register (drop,
  `platform/xcap_impl.rs`, "X11 remains the portal-less Linux screen fallback and
  the adopted backends cover macOS and Windows").

## Coordination

- The release gate: xcap's removal waits on the release bump carrying moq's
  merged capture stack, because its replacements are moq's macOS and Windows
  screen backends.
- Adopt-first ordering: xcap is deletable only after both
  `capture-macos-screen.md` and `capture-windows.md` pass their R-g gates, on the
  same gate as `capture-nokhwa.md`.
- Linux coverage is unaffected: X11 (`capture-x11.md`) stays as the portal-less
  Linux screen fallback.
- No base plan or upstream contribution is needed; this is a local drop with no
  moq-side change.
