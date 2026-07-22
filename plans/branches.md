# Branch tree: every branch in both repositories

> Campaign: both | Kind: reference | Maintained by the coordinator; update when a
> branch is created, merged, or abandoned. Read `prompt-base.md` for the branch
> and worktree rules this tree follows.

Two repositories: `iroh-live` (this one) and `moq` (`/home/bit/Code/rust/moq`,
plus the external `moq-dev/vaapi` repo for the VA layer). Upstream work uses
**paired branches**: a moq branch that adds the contribution and an iroh-live
branch with the same name that depends on it (path dep during development, git
branch dep at handoff) and cuts the code the contribution replaces. Some leaves
add without a cut; those have no iroh-live pair beyond the dep bump. Align work
is iroh-live-only.

## Upstream campaign (paired `up/<name>` branches)

| moq branch | iroh-live pair | Plan | Cut guidance (pair side) |
|---|---|---|---|
| `up/base` | `up/base` | `upstream/base/B1..B3,B5` | none; pair pins the base API for leaves |
| `up/backend-trait` | - | `upstream/base/B4-backend-trait-registration.md` | conditional (Path B / Android only) |
| `up/vaapi-decode` (also `moq-dev/vaapi` PR) | `up/vaapi-decode` | `upstream/codec/vaapi-decode.md` | `upstream/counterpart/codec-remove.md` (VAAPI rows) |
| `up/vaapi-encode` (also `moq-dev/vaapi` PR) | `up/vaapi-encode` | `upstream/codec/vaapi-encode.md` | `upstream/counterpart/codec-remove.md` (VAAPI rows) |
| `up/vtb-mf-decode-surface` | `up/vtb-mf-decode-surface` | `upstream/codec/vtb-mf-decode-surface.md` | `upstream/counterpart/codec-remove.md` (VTB rows) |
| `up/v4l2-encode` | `up/v4l2-encode` | `upstream/codec/v4l2-encode.md` | `upstream/counterpart/codec-remove.md` (V4L2 rows) |
| `up/v4l2-decode` | `up/v4l2-decode` | `upstream/codec/v4l2-decode.md` | `upstream/counterpart/codec-remove.md` (V4L2 rows) |
| `up/android-mediacodec` | `up/android-mediacodec` | `upstream/codec/android-mediacodec.md` | `upstream/counterpart/codec-remove.md` (Android rows) |
| `up/bitstream-sps-vui` | - | `upstream/codec/bitstream-sps-vui.md` | improvement only, no cut |
| `up/av1-software` | - | `upstream/codec/av1-software.md` | DEFERRED; local rip-out tracked in `upstream/counterpart/codec-remove.md` |
| `up/opus-improvements` | - | `upstream/audio/opus-improvements.md` | improvement only, no cut |
| `up/pcm` | `up/pcm` | `upstream/audio/pcm.md` | PCM catalog variant lands in the same branch (hang) |
| `up/audio-device` | `up/audio-device` | `upstream/audio/audio-device-unify.md` | pair deletes `audio_backend`/AEC once released |
| `up/pipewire-dmabuf` | `up/pipewire-dmabuf` | `upstream/capture/pipewire-dmabuf.md` | `upstream/counterpart/capture-remove.md` (PipeWire rows) |
| `up/v4l2-camera-enum` | `up/v4l2-camera-enum` | `upstream/capture/v4l2-camera-enum.md` | `upstream/counterpart/capture-remove.md` (V4L2 rows) |
| `up/libcamera-preencoded` | `up/libcamera-preencoded` | `upstream/capture/libcamera-preencoded.md` | `upstream/counterpart/capture-remove.md` (libcamera rows) |
| `up/parity-ports` | - | `upstream/capture/parity-ports.md` | ports our fixes into adopt-theirs backends; precedes their cuts in `upstream/counterpart/` |
| `up/video-render` | `up/video-render` | `upstream/render/moq-video-render.md` | `upstream/counterpart/render-adopt.md` |
| `up/moq-egui` | `up/moq-egui` | `upstream/render/moq-egui.md` | pair deletes `moq-media-egui` (dioxus stays) |

The deletion ledger for all pair-side cuts is `upstream/cut-plan.md`. The
standing disposition of every iroh-live media module (upstream / adopt-theirs /
keep / defer / drop) is `upstream/DISPOSITION.md`.

## Align campaign (iroh-live-only `align/<task>` branches)

| iroh-live branch | Plan |
|---|---|
| `align/pin-bump` | `align-to-moq/0-overview.md` (Wave 0) |
| `align/transport-delegate` | `align-to-moq/tasks/transport-delegate.md` |
| `align/catalog-adopt` | `align-to-moq/tasks/catalog-adopt.md` |
| `align/pubsub-align` | `align-to-moq/tasks/pubsub-align.md` |
| `align/rooms-announce` | `align-to-moq/tasks/rooms-announce.md` |
| `align/sync-adaptive-align` | `align-to-moq/tasks/sync-adaptive-align.md` |

## Planning branch

| branch | content |
|---|---|
| `plan-upstream` (iroh-live) | this `plans/` tree |

## Status

No `up/*` or `align/*` branch exists yet. When one is created, note it here and
in the campaign's `status.md`.
