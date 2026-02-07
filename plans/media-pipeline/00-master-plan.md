# Media Pipeline Overhaul — Master Plan

## Context

The iroh-live project currently depends on ffmpeg (via `ffmpeg-next`/`ffmpeg-sys-next`), a massive C multimedia library that hurts cross-platform compatibility and ease of use. The goal is to replace it with focused crates and simultaneously build libwebrtc-grade media quality — resilience to packet loss, jitter buffering, adaptive bitrate, and temporal scalability — that the project currently lacks entirely.

## Detailed phase plans

Stored in-repo at `plans/media-pipeline/`. Copied from `.claude/plans/media-pipeline/` during Phase 0.

## Development workflow

- **CLAUDE.md**: Create at repo root with project conventions, build/test/lint commands, and commit guidelines
- **Plans in-repo**: Copy plan files from `.claude/plans/media-pipeline/` into `plans/media-pipeline/` so they're version-controlled and reviewable
- **Small commits**: Each commit must leave all crates compiling, formatting clean (`cargo fmt`), and lints passing (`cargo clippy`). Concise commit messages
- **CI**: No CI config exists yet — manual verification via `cargo build`, `cargo test`, `cargo fmt --check`, `cargo clippy`

## Phase overview

| Phase | Track | Description | Status | Detailed plan |
|-------|-------|-------------|--------|---------------|
| 0 | Setup | Lints, CLAUDE.md, plans in-repo, module file fixes | Pending | below |
| 1 | A - Codec | Codec swap — remove ffmpeg, add openh264 + unsafe-libopus + utilities | Pending | [phase-1-codec-swap.md](media-pipeline/phase-1-codec-swap.md) |
| 2 | A - Codec | HW accel (VAAPI, VideoToolbox) + AV1 (rav1e/rav1d) behind features | Pending | [phase-2-hw-accel-av1.md](media-pipeline/phase-2-hw-accel-av1.md) |
| 3 | B - Resilience | Audio resilience — Opus FEC/PLC/DTX, jitter buffer, comfort noise | Pending | [phase-3-audio-resilience.md](media-pipeline/phase-3-audio-resilience.md) |
| 4 | B - Resilience | Video resilience — adaptive bitrate, frame timing, temporal SVC | Pending | [phase-4-video-resilience.md](media-pipeline/phase-4-video-resilience.md) |

## Phase 0: Project Setup

### 0a. Create `CLAUDE.md` at repo root
Contents:
- Project overview (workspace with 4 crates)
- Build: `cargo build`, `cargo build --all-features`
- Test: `cargo test`, `cargo test --all-features`
- Lint: `cargo clippy --workspace --all-features`, `cargo fmt --check`
- Commit conventions: small incremental commits, all crates compile, clippy clean, fmt clean, concise descriptions
- Crate descriptions (iroh-live, iroh-moq, moq-media, web-transport-iroh)

### 0b. Add workspace lints to `Cargo.toml`
Add to workspace root `Cargo.toml`:
```toml
[workspace.lints.rust]
unexpected_cfgs = { level = "deny", check-cfg = ["cfg(docsrs)"] }
elided_lifetimes_in_paths = "warn"
unnameable_types = "warn"
unreachable_pub = "warn"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
or_fun_call = "warn"
use_self = "warn"
unused_async = "warn"
absolute_paths = "warn"
manual_let_else = "warn"
allow_attributes_without_reason = "warn"
mod_module_files = "deny"
```

Add `[lints] workspace = true` to each crate's `Cargo.toml`:
- `iroh-live/Cargo.toml`
- `iroh-moq/Cargo.toml`
- `moq-media/Cargo.toml`
- `web-transport-iroh/Cargo.toml`

### 0c. Fix `mod_module_files` violations (3 found)
The `mod_module_files = "deny"` lint requires `foo/mod.rs` style, not `foo.rs` + `foo/` directory style. Current violations:

1. `moq-media/src/audio.rs` + `moq-media/src/audio/` → move `audio.rs` to `audio/mod.rs`
2. `iroh-live/src/rooms.rs` + `iroh-live/src/rooms/` → move `rooms.rs` to `rooms/mod.rs`
3. `moq-media/src/ffmpeg/video/util.rs` + `moq-media/src/ffmpeg/video/util/` → move `util.rs` to `util/mod.rs`

### 0d. Fix all other lint warnings
Run `cargo clippy --workspace` and fix all warnings from the new lints. Expected categories:
- `unreachable_pub` — change `pub` to `pub(crate)` on internal items
- `elided_lifetimes_in_paths` — add explicit lifetime annotations
- `missing_debug_implementations` — add `#[derive(Debug)]` where missing
- `use_self` — replace type name with `Self` in impl blocks
- `manual_let_else` — convert `match`/`if let` + return to `let ... else`
- `unused_async` — remove `async` from functions that don't `.await`
- `or_fun_call` — replace `.unwrap_or(expr)` with `.unwrap_or_else(|| expr)` for non-trivial exprs
- `allow_attributes_without_reason` — add reasons to any `#[allow(...)]`
- `absolute_paths` — use imports instead of absolute paths

Each lint category should be a separate small commit.

### 0e. Copy plans into repo
- Create `plans/media-pipeline/` directory in repo root
- Copy plan files from `.claude/plans/media-pipeline/` into `plans/media-pipeline/`
- Commit as "docs: add media pipeline overhaul plans"

### 0f. Verify
- `cargo build --workspace` — compiles
- `cargo clippy --workspace` — no warnings
- `cargo fmt --check` — clean
- `cargo test --workspace` — passes

## Summary of each phase

### Phase 1: Codec Swap
Remove `ffmpeg-next`/`ffmpeg-sys-next`. Replace with:
- `openh264` — H.264 encode/decode (bundled C via `cc`)
- `unsafe-libopus` — Opus encode/decode (c2rust, no C compiler)
- `yuvutils-rs` — YUV↔RGBA (pure Rust, SIMD)
- `pic-scale` — image scaling (pure Rust)
- `rubato` — audio resampling (pure Rust, SIMD)
- `image` crate — MJPEG decode (already a dependency)

Fixes 7 bugs found in review (3x frame copies, viewport bug, unwrap, etc). Optimizes encode/decode pipelines from 3 copies to 1 conversion.

**Tests**: 9 unit test suites (YUV conversion, scaling, MJPEG, Annex B, resampling, Opus enc/dec, H.264 enc/dec) + 3 integration test suites (full pipeline, trait compliance, no-ffmpeg build).

### Phase 2: HW Accel + AV1
Feature-gated optional backends:
- `vaapi` — `cros-codecs` for Linux GPU encoding
- `videotoolbox` — `objc2-video-toolbox` for macOS GPU encoding
- `av1` — `rav1e` (encoder) + `rav1d` (decoder), pure Rust

Backend selector tries HW first, falls back to openh264.

**Tests**: 5 unit test suites (backend selection, AV1 enc/dec, VAAPI, VideoToolbox) + 3 integration test suites (cross-codec interop, feature-gate isolation, dynamic backend switching).

### Phase 3: Audio Resilience
- Opus in-band FEC with adaptive loss percentage
- 1-frame lookahead FEC recovery on decoder
- Packet loss concealment (PLC) via Opus native comfort noise
- DTX for bandwidth savings during silence
- Adaptive audio jitter buffer (BTreeMap-based, EMA-smoothed target depth)
- Loss rate feedback loop: observed loss → encoder FEC tuning

**Tests**: 4 unit test suites (jitter buffer — 10 scenarios, FEC/PLC — 5 scenarios, DTX — 4 scenarios, loss feedback — 4 scenarios) + 5 integration test suites (packet loss simulation at 5/10/20/30%, jitter simulation, combined loss+jitter, DTX bandwidth measurement, 60s stability).

### Phase 4: Video Resilience
- Adaptive bitrate (AIMD algorithm, monitor QUIC backpressure)
- Dynamic `set_bitrate()` on encoder backends
- EMA-smoothed frame delay for stable playback
- Frame freeze (hold last frame on timeout, don't stall)
- Late frame detection and skip
- OpenH264 temporal SVC (2-4 layers, selective layer dropping)
- Quality metrics observability (`VideoStats` struct)

**Tests**: 5 unit test suites (ABR algorithm — 7 scenarios, frame timing — 4 scenarios, late frames — 5 scenarios, temporal SVC — 5 scenarios, quality metrics — 5 scenarios) + 6 integration test suites (ABR simulation, frame timing simulation, late frame simulation, temporal SVC end-to-end, degradation path, 120s stability).
