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
| 0 | Setup | Lints, CLAUDE.md, plans in-repo, module file fixes | **Done** | below |
| 1 | A - Codec | Codec swap — remove ffmpeg, add openh264 + unsafe-libopus + utilities | **Done** | [phase-1-codec-swap.md](media-pipeline/phase-1-codec-swap.md) |
| 2 | A - Codec | AV1 codec support (rav1e encoder + dav1d decoder) | **Done** | [phase-2-av1.md](media-pipeline/phase-2-av1.md) |
| 2b | A - Codec | HW acceleration (VAAPI, VideoToolbox) | Pending | [phase-2b-hw-accel.md](media-pipeline/phase-2b-hw-accel.md) |
| 3 | B - Resilience | AV resilience — overview of 3a–3d | Pending | [phase-3-av-resilience.md](media-pipeline/phase-3-av-resilience.md) |
| 3a | B - Resilience | Adaptive rendition switching — catalog-aware, signal-driven | Pending | [phase-3a-rendition-switching.md](media-pipeline/phase-3a-rendition-switching.md) |
| 3b | B - Resilience | Jitter buffer & A/V sync — playout timing, adaptive latency, lip-sync | Pending | [phase-3b-jitter-sync.md](media-pipeline/phase-3b-jitter-sync.md) |
| 3c | B - Resilience | Forward error correction — Opus FEC/PLC/DTX, comfort noise | Future | [phase-3c-fec.md](media-pipeline/phase-3c-fec.md) |
| 3d | B - Resilience | Adaptive encoding — encoder rate control, bandwidth estimation, pacing | Future | [phase-3d-adaptive-encoding.md](media-pipeline/phase-3d-adaptive-encoding.md) |

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

### Phase 2: AV1 Codec Support (Done)
AV1 encode/decode behind default-on `av1` feature flag:
- `rav1e` 0.8 — pure Rust AV1 encoder (speed preset 10, low-latency)
- `rav1d` — pure Rust AV1 decoder
- `DynamicVideoDecoder` — enum dispatch, auto-routes H264→openh264, AV1→rav1d
- `DefaultDecoders` updated to use `DynamicVideoDecoder`
- Examples updated with `--codec` CLI arg

**Tests**: 84 passing (default), 71 without `av1` feature. Encoder tests, decoder tests, roundtrip tests, DynamicVideoDecoder routing tests.

### Phase 2b: HW Acceleration (Pending)
Feature-gated optional HW encoder backends:
- `vaapi` — `cros-codecs` for Linux GPU encoding
- `videotoolbox` — `objc2-video-toolbox` for macOS GPU encoding

Backend selector tries HW first, falls back to openh264.

### Phase 3: AV Resilience
Subscriber-side and publisher-side media quality improvements, structured as four sub-phases:

**Phase 3a: Adaptive Rendition Switching** — WatchTrack and AudioTrack switch renditions on-demand based on injected network signals (RTT, loss, bandwidth). Catalog-aware selection inspects metadata (resolution, bitrate, codec). Seamless switching runs old and new decoders in parallel during handoff.

**Phase 3b: Jitter Buffer & A/V Sync** — Frame-level playout timing on top of hang's group-level management. PlayoutClock (shared `Arc<Mutex>`) coordinates A/V sync. PlayoutMode: `Auto { min, max }` (adapts to jitter) / `Fixed(Duration)` (user-controlled). Audio is sync master.

**Phase 3c: Forward Error Correction (Future)** — Opus in-band FEC with adaptive loss percentage. 1-frame lookahead FEC recovery. PLC via Opus native comfort noise. DTX for bandwidth savings during silence.

**Phase 3d: Adaptive Encoding (Future)** — `set_bitrate()` / `force_keyframe()` on encoder trait + all backends. Bandwidth estimation from QUIC PathStats (reuses NetworkSignals). Transport-aware rate control. Quality degradation state machine. Sender-side frame pacing.
