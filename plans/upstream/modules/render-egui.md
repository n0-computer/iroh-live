# Egui video integration (moq-egui)

VERDICT: move from iroh-live to moq

> Media-stack upstream module. Read ../overview.md first; the decisive context
> is ../zerocopy.md. The consolidated comparison and verdict table is
> ../comparison.md.
>
> Zero-copy: this module consumes moq-video-render's already-imported GPU
> textures, so it inherits the decode-to-render zero-copy path rather than
> creating one. See ../zerocopy.md and render-video-render.md.

## What it is

iroh-live's `moq-media-egui` crate is an egui integration that displays decoded
video frames: a frame view widget, an egui renderer that registers a video
texture with egui and updates it per frame, and a wgpu device configuration
helper that builds an egui-wgpu setup carrying the DMA-BUF extensions the
zero-copy import path needs. Its generic parts move into the moq workspace as a
new crate `moq-egui` over `moq-video-render`, so an egui application can display
a moq video track without writing any wgpu or texture registration code itself.
moq has no egui or UI integration at all, so the verdict is "move from iroh-live
to moq": this is a contribution, not a convergence. It joins as an off-default
workspace member (like `moq-wasm` and `moq-gst`) so moq's default and relay
builds never compile egui or wgpu.

## iroh-live vs moq

iroh-live has the full egui plumbing. `FrameView` (`moq-media-egui/src/lib.rs:58`)
renders frames into an egui texture over either a CPU upload or the wgpu path,
with `render_path_name` diagnostics, `texture_info`, and an `image()` accessor.
`EguiVideoRenderer` (`lib.rs:291`) wraps the video renderer, registers its output
texture with egui via `register_native_texture`, and updates it per frame through
`update_egui_texture_from_wgpu_texture`. `create_egui_wgpu_config` (`lib.rs:368`,
Linux DMA-BUF device path at `lib.rs:380`) builds an `egui_wgpu::WgpuConfiguration`
with a Vulkan device carrying the DMA-BUF extensions on Linux and a default device
elsewhere. moq has no counterpart. It is the first UI consumer of moq-video-render
and completes the render story: moq-video-render proves the `Native` vocabulary
renders, and moq-egui proves the renderer composes into a real UI toolkit.

## Zero-copy

This module is on the decode-to-render zero-copy path only by inheritance. It
consumes moq-video-render's already-imported GPU textures: `EguiVideoRenderer`
hands a `decode::Frame` to `moq_video_render::VideoRenderer`, which owns all the
import paths (Vulkan DMA-BUF, EGL, Metal) and returns a texture already resident
on the GPU, and moq-egui registers that texture with egui and updates it per
frame. moq-egui names no `Native` variant and never touches an importer directly;
it creates no zero-copy path of its own. Its one zero-copy-relevant contribution
is `create_egui_wgpu_config`, which builds the wgpu device with the DMA-BUF
extensions so the renderer beneath it can take the import path rather than falling
to CPU download, preserving the fallback to a default device when extension
creation fails (`lib.rs:393-397`). See render-video-render.md for the path itself
and ../zerocopy.md for the decode-to-render analysis.

## What to do

MOVE the frame and texture plumbing into a new moq workspace crate; leave the
iroh-live-specific widgets in iroh-live.

What moves (target crate `rs/moq-egui`, an off-default workspace member):

- `FrameView` (`lib.rs:58-176`): renders frames into an egui texture, CPU upload
  or wgpu path, with `render_path_name` diagnostics, `texture_info`, and the
  `image()` accessor. Moves, retargeted from iroh-live's `VideoFrame` to
  `moq_video::decode::Frame`.
- `EguiVideoRenderer` (`lib.rs:291-360`): wraps the video renderer, registers its
  output texture with egui via `register_native_texture`, and updates it per
  frame through `update_egui_texture_from_wgpu_texture`. Moves, retargeted from
  `WgpuVideoRenderer` to `moq_video_render::VideoRenderer`, adopting the by-value
  `render(Frame)` signature that crate fixed.
- `create_egui_wgpu_config` (`lib.rs:368-408`): builds an
  `egui_wgpu::WgpuConfiguration` with a Vulkan device carrying the DMA-BUF
  extensions on Linux via the moq-video-render device helper (the port of
  `create_device_with_dmabuf_extensions`), default configuration elsewhere.
  Moves, preserving the fallback to a default device when extension creation
  fails (`lib.rs:393-397`).
- `format_bitrate` (`lib.rs:36-44`) and the generic layout helpers `overlay_bar`
  (`overlay.rs:25-34`) and `fit_to_aspect` (`overlay.rs:37-45`): small,
  dependency-free egui helpers with no iroh-live type in their signatures. Move
  as a small `helpers` module.

What stays in iroh-live:

- `VideoTrackView` (`lib.rs:186-276`): wraps iroh-live's
  `moq_media::subscribe::VideoTrack` (latest-wins `try_recv` polling and
  `set_viewport`), an iroh-live type the align-to-moq campaign owns. It becomes a
  thin iroh-live-side wrapper over moq-egui's `FrameView`.
- `DebugOverlay` and `StatCategory` (`overlay.rs:79` onward, the bulk of the
  1,080-line file): driven by `moq_media::stats` types (`NetStats`,
  `PublishStats`, `SubscribeStats`, `Timeline`) that do not exist in moq. Stays
  in iroh-live with the stats types it renders.

The dioxus integration (iroh-live's `moq-media-dioxus`) explicitly stays in
iroh-live for now; only the egui crate moves.

Cargo and workspace wiring:

- Added to the `[workspace] members` array in moq's root `Cargo.toml`
  (`Cargo.toml:2-30`) and deliberately left out of `default-members`
  (`Cargo.toml:31-60`), following the members already excluded there (`libmoq`,
  `moq-ffi`, `moq-gst`, `moq-wasm`), so a plain workspace build compiles no egui
  or wgpu.
- Referenced by no other moq crate; consumers opt in explicitly. Neither
  `moq-video` nor `moq-relay` gains a dependency edge.
- Dependencies: `moq-video-render` (workspace), `moq-video` (workspace, for
  `decode::Frame`), and egui, epaint, egui-wgpu, wgpu, and pollster from
  crates.io. All are registry dependencies; nothing git-pinned.
- Features, ported from the existing shape with the moq-media forwards rewritten
  to moq-video-render forwards:
  `wgpu-render = ["dep:egui-wgpu", "dep:wgpu", "dep:pollster", "moq-video-render/wgpu"]`,
  `dmabuf-import = ["wgpu-render", "moq-video-render/dmabuf-import", "wgpu/vulkan"]`,
  and `metal-import = ["wgpu-render", "moq-video-render/metal-import", "wgpu/metal"]`.
  The heavy importer stacks stay behind non-default cascades. Whether
  `wgpu-render` is a default feature of this crate (as it is in `moq-media-egui`
  today) is acceptable either way because the crate itself is off the default
  workspace build; the plan keeps the current default for parity.
- Module layout: `src/lib.rs` with `FrameView`, `EguiVideoRenderer`, and the
  device config helper, plus a small `helpers` module.

Adaptation to moq's vocabulary:

- Frame vocabulary: every `moq_media::format::VideoFrame` becomes
  `moq_video::decode::Frame`; the crate holds no frame model of its own.
- Ownership: the existing `render_frame(&mut self, frame: &VideoFrame)` borrow
  becomes by-value `Frame` end to end, matching moq-video-render's decision;
  callers that need the timestamp read it before handing the frame off.
- The `RenderPath` diagnostics mapping in `render_path_name` (`lib.rs:111-124`)
  maps onto moq-video-render's `RenderPath` variants (`CpuI420` replaces
  `CpuRgba`).
- The `pub use moq_media` re-export (`lib.rs:30`) becomes `pub use
  moq_video_render` plus `pub use egui_wgpu` under `wgpu-render`.
- Errors: the crate keeps a small error surface of its own (the current code uses
  `anyhow`; the port defines a thin `thiserror` enum consistent with
  moq-video-render's choice) and logs render failures rather than propagating
  them into the UI loop, as `FrameView::render_frame` does today (`lib.rs:130-132`).

iroh-live removal (the paired branch): iroh-live deletes the `moq-media-egui`
crate and consumes `moq-egui` from the moq workspace. `VideoTrackView` and the
`DebugOverlay` stats widgets move into iroh-live proper as thin wrappers over
`moq-egui` (their consumers are `iroh-live-cli` and the `split.rs` and
`viewer.rs` examples), and `moq-media-dioxus` stays unchanged.

## Tests

- Hardware-free unit tests: `fit_to_aspect` geometry cases, `format_bitrate`
  formatting, and a `FrameView` CPU-path test that feeds a synthetic I420
  `decode::Frame` and asserts the egui texture takes the frame's dimensions.
- A compile-level feature matrix in CI: the crate builds with no features
  (CPU-only), with `wgpu-render`, and with each importer feature, on a GPU-less
  host.
- Actually exercising the zero-copy texture path needs a GPU and a windowing
  system that moq CI lacks; that is validated through the iroh-live device test
  runner and an eframe example run on developer hosts, and the PR text says so.

## Evidence

- ../overview.md dependency tree: `moq-egui` is the second render leaf, depending
  on moq-video-render.
- render-video-render.md: the sibling module this crate builds on, including its
  public `VideoRenderer`, `RenderPath`, and the by-value `render(Frame)`
  signature this crate adopts.
- Verified against the iroh-live working tree on 2026-07-22:
  `moq-media-egui/src/lib.rs` is 408 lines and `src/overlay.rs` is 1,080 lines
  (1,488 total). `FrameView` at `lib.rs:58`, `VideoTrackView` at `lib.rs:186`,
  `EguiVideoRenderer` at `lib.rs:291`, `create_egui_wgpu_config` at `lib.rs:368`
  with the Linux DMA-BUF device path at `lib.rs:380`. `overlay_bar` at
  `overlay.rs:25`, `fit_to_aspect` at `overlay.rs:37`, `DebugOverlay` at
  `overlay.rs:79`.
- `moq-media-egui/Cargo.toml`: dependencies egui 0.33, epaint 0.33, and the
  optional egui-wgpu, wgpu 27, and pollster behind the `wgpu-render` feature;
  `dmabuf-import` and `metal-import` forward to moq-media's render features. This
  feature shape ports over with the moq-media forwards rewritten to
  moq-video-render forwards.

## Coordination

- Depends on render-video-render.md. This plan does not finalize until that
  crate's public API (renderer type, `RenderPath`, device helper, and the
  by-value frame decision) is settled, and any gap it finds in that API is filed
  against the moq-video-render module rather than worked around here. The branch
  is cut from `up/base` with moq-video-render's branch merged in, because this
  leaf cannot compile before that crate exists.
- Base API freeze (coordination point 1) applies only transitively through
  moq-video-render; moq-egui consumes no B1 or B3 type directly. If the CPU-only
  path needs a borrowing pixel accessor that `into_i420(self)` cannot express,
  that is the same base-API question moq-video-render carries (its frame-parameter
  options (a) and (b)) and is resolved there.
- CI hardware gating (coordination point 9): CI verifies compilation and the CPU
  path only; GPU texture paths are validated on the iroh-live device test runner.
- Release gate (coordination point 12) for the pair-side deletion.
- Open question: whether moq-egui should also ship a track-level widget over
  `moq_video::decode::Consumer` (subscribe a broadcast and display it, the
  moq-native analog of `VideoTrackView`); current proposal: ship the frame-level
  plumbing first and add a consumer-driven widget as a follow-up once a
  latest-wins polling shape over `Consumer::read` exists, since `Consumer` today
  is a plain async `read` with no non-blocking accessor
  (`rs/moq-video/src/decode/consumer.rs:59-74`).
