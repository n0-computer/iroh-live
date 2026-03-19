# Dependency weight reduction

- [ ] Remove `av1` from moq-media default features
- [ ] Remove `videotoolbox` and `metal-import` from moq-media default features
- [ ] Remove `xcap` and `nokhwa` from rusty-capture default features
- [ ] Exclude moq-media-dioxus from default workspace members
- [ ] Align bindgen versions across cros-codecs, pipewire-sys, openh264-sys2
- [ ] Investigate lighter alternative to `image` crate for RgbaImage + JPEG

## Current state (2026-03-19)

793 unique crates in the full workspace build. 789 with default features alone,
meaning feature gating saves almost nothing. Full dev build takes 2m 26s on an
Intel Meteor Lake desktop (incremental warm).

## Heaviest dependencies by build time

Measured via `cargo build --workspace --all-features --timings`:

| Crate | Build time | Source LOC | Kind |
|-------|-----------|------------|------|
| moq-media | 33.6s | 9K Rust (ours) | workspace |
| iroh-live-relay | 31.6s | 1K Rust (ours) | workspace |
| iroh | 30.7s | ~20K Rust | upstream |
| egui | 30.1s | ~30K Rust | UI (examples + moq-media-egui) |
| blitz-dom | 27.6s | ~15K Rust | dioxus-native renderer |
| iroh-gossip | 26.2s | ~8K Rust | upstream |
| iroh-moq | 25.0s | 2K Rust (ours) | workspace |
| rav1e | 22.5s | 68K Rust + 62K C/asm | AV1 encoder |
| reqwest | 22.1s | ~5K Rust | HTTP client (iroh dep) |
| hickory-proto | 21.9s | ~20K Rust | DNS (iroh dep) |
| bindgen (x3) | 15-19s each | -- | C binding generator |
| image | 16.2s | 9K Rust | image format library |

## Heaviest dependencies by source size

| Crate | Rust LOC | C/C++ LOC | Purpose |
|-------|----------|-----------|---------|
| aws-lc-sys | 416K | 355K | TLS crypto (rustls backend) |
| ring | -- | 151K | TLS crypto (noq-proto, rcgen) |
| openh264-sys2 | 2K | 124K | H.264 software codec |
| naga | 109K | -- | wgpu shader compiler |
| rav1e | 68K | 62K | AV1 encoder |
| unsafe-libopus | 56K | -- | Opus codec (c2rust port) |
| wgpu-hal | 50K | -- | wgpu GPU abstraction |
| cros-codecs | 49K | -- | VAAPI hardware codec |
| noq-proto | 44K | -- | QUIC protocol (iroh's quinn fork) |

## Per-crate dependency counts (default features)

| Crate | Deps | Role |
|-------|------|------|
| rusty-codecs | 93 | codec implementations |
| moq-media-android | 186 | Android integration |
| rusty-capture | 216 | capture (includes xcap + nokhwa fallbacks) |
| moq-media-egui | 232 | egui video widget |
| iroh-moq | 260 | MoQ transport |
| moq-media | 309 | media pipelines |
| iroh-live-relay | 418 | relay server |
| iroh-live | 438 | high-level API |
| moq-media-dioxus | 470 | dioxus video widget |

## Analysis

### Dual crypto backends (aws-lc-sys + ring)

Both `ring` (151K C) and `aws-lc-sys` (355K C) are compiled. `ring` is
needed by noq-proto (iroh's QUIC implementation) and rcgen. `aws-lc-rs` is
needed by rustls (via moq-token, jsonwebtoken) and rcgen. This is roughly
500K lines of C just for crypto. Unifying would require upstream changes in
iroh and moq ecosystems, so this is not actionable short-term.

### rusty-capture defaults (216 vs 76 deps)

`xcap` and `nokhwa` are cross-platform fallback capture libraries enabled by
default. They add 140 transitive deps but are rarely used on Linux (PipeWire
covers screen + camera) or macOS (ScreenCaptureKit + AVFoundation). Making
them opt-in would drop rusty-capture from 216 to 76 deps.

### moq-media defaults pull AV1 + macOS features

`av1` (rav1e, 22.5s build), `videotoolbox`, and `metal-import` are default
features. AV1 is rarely needed for real-time streaming. VideoToolbox and
Metal import are macOS-only and compile as no-ops on Linux. Removing them
from defaults would save build time on the most common development platform
without affecting functionality for users who opt in.

### moq-media-dioxus is the heaviest workspace member

470 deps, 224 unique to it (blitz-dom, vello, anyrender, etc.). This crate
is prototype-quality and pulls in an entire browser rendering engine.
Excluding it from default workspace members would significantly reduce
`cargo build --workspace` time without affecting other crates.

### Three bindgen versions

cros-codecs pins bindgen 0.65, pipewire-sys pins 0.70, openh264-sys2 pins
0.72. Each bindgen invocation takes 15-19s to compile. Aligning to a single
version would eliminate two of the three compilations. This requires upstream
PRs or version bumps in those dependencies.

### image crate weight

Used for `RgbaImage` type (pixel buffer with width/height) and MJPEG
decoding. Even with `default-features = false`, the image crate pulls in
format support and takes 16.2s. A lighter alternative for just the RGBA
buffer type and JPEG decode would help, but the image crate's `RgbaImage`
is deeply embedded in the `VideoFrame` API surface so migrating away is a
larger refactor.

## Recommended priority

The first four items (remove av1/vtb/metal from moq-media defaults, remove
xcap/nokhwa from rusty-capture defaults, exclude dioxus from workspace
members) are one-line Cargo.toml changes with no code modifications. They
would reduce default workspace deps from ~789 to roughly 550-600 and shave
about 50s from a clean build.

The bindgen alignment requires upstream coordination. The crypto unification
and image replacement are longer-term structural changes.
