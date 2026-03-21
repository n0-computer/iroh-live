# Dependency weight reduction

See the dependency analysis in
[docs/architecture/performance.md](../docs/architecture/performance.md)
for build-time measurements and per-crate dependency counts.

## Checklist

- [x] Remove `xcap` and `nokhwa` from rusty-capture default features
- [x] Exclude moq-media-dioxus from default workspace members
- [x] Remove `av1` from iroh-live default features (moved to best-perf)
- [ ] Align bindgen versions across cros-codecs, pipewire-sys,
  openh264-sys2 (three copies, 15–19s each; requires upstream PRs)
- [ ] Investigate lighter alternative to `image` crate — used for
  `RgbaImage` type and MJPEG decode; deeply embedded in VideoFrame API
- [ ] Unify crypto backends (ring + aws-lc-sys) — requires upstream
  changes in iroh and moq ecosystems; not actionable short-term
