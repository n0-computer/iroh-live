# Branch registry

> Campaign: upstream (media stack) | Kind: reference | Read `../prompt.md` for
> the paired-branch model this registry follows.

Two repositories: iroh-live (this one) and moq (`/home/bit/Code/rust/moq`, plus
the external `moq-dev/vaapi` repo for the VA layer). Each contribution lives on a
moq branch `up/<name>`; where the module has a cut side, a same-named iroh-live
branch depends on it and deletes the local code. Modules that only improve moq
have no iroh-live pair beyond the dep bump.

Branch name per module is `up/<module basename without the category prefix>`,
except where noted. The base plans share `up/base` (B4 is a separate
`up/backend-trait`).

| Branch | Module | iroh-live pair | Notes |
|---|---|---|---|
| `up/base` | `base/B1..B3,B5` | `up/base` | the base API; pair pins it for the rest |
| `up/backend-trait` | `base/B4-backend-trait-registration.md` | - | conditional (Android / Path B only) |
| `up/vaapi-encode` | `modules/codec-vaapi-encode.md` | `up/vaapi-encode` | also a `moq-dev/vaapi` PR |
| `up/vaapi-decode` | `modules/codec-vaapi-decode.md` | `up/vaapi-decode` | also a `moq-dev/vaapi` PR |
| `up/v4l2-encode` | `modules/codec-v4l2-encode.md` | `up/v4l2-encode` | |
| `up/v4l2-decode` | `modules/codec-v4l2-decode.md` | `up/v4l2-decode` | |
| `up/android-mediacodec` | `modules/codec-android-mediacodec.md` | `up/android-mediacodec` | needs B4 |
| `up/decode-surface` | `modules/codec-decode-surface.md` | `up/decode-surface` | |
| `up/opus` | `modules/codec-opus.md` | `up/opus` | |
| `up/pcm` | `modules/codec-pcm.md` | `up/pcm` | hang catalog variant in the same branch |
| `up/bitstream-sps-vui` | `modules/codec-bitstream-sps-vui.md` | - | improvement only; local sps.rs deleted at M0 |
| `up/av1-software` | `modules/codec-av1.md` | - | deferred upstream; local rip-out at M0 |
| `up/audio-device` | `modules/audio-backend.md` | `up/audio-device` | pair deletes audio_backend/AEC |
| `up/pipewire-dmabuf` | `modules/capture-pipewire.md` | `up/pipewire-dmabuf` | |
| `up/v4l2-camera-enum` | `modules/capture-v4l2.md` | `up/v4l2-camera-enum` | |
| `up/libcamera-preencoded` | `modules/capture-libcamera.md` | `up/libcamera-preencoded` | publish_preencoded is an open question |
| `up/x11-capture` | `modules/capture-x11.md` | `up/x11-capture` | leaf pending |
| `up/parity-ports` | `parity-ports.md` | - | ports our fixes into the adopt-theirs backends |
| `up/video-render` | `modules/render-video-render.md` | `up/video-render` | the render crate |
| `up/moq-egui` | `modules/render-egui.md` | `up/moq-egui` | moq-media-egui moves; dioxus stays |

The adopt-theirs modules (openh264, videotoolbox-encode, dispatch, annexb,
config-mirror, macos-screen, macos-camera, windows, nokhwa, xcap, resample) and
`capture-android` have no moq contribution branch; their cuts ride the
release-bump commit on iroh-live per the staging in `staging-and-risks.md`.

No `up/*` branch exists yet. Note each here as it is created.
