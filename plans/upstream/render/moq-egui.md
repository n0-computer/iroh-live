# moq-egui. Move moq-media-egui into the moq workspace over moq-video-render

> Campaign: upstream | Kind: leaf plan | Branch: up/moq-egui |
> PR target: moq monorepo | Read ../0-overview.md first.
Depends on: moq-video-render (the crate this one renders through), and
transitively B1 and B3. Wave 3.
Path: A (in-tree, off-default workspace member)
Size: M

## Goal

Move iroh-live's `moq-media-egui` crate into the moq workspace as a new crate
`moq-egui`: the egui integration that renders decoded video frames through
`moq-video-render` and provides the egui-side texture plumbing, so an egui
application can display a moq video track without writing any wgpu or texture
registration code itself. The crate carries the frame view widget (CPU texture
upload or wgpu-accelerated), the egui renderer that registers
`moq-video-render` output textures with egui and updates them per frame, and
the wgpu device configuration helper that builds an egui-wgpu setup carrying
the DMA-BUF extensions the zero-copy import path needs. It is the first UI
consumer of `moq-video-render` and completes the render story: moq-video-render
proves the `Native` vocabulary renders, and moq-egui proves the renderer
composes into a real UI toolkit. Like moq-video-render, it joins the workspace
as an off-default member so moq's default and relay builds never compile egui
or wgpu.

The dioxus integration (iroh-live's `moq-media-dioxus`) explicitly stays in
iroh-live for now; only the egui crate moves. The upstream overview already
records this split (`0-overview.md`, dependency tree and plan index), and the
branch registry row says the same (`../branches.md`: pair deletes
`moq-media-egui`, dioxus stays).

## Evidence

- `render/moq-video-render.md`: the leaf this crate builds on, including its
  public `VideoRenderer`, `RenderPath`, and the by-value `render(Frame)`
  signature this crate must adopt.
- `0-overview.md` dependency tree: `moq-egui` is the second render leaf,
  depending on moq-video-render, wave 3.
- Verified against the iroh-live working tree on 2026-07-22:
  `moq-media-egui/src/lib.rs` is 408 lines and `src/overlay.rs` is 1,080
  lines (1,488 total). `FrameView` at `lib.rs:58`, `VideoTrackView` at
  `lib.rs:186`, `EguiVideoRenderer` at `lib.rs:291`,
  `create_egui_wgpu_config` at `lib.rs:368` with the Linux DMA-BUF device
  path at `lib.rs:380`. `overlay_bar` at `overlay.rs:25`, `fit_to_aspect` at
  `overlay.rs:37`, `DebugOverlay` at `overlay.rs:79`.
- `moq-media-egui/Cargo.toml`: dependencies egui 0.33, epaint 0.33, and the
  optional egui-wgpu, wgpu 27, and pollster behind the `wgpu-render` feature;
  `dmabuf-import` and `metal-import` features forward to moq-media's render
  features. This feature shape ports over with the moq-media forwards
  rewritten to moq-video-render forwards.

## moq API consumed

- `moq-video-render`'s public API: `VideoRenderer::new(device, queue)`,
  `render(Frame)` returning a texture view, `render_cached`, `RenderPath`,
  and the `create_device_with_extensions` helper (the port of
  `create_device_with_dmabuf_extensions`). moq-egui names no moq-video
  internal type and never touches `Native` directly; the renderer owns the
  import paths.
- `moq_video::decode::Frame` (`rs/moq-video/src/decode/mod.rs:36-46`) as the
  frame input type, with `timestamp` and `size` for presentation bookkeeping,
  and `into_i420()` on the CPU-only path.
- Nothing from the frozen base contract beyond what moq-video-render already
  consumes; if the CPU-only path needs a borrowing pixel accessor that
  `into_i420(self)` cannot express, that is the same base-API question
  moq-video-render already carries (its "moq API consumed" section, options
  (a) and (b)) and is resolved there, not here.

## Source to port

From `moq-media-egui` (1,488 LOC total), the frame and texture plumbing moves;
the iroh-live-specific widgets stay:

- `FrameView` (`lib.rs:58-176`): renders frames into an egui texture, CPU
  upload or wgpu path, with `render_path_name` diagnostics, `texture_info`,
  and the `image()` accessor. Moves, retargeted from iroh-live's
  `VideoFrame` to `moq_video::decode::Frame`.
- `EguiVideoRenderer` (`lib.rs:291-360`): wraps the video renderer, registers
  its output texture with egui via `register_native_texture`, and updates it
  per frame through `update_egui_texture_from_wgpu_texture`. Moves,
  retargeted from `WgpuVideoRenderer` to `moq_video_render::VideoRenderer`.
- `create_egui_wgpu_config` (`lib.rs:368-408`): builds an
  `egui_wgpu::WgpuConfiguration` with a Vulkan device carrying the DMA-BUF
  extensions on Linux (via the moq-video-render device helper), default
  configuration elsewhere. Moves.
- `format_bitrate` (`lib.rs:36-44`) and the generic layout helpers
  `overlay_bar` (`overlay.rs:25-34`) and `fit_to_aspect`
  (`overlay.rs:37-45`): small, dependency-free egui helpers with no
  iroh-live type in their signatures. Move as a small helpers module.

Stays in iroh-live:

- `VideoTrackView` (`lib.rs:186-276`): wraps iroh-live's
  `moq_media::subscribe::VideoTrack` (latest-wins `try_recv` polling and
  `set_viewport`), which is an iroh-live type the align-to-moq campaign owns.
  It becomes a thin iroh-live-side wrapper over moq-egui's `FrameView`.
- `DebugOverlay` and `StatCategory` (`overlay.rs:79` onward, the bulk of the
  1,080-line file): driven by `moq_media::stats` types (`NetStats`,
  `PublishStats`, `SubscribeStats`, `Timeline`) that do not exist in moq.
  Stays in iroh-live with the stats types it renders.

## Target in moq

A new `moq-egui` crate under `rs/moq-egui`, wired exactly like
moq-video-render:

- Added to the `[workspace] members` array in moq's root `Cargo.toml`
  (`Cargo.toml:2-30`) and deliberately left out of `default-members`
  (`Cargo.toml:31-60`), following the members already excluded there
  (`libmoq`, `moq-ffi`, `moq-gst`, `moq-wasm`), so a plain workspace build
  compiles no egui or wgpu.
- Referenced by no other moq crate; consumers opt in explicitly. Neither
  `moq-video` nor `moq-relay` gains a dependency edge.
- Crate dependencies: `moq-video-render` (workspace), `moq-video`
  (workspace, for `decode::Frame`), and egui, epaint, egui-wgpu, wgpu, and
  pollster from crates.io. All are registry dependencies; nothing in the
  ported crate is git-pinned.
- Features, ported from the existing shape with the forwards rewritten:
  `wgpu-render = ["dep:egui-wgpu", "dep:wgpu", "dep:pollster", "moq-video-render/wgpu"]`
  (or the equivalent base feature moq-video-render lands with),
  `dmabuf-import = ["wgpu-render", "moq-video-render/dmabuf-import", "wgpu/vulkan"]`,
  and `metal-import = ["wgpu-render", "moq-video-render/metal-import", "wgpu/metal"]`.
  The heavy importer stacks stay behind non-default cascades per the
  dependency-weight constraint; whether `wgpu-render` is a default feature of
  this crate (as it is in `moq-media-egui` today) is acceptable either way
  because the crate itself is off the default workspace build, and the plan
  keeps the current default for parity.
- Module layout: `src/lib.rs` with `FrameView`, `EguiVideoRenderer`, and the
  device config helper, plus a small `helpers` module for `overlay_bar`,
  `fit_to_aspect`, and `format_bitrate`.

## Implementation steps

1. Create `rs/moq-egui` on the `up/moq-egui` branch cut from `up/base` with
   moq-video-render's branch merged in (this leaf cannot compile before that
   leaf exists), and add the workspace membership without touching
   `default-members`.
2. Port `FrameView` over `moq_video::decode::Frame`. The wgpu path hands the
   frame to `EguiVideoRenderer`; the CPU path converts I420 to RGBA for
   `egui::ColorImage` (iroh-live's `VideoFrame::rgba_image()` cache does this
   today; the port carries a small I420-to-RGBA conversion or reuses the one
   moq-video-render's CPU path exposes, whichever that crate lands with).
3. Port `EguiVideoRenderer` over `moq_video_render::VideoRenderer`, adopting
   the by-value `render(Frame)` signature that crate fixed, and keep the
   texture registration and per-frame update logic unchanged.
4. Port `create_egui_wgpu_config` over the renamed device helper, preserving
   the fallback to a default device when extension creation fails
   (`lib.rs:393-397`).
5. Port the `RenderPath` diagnostics mapping in `render_path_name`
   (`lib.rs:111-124`) onto moq-video-render's `RenderPath` variants
   (`CpuI420` replaces `CpuRgba`).
6. Move the generic helpers, write crate docs with a minimal eframe example,
   and add the tests below.

## Tests

- Hardware-free unit tests: `fit_to_aspect` geometry cases, `format_bitrate`
  formatting, and a `FrameView` CPU-path test that feeds a synthetic I420
  `decode::Frame` and asserts the egui texture takes the frame's dimensions.
- A compile-level feature matrix in CI: the crate builds with no features
  (CPU-only), with `wgpu-render`, and with each importer feature, on a
  GPU-less host. Actually exercising the zero-copy texture path needs a GPU
  and a windowing system, which moq CI lacks (coordination point 9); that is
  validated through the iroh-live device test runner and the eframe example
  run on developer hosts, and the PR text says so.

## Adaptation notes

- Frame vocabulary: every `moq_media::format::VideoFrame` becomes
  `moq_video::decode::Frame`; the crate holds no frame model of its own.
- Ownership: the existing `render_frame(&mut self, frame: &VideoFrame)`
  borrow becomes by-value `Frame` end to end, matching moq-video-render's
  decision; callers that need the timestamp read it before handing the frame
  off.
- The `pub use moq_media` re-export (`lib.rs:30`) becomes `pub use
  moq_video_render` plus `pub use egui_wgpu` under `wgpu-render`, so
  downstream apps can name the render state type without a direct
  dependency.
- Errors: the crate keeps a small error surface of its own (the current code
  uses `anyhow`; the port defines a thin `thiserror` enum consistent with
  moq-video-render's choice) and logs render failures rather than
  propagating them into the UI loop, as `FrameView::render_frame` does today
  (`lib.rs:130-132`).
- Conventional commits with the `moq-egui` scope.

## Counterpart

The paired iroh-live `up/moq-egui` branch (`../branches.md`): iroh-live
deletes the `moq-media-egui` crate and consumes `moq-egui` from the moq
workspace. `VideoTrackView` and the `DebugOverlay` stats widgets move into
iroh-live proper as thin wrappers over `moq-egui` (their consumers are
`iroh-live-cli` and the `split.rs` and `viewer.rs` examples), and
`moq-media-dioxus` stays in iroh-live unchanged. The cut follows
`cut-plan.md` and coordination point 12.

## Coordination

- Depends on the moq-video-render leaf; this plan does not finalize until
  that crate's public API (renderer type, `RenderPath`, device helper, and
  the by-value frame decision) is settled, and any gap it finds in that API
  is filed against the moq-video-render plan rather than worked around here.
- Base API freeze (coordination point 1) applies only transitively through
  moq-video-render; moq-egui itself consumes no B1 or B3 type directly.
- Coordination point 9: CI verifies compilation and the CPU path only; GPU
  texture paths are validated on the iroh-live device test runner.
- Coordination point 12 for the pair-side deletion.
- Open question: whether moq-egui should also ship a track-level widget over
  `moq_video::decode::Consumer` (subscribe a broadcast and display it, the
  moq-native analog of our `VideoTrackView`), discussed under Source to
  port; current proposal: ship the frame-level plumbing first and add a
  consumer-driven widget as a follow-up once a latest-wins polling shape
  over `Consumer::read` exists, since `Consumer` today is a plain async
  `read` with no non-blocking accessor
  (`rs/moq-video/src/decode/consumer.rs:59-74`).

## Acceptance checklist

- `rs/moq-egui` exists as a workspace member, excluded from
  `default-members`, referenced by no other moq crate; the default workspace
  build compiles no egui, egui-wgpu, wgpu, or pollster.
- The crate builds with no features, with `wgpu-render`, and with each
  importer feature on a GPU-less host; unit tests pass.
- `FrameView` and `EguiVideoRenderer` accept `moq_video::decode::Frame` and
  render through `moq_video_render::VideoRenderer` only; no moq-video
  internal type is named.
- `create_egui_wgpu_config` produces a working egui-wgpu setup with the
  DMA-BUF device on Linux and the default device elsewhere, with the
  failure fallback preserved.
- An eframe example in the crate displays a decoded track on a developer
  host, exercising the wgpu path.
- The paired iroh-live branch builds with `moq-media-egui` deleted,
  `iroh-live-cli` and the examples consuming `moq-egui` plus the relocated
  `VideoTrackView` and `DebugOverlay` wrappers, and `moq-media-dioxus`
  untouched.
