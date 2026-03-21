# Code size analysis

This report breaks down the workspace by lines of code, identifies
duplication, and lists reduction opportunities with estimated effort and
savings. All line counts are from `tokei` (Rust code lines, excluding
comments and blanks) unless otherwise noted.

## Workspace summary

| Crate | Files | Code lines | Comment lines | Blank lines | Total lines |
|-------|------:|----------:|-------------:|----------:|----------:|
| rusty-codecs | 51 | 16,516 | 977 | 1,990 | 19,483 |
| moq-media | 21 | 9,684 | 614 | 1,293 | 11,591 |
| iroh-live | 22 | 5,479 | 257 | 758 | 6,494 |
| rusty-capture | 17 | 3,817 | 218 | 472 | 4,507 |
| moq-media-egui | 2 | 1,198 | 44 | 141 | 1,383 |
| iroh-live-relay | 4 | 764 | 56 | 115 | 935 |
| moq-media-android | 5 | 657 | 40 | 81 | 778 |
| iroh-moq | 1 | 455 | 2 | 47 | 504 |
| moq-media-dioxus | 2 | 357 | 22 | 57 | 436 |
| **Total** | **135** | **43,003** | **2,382** | **5,508** | **50,893** |

rusty-codecs accounts for 38% of all code lines. moq-media is second at
22%. Together they are 60% of the workspace.

## Per-crate breakdowns

### rusty-codecs (16,516 code lines)

Source files by total lines (top 15):

| File | Lines | Purpose |
|------|------:|---------|
| `codec/vaapi/encoder.rs` | 1,484 | VAAPI HW encoder |
| `render/dmabuf_import.rs` | 1,404 | DMA-BUF import + VPP retiler |
| `codec/tests/harness.rs` | 1,284 | Codec test harness |
| `format.rs` | 1,253 | Frame/sample format types |
| `codec/v4l2/encoder.rs` | 1,152 | V4L2 HW encoder |
| `codec/vtb/encoder.rs` | 921 | VideoToolbox encoder |
| `codec/vaapi/decoder.rs` | 791 | VAAPI HW decoder |
| `render.rs` | 738 | wgpu video rendering |
| `libcamera.rs` | 678 | Raspberry Pi camera |
| `processing/convert.rs` | 600 | Colorspace conversion |
| `codec/vtb/decoder.rs` | 594 | VideoToolbox decoder |
| `codec/h264/sps.rs` | 584 | SPS VUI patcher |
| `render/gles.rs` | 554 | GLES rendering (Android) |
| `codec/v4l2/decoder.rs` | 521 | V4L2 HW decoder |
| `codec/h264/encoder.rs` | 458 | openh264 encoder |

The six encoder backends (h264, av1, vaapi, v4l2, vtb, android) total
roughly 4,800 lines. The four decoder backends total roughly 2,250 lines.
Rendering (wgpu + GLES + DMA-BUF + GLES-DMA-BUF) totals 3,100 lines.
Tests total 2,350 lines.

### moq-media (9,684 code lines)

| File | Lines | Purpose |
|------|------:|---------|
| `audio_backend.rs` | 2,332 | Audio output + backends |
| `pipeline.rs` | 1,556 | Encode/decode orchestration |
| `publish.rs` | 1,360 | Publish-side API |
| `adaptive.rs` | 1,035 | Adaptive bitrate selection |
| `subscribe.rs` | 1,026 | Subscribe-side API |
| `playout.rs` | 811 | Playout clock + buffer |
| `stats.rs` | 470 | Stats collection |
| `audio_backend/aec.rs` | 392 | Echo cancellation |
| `test_util.rs` | 320 | Test sources (feature-gated) |
| `publish/controller.rs` | 318 | Publish controller |
| Tests | 1,660 | pipeline_integration + zero_copy |
| Examples (`viewer.rs`) | 963 | |

`audio_backend.rs` at 2,332 lines is the largest single file. The audio
subsystem (backend + AEC) totals 2,724 lines, which is 28% of moq-media.

### iroh-live (5,479 code lines)

| Category | Lines |
|----------|------:|
| `src/` | 1,438 |
| `tests/` | 1,734 |
| `examples/` | 3,761 |

Examples are 69% of the crate's code. The largest examples are `split.rs`
(965 lines) and `call.rs` (920 lines). Core library code is lean at 1,438
lines.

### rusty-capture (3,817 code lines)

| File | Lines | Purpose |
|------|------:|---------|
| `platform/linux/pipewire.rs` | 1,541 | PipeWire capture |
| `platform/linux/v4l2.rs` | 552 | V4L2 camera |
| `lib.rs` | 499 | Public API |
| `platform/apple/screen.rs` | 394 | ScreenCaptureKit |
| `platform/linux/x11.rs` | 373 | X11 screen capture |
| `types.rs` | 340 | Type definitions |
| `platform/nokhwa_impl.rs` | 239 | nokhwa fallback |
| `platform/apple/camera.rs` | 231 | AVFoundation camera |
| `platform/xcap_impl.rs` | 175 | xcap fallback |
| `platform/windows/mod.rs` | 45 | Windows stub |
| `platform/android/mod.rs` | 40 | Android stub |
| Tests | 617 | |

PipeWire capture is the biggest module at 1,541 lines. The fallback
backends (nokhwa + xcap) total 414 lines and could be removed if we commit
to the native backends.


## Duplication and reduction opportunities

### D1: `scale_if_needed` across six encoder backends

**Current state.** Every encoder backend (h264, av1, vaapi, v4l2, vtb,
android) contains a near-identical `scale_if_needed` method. Five of the
six are character-for-character identical (20 lines each). The VAAPI
variant has a small twist: it force-scales to encoder dimensions when the
scale mode returns smaller dims, adding about five extra lines.

**Fix.** Extract a free function or default trait method on a shared
encoder trait. The VAAPI variant can override or call the common
implementation with a post-processing step.

| Metric | Value |
|--------|-------|
| Current lines | ~125 (6 copies) |
| After | ~30 (1 function + 1 VAAPI override) |
| Savings | ~95 lines |
| Effort | trivial |
| Risk | none |

### D2: `config()` duplication across H.264 encoder backends

The `config()` method on `VideoEncoder` returns a `VideoConfig` struct.
Five H.264 backends (openh264, vaapi, v4l2, vtb, android) produce the same
`VideoConfig::H264(...)` body with identical profile/constraints/level
values. They differ only in how they access `self.avcc` (vtb locks a
mutex). Meanwhile, each backend also implements `config_for()` on
`VideoEncoderFactory`, which duplicates the same logic without `&self`.

**Fix.** Add a shared `h264_video_config(width, height, bitrate, framerate,
nal_format, avcc)` helper function. Both `config()` and `config_for()` can
call it.

| Metric | Value |
|--------|-------|
| Current lines | ~100 (5 copies of config + 5 copies of config_for) |
| After | ~25 (1 helper + thin wrappers) |
| Savings | ~75 lines |
| Effort | trivial |
| Risk | none |

### D3: `DynamicVideoDecoder` match-arm boilerplate

`dynamic.rs` (252 lines) contains seven match-arm blocks that delegate
`VideoDecoder` trait methods to the inner variant. Each block has seven
arms (one per backend), and five of the seven blocks are purely mechanical
forwarding. Adding a new backend means touching every block.

**Fix.** A macro or `enum_dispatch` crate could generate the forwarding
impls. `enum_dispatch` is well-maintained and widely used, but it does not
support conditional compilation with `#[cfg]` on variants, which this enum
relies on heavily. A declarative macro is the better fit here.

| Metric | Value |
|--------|-------|
| Current lines | ~170 (forwarding blocks) |
| After | ~40 (macro + `new` + `name`) |
| Savings | ~130 lines |
| Effort | small |
| Risk | low — the `#[cfg]` interactions need careful testing |

### D4: RGBA/BGRA conversion function pairs in `convert.rs`

Four function pairs differ only in whether they call `rgba_*` or `bgra_*`
variants from `yuvutils-rs`:

- `rgba_to_yuv420_data` / `bgra_to_yuv420_data`
- `rgba_to_nv12_data` / `bgra_to_nv12_data`
- `yuv420_to_rgba_from_slices` / `yuv420_to_bgra_from_slices`
- `yuv420_to_rgba_data` / `yuv420_to_bgra_data`

Each pair shares the same body except for the yuvutils function name and
the variable name (`rgba` vs `bgra`).

**Fix.** Parameterize by `PixelFormat` (the `pixel_format_to_yuv420`
pattern already exists for one pair). The public API can stay, with
each function becoming a one-line wrapper around the parameterized core.

| Metric | Value |
|--------|-------|
| Current lines | ~160 (8 functions, duplicated bodies) |
| After | ~80 (4 core functions + 8 one-line wrappers) |
| Savings | ~80 lines |
| Effort | small |
| Risk | none |

### D5: NV12 render pass duplication in `render.rs`

Three methods perform the same bind-group-create-then-render-pass
sequence: `render_nv12`, `render_imported_nv12`, and
`render_imported_metal_nv12`. The only differences are the label strings
and where the Y/UV texture views come from. The render pass setup
(~25 lines each) is identical.

**Fix.** Extract a `render_nv12_from_views(y_view, uv_view, label)` helper.
Each method reduces to texture preparation + a helper call.

| Metric | Value |
|--------|-------|
| Current lines | ~75 (3 copies of the pass) |
| After | ~35 (1 helper + 3 callers) |
| Savings | ~40 lines |
| Effort | trivial |
| Risk | none |

### D6: `DiscoveredVideoSource` in examples

`call.rs` and `split.rs` each define `DiscoveredVideoSource` with
`Display`, `create`, and `discover_video_sources`. The implementations are
nearly identical (~50 lines each).

**Fix.** Move to `examples/common/` as shared example infrastructure.
The `frame_dump.rs` example does not need it, so this is strictly opt-in.

| Metric | Value |
|--------|-------|
| Current lines | ~100 (2 copies) |
| After | ~55 (1 copy in common + 2 imports) |
| Savings | ~45 lines |
| Effort | trivial |
| Risk | none |

### D7: `overlay.rs` `show` / `show_publish` duplication

`show` (lines 120-251) and `show_publish` (lines 254-354) share the same
rendering logic: visibility check, expanded-panel stacking, bottom bar
painting, section measurement, hover effects, click handling. The
differences are: `show` has a timeline panel for the TIME category,
and each method calls its own `detail_entries` / `format_section_summary`
variant.

**Fix.** Parameterize the rendering core with a trait or closure pair for
`detail_entries` and `format_section_summary`. The timeline panel can be
an optional callback. This eliminates ~80 lines of duplicated UI code.

| Metric | Value |
|--------|-------|
| Current lines | ~230 (two methods) |
| After | ~150 (one generic method + 2 thin entry points) |
| Savings | ~80 lines |
| Effort | small |
| Risk | low |

### D8: `libcamera.rs` compiled unconditionally

The `libcamera` module (678 lines) is declared without any `#[cfg]` gate
in `rusty-codecs/src/lib.rs`, even though it is only relevant on Raspberry
Pi. The `raspberry-pi` feature exists but does not gate this module. Every
build compiles code that spawns `rpicam-vid` as a subprocess.

**Fix.** Gate the module with `#[cfg(feature = "raspberry-pi")]`.

| Metric | Value |
|--------|-------|
| Lines affected | 678 |
| Savings | 678 lines compiled on non-Pi targets |
| Effort | trivial |
| Risk | none (additive cfg gate) |

### D9: Dead-code-annotated functions in `convert.rs`

Two functions carry `#[allow(dead_code)]`:

- `yuv420_to_rgba_data` — annotated "symmetric API; prefer from_slices"
- `yuv420_to_bgra_data` — same annotation

If the `from_slices` variants are the preferred API and these functions
have no callers, they should be removed. If they exist for future use,
they should be behind a cfg or documented as part of the public API.

| Metric | Value |
|--------|-------|
| Current lines | ~40 |
| Savings | 40 lines |
| Effort | trivial |
| Risk | none |

### D10: `Nv12Data` struct fields marked dead

The `Nv12Data` struct has `#[allow(dead_code)]` on its stride and
dimension fields with the reason "used by future render paths." If these
fields are not used today, the struct could be simplified. If they are used
through `into_contiguous()` only, the fields are genuinely dead.

| Metric | Value |
|--------|-------|
| Current lines | ~15 |
| Savings | 5-10 lines |
| Effort | trivial |
| Risk | none |

### D11: Fallback capture backends (nokhwa + xcap)

`nokhwa_impl.rs` (239 lines) and `xcap_impl.rs` (175 lines) are fallback
capture backends. If PipeWire and V4L2 are the production paths on Linux,
and Apple/Windows have their own backends, these may be dead weight for
platforms that already have native support.

**Assessment.** These provide Windows/fallback support and are gated behind
features. They are not actively duplicating other code, but they contribute
414 lines that may not be tested or maintained. Worth evaluating whether
they are still needed given the platform stubs.

| Metric | Value |
|--------|-------|
| Current lines | 414 |
| Potential savings | 414 (if removed) |
| Effort | small (need to verify no users) |
| Risk | medium (could break Windows builds if someone depends on them) |

### D12: `format.rs` Apple-specific types

`format.rs` (1,253 lines) contains `CvPixelBufferInfo` (~70 lines) and
`AppleGpuFrame` (~130 lines) with their `unsafe impl Send/Sync` blocks.
These are `#[cfg(target_os = "macos")]`-gated but contribute to cognitive
load. They are not duplicated, but they inflate what is otherwise a
data-types file.

**Assessment.** Moving Apple-specific GPU frame types to a `format/apple.rs`
submodule would improve organization without reducing total lines. Not a
reduction opportunity, but a readability one.

| Metric | Value |
|--------|-------|
| Effort | small |
| Risk | none |

## External crate opportunities

### `enum_dispatch` for `DynamicVideoDecoder` — not viable

As noted in D3, `enum_dispatch` does not handle `#[cfg]` attributes on
enum variants. The workspace's heavy use of conditional compilation per
backend rules it out. A declarative macro is the right tool here.

### No other strong candidates identified

The colorspace conversion code delegates to `yuvutils-rs` already. The
scaling code uses `fast_image_resize`. The codec implementations wrap
C libraries (openh264, rav1e, dav1d, opus). The wgpu rendering is
domain-specific enough that no general crate would help. The SPS VUI
patcher (584 lines) does a specific bit-level operation that no published
crate handles.


## Summary table

| ID | Description | Current | After | Savings | Effort | Risk | Status |
|----|-------------|--------:|------:|--------:|--------|------|--------|
| D1 | `scale_if_needed` | 125 | 30 | 95 | trivial | none | **done** (f8a261e) |
| D2 | H.264 `config()` | 100 | 25 | 75 | trivial | none | **done** (e4fcf92) |
| D3 | `DynamicVideoDecoder` match arms | 170 | 40 | 130 | small | low | **done** (f8d15d2) |
| D4 | RGBA/BGRA conversion pairs | 160 | 80 | 80 | small | none | open |
| D5 | NV12 render pass | 75 | 35 | 40 | trivial | none | open |
| D6 | `DiscoveredVideoSource` | 100 | 55 | 45 | trivial | none | open |
| D7 | `show`/`show_publish` overlay | 230 | 150 | 80 | small | low | open |
| D8 | `libcamera.rs` no cfg gate | 678 | 0* | 678* | trivial | none | **already done** (was cfg-gated) |
| D9 | Dead conversion functions | 40 | 0 | 40 | trivial | none | **done** (f8d15d2) |
| D10 | Dead `Nv12Data` fields | 15 | 5 | 10 | trivial | none | open |
| D11 | Fallback capture backends | 414 | 0 | 414 | small | medium | design decision needed |
| | **Total** | | | **~1,690** | | | |

*D8 does not reduce total workspace lines; it reduces compiled lines on
non-Pi targets. It was already cfg-gated when the report was written.

D1, D2, D3, D8, D9 are complete (~460 lines reduced). D5 and D6 are trivial
remaining items. D4 and D7 are small cleanup tasks. D11 requires a
decision about platform support scope before acting on it.
