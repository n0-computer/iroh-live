# Branch registry: the upstream campaign's paired branches

> Campaign: upstream | Kind: reference | Maintained by the coordinator; update
> when a branch is created, merged, or abandoned. Read `0-overview.md` for the
> paired-branch model this registry follows.

Two repositories: `iroh-live` (this one) and `moq` (`/home/bit/Code/rust/moq`,
plus the external `moq-dev/vaapi` repo for the VA layer). This campaign uses
**paired branches**: a moq branch that adds the contribution and an iroh-live
branch with the same name that depends on it (path dep during development, git
branch dep at handoff) and cuts the code the contribution replaces. Some leaves
add without a cut; those have no iroh-live pair beyond the dep bump. Plan paths
below are relative to this `upstream/` directory.

## Paired `up/<name>` branches

| moq branch | iroh-live pair | Plan | Cut guidance (pair side) |
|---|---|---|---|
| `up/base` | `up/base` | `base/B1..B3,B5` | none; pair pins the base API for leaves |
| `up/backend-trait` | - | `base/B4-backend-trait-registration.md` | conditional (Path B / Android only) |
| `up/vaapi-decode` (also `moq-dev/vaapi` PR) | `up/vaapi-decode` | `codec/vaapi-decode.md` | `counterpart/codec-remove.md` (VAAPI rows) |
| `up/vaapi-encode` (also `moq-dev/vaapi` PR) | `up/vaapi-encode` | `codec/vaapi-encode.md` | `counterpart/codec-remove.md` (VAAPI rows) |
| `up/vtb-mf-decode-surface` | `up/vtb-mf-decode-surface` | `codec/vtb-mf-decode-surface.md` | `counterpart/codec-remove.md` (VTB rows) |
| `up/v4l2-encode` | `up/v4l2-encode` | `codec/v4l2-encode.md` | `counterpart/codec-remove.md` (V4L2 rows) |
| `up/v4l2-decode` | `up/v4l2-decode` | `codec/v4l2-decode.md` | `counterpart/codec-remove.md` (V4L2 rows) |
| `up/android-mediacodec` | `up/android-mediacodec` | `codec/android-mediacodec.md` | `counterpart/codec-remove.md` (Android rows) |
| `up/bitstream-sps-vui` | - | `codec/bitstream-sps-vui.md` | improvement only, no cut |
| `up/av1-software` | - | `codec/av1-software.md` | DEFERRED; local rip-out tracked in `counterpart/codec-remove.md` |
| `up/opus-improvements` | - | `audio/opus-improvements.md` | improvement only, no cut |
| `up/pcm` | `up/pcm` | `audio/pcm.md` | PCM catalog variant lands in the same branch (hang) |
| `up/audio-device` | `up/audio-device` | `audio/audio-device-unify.md` | pair deletes `audio_backend`/AEC once released |
| `up/pipewire-dmabuf` | `up/pipewire-dmabuf` | `capture/pipewire-dmabuf.md` | `counterpart/capture-remove.md` (PipeWire rows) |
| `up/v4l2-camera-enum` | `up/v4l2-camera-enum` | `capture/v4l2-camera-enum.md` | `counterpart/capture-remove.md` (V4L2 rows) |
| `up/libcamera-preencoded` | `up/libcamera-preencoded` | `capture/libcamera-preencoded.md` | `counterpart/capture-remove.md` (libcamera rows) |
| `up/parity-ports` | - | `capture/parity-ports.md` | ports our fixes into adopt-theirs backends; precedes their cuts in `counterpart/` |
| `up/video-render` | `up/video-render` | `render/moq-video-render.md` | `counterpart/render-adopt.md` |
| `up/moq-egui` | `up/moq-egui` | `render/moq-egui.md` | pair deletes `moq-media-egui` (dioxus stays) |

The deletion ledger for all pair-side cuts is `cut-plan.md`. The standing
disposition of every iroh-live media module (upstream-ours / adopt-theirs /
keep / defer / drop) is `DISPOSITION.md`.

## Status

No `up/*` branch exists yet. When one is created, note it here and in the
campaign's `status.md`.
