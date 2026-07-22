# Comparisons index and consolidated capability matrix

These documents establish, component by component, where iroh-live's owned media
stack (rusty-codecs, rusty-capture, moq-media) stands against current moq main
(HEAD `3a3e0ea8`, the `dev` line merged into `main` on 2026-07-21, so there is no
branch distinction). For every codec, capture backend, zero-copy path, and audio
device concern they name who is stronger and why, and they resolve each into one
of five verdicts: adopt theirs, keep ours, upstream ours, merge, or complementary.
The upstream campaign plans in [../base](../base), [../codec](../codec),
[../capture](../capture), and [../render](../render) act on these verdicts,
turning each "upstream ours" and "merge" into an ordered, independently executable
PR plan against moq. Read [../0-overview.md](../0-overview.md) first for the
campaign structure, the wave ordering, and the frozen base API contract that every
leaf plan codes against.

## Documents

| Document | Purpose |
|---|---|
| [codecs.md](codecs.md) | Codec-by-codec, backend-by-backend comparison (H.264, H.265, AV1, VP9, Opus, PCM) with a per-backend verdict table |
| [capture.md](capture.md) | Capture backend comparison across Linux, macOS, Windows, and Android, plus capture-side zero-copy and audio capture |
| [audio.md](audio.md) | Audio stack: Opus wrapper, resampling, device I/O, AEC, file sources, and the A/V sync touchpoint |
| [zerocopy.md](zerocopy.md) | Zero-copy deep dive: frame and surface models, the four GPU paths, the vendor-API coverage matrix, and the render-crate placement decision |
| [traits-api.md](traits-api.md) | Trait, interface, and Rust-API contrast: open traits versus closed `pub(crate)` backends, exhaustive signature tables, and the D1 through D12 decision list |
| [moq-changes.md](moq-changes.md) | The concrete moq-side API changes to make everything upstreamable, with the sequenced change list and the in-tree versus out-of-tree recommendation per backend |
| [iroh-live-code-map.md](iroh-live-code-map.md) | Code map of what iroh-live owns, per-crate LOC inventory, and ownership totals |
| [moq-inventory.md](moq-inventory.md) | Inventory of what moq main offers today, crate by crate, with the capability summary table |

The per-crate code maps that back these comparisons live in [maps/](maps): moq-net
origin, moq-video, moq-audio and moq-nvenc, moq-transcode and moq-stats, moq-media,
the room layer, rusty-capture, and rusty-codecs.

## Consolidated capability matrix

Rows are every codec by direction, then the capture backends, the zero-copy paths,
and the audio device layer. Columns are the two stacks, the verdict, and the plan
that acts on it. Cells link to the detailed section that backs the verdict.

### Codecs

| Component | iroh-live | moq main | Verdict | Plan |
|---|---|---|---|---|
| H.264 encode | openh264, VAAPI, V4L2, VideoToolbox, Android ([details](codecs.md#1-h264)) | openh264, VAAPI stub, VT, MF, NVENC | merge | [vaapi-encode](../codec/vaapi-encode.md), [v4l2-encode](../codec/v4l2-encode.md), [android](../codec/android-mediacodec.md) |
| H.264 decode | openh264, VAAPI, V4L2, VideoToolbox, Android ([details](codecs.md#1-h264)) | openh264, VT, MF, NVDEC ([no VAAPI](codecs.md#vaapi-vs-vaapi--moq-vaapi)) | merge, [upstream ours](codecs.md#vaapi-vs-vaapi--moq-vaapi) | [vaapi-decode](../codec/vaapi-decode.md), [v4l2-decode](../codec/v4l2-decode.md), [android](../codec/android-mediacodec.md) |
| H.265 encode | none | VT, MF, NVENC ([details](codecs.md#2-h265)) | adopt-theirs | none |
| H.265 decode | none | VT, MF, NVDEC ([details](codecs.md#2-h265)) | adopt-theirs | none |
| VP9 encode | none | none ([catalog-only](codecs.md#4-vp9)) | n/a | none |
| VP9 decode | none | none ([catalog-only](codecs.md#4-vp9)) | n/a | none |
| AV1 encode | rav1e software ([only one, either stack](codecs.md#3-av1)) | none | [upstream ours](codecs.md#3-av1) | [av1-software](../codec/av1-software.md) |
| AV1 decode | rav1d software (universal fallback) | NVDEC 8-bit only ([details](codecs.md#3-av1)) | [upstream ours](codecs.md#3-av1) | [av1-software](../codec/av1-software.md) |
| Opus encode | libopus, runtime bitrate, pre-skip ([details](codecs.md#5-opus)) | libopus, rate snap, validation | [merge](audio.md#15-verdict-on-the-codec-layer) | [opus-improvements](../codec/opus-improvements.md) |
| Opus decode | libopus, decoder remix ([details](audio.md#12-opus-decoder-feature-by-feature)) | libopus ([no PLC either side](audio.md#15-verdict-on-the-codec-layer)) | [merge](audio.md#15-verdict-on-the-codec-layer) | [opus-improvements](../codec/opus-improvements.md) |
| PCM encode | raw f32 ([ours only](codecs.md#6-pcm)) | none | keep-ours | [pcm](../codec/pcm.md) |
| PCM decode | raw f32 ([ours only](audio.md#14-pcm-codec-ours-only)) | none | keep-ours | [pcm](../codec/pcm.md) |

### Capture backends

| Component | iroh-live | moq main | Verdict | Plan |
|---|---|---|---|---|
| PipeWire (Linux screen) | DMA-BUF into VAAPI ([details](capture.md#linux-screen-pipewire)) | CPU-only ([#2238](capture.md#linux-screen-pipewire)) | keep-ours, [upstream ours](zerocopy.md#2a-capture-to-encode-verdict-complementary) | [pipewire-dmabuf](../capture/pipewire-dmabuf.md) |
| V4L2 (Linux camera) | enumeration, NV12 passthrough ([details](capture.md#linux-camera-v4l2)) | none | keep-ours, adopt MJPEG decode | [v4l2-camera-enum](../capture/v4l2-camera-enum.md) |
| X11 | fallback ([ours only](capture.md#backends-only-we-have)) | none | keep-ours | none |
| libcamera (raw) | ours only ([details](capture.md#backends-only-we-have)) | none | keep-ours | none |
| libcamera H.264 (pre-encoded) | rpicam-vid, strongest candidate ([details](capture.md#backends-only-we-have)) | none | [upstream ours](capture.md#5-verdict) | [libcamera-preencoded](../capture/libcamera-preencoded.md) |
| ScreenCaptureKit (macOS screen) | functional subset plus BGRA ([details](capture.md#macos-screen-screencapturekit)) | app capture, NV12 surfaces | [adopt-theirs](capture.md#macos-screen-screencapturekit) | none |
| AVFoundation (macOS camera) | stub ([details](capture.md#macos-camera)) | complete, zero-copy, TCC | [adopt-theirs](capture.md#macos-camera) | none |
| Windows (camera, screen) | doc stubs ([details](capture.md#windows)) | MF, Desktop Duplication, GPU-resident | [adopt-theirs](capture.md#windows) | none |
| Android | HardwareBuffer plan ([ours only](capture.md#5-verdict)) | none | keep-ours | [android](../codec/android-mediacodec.md) |
| nokhwa | fallback camera ([details](capture.md#backends-only-we-have)) | none | keep-ours (fallback) | none |
| xcap | fallback screen ([details](capture.md#backends-only-we-have)) | none | keep-ours (fallback) | none |

### Zero-copy paths

| Component | iroh-live | moq main | Verdict | Plan |
|---|---|---|---|---|
| Capture to encode | Linux DMA-BUF, macOS CVPixelBuffer ([details](zerocopy.md#2a-capture-to-encode-verdict-complementary)) | macOS, Windows surfaces into encoder | [complementary](zerocopy.md#2a-capture-to-encode-verdict-complementary) | [pipewire-dmabuf](../capture/pipewire-dmabuf.md), [B1](../base/B1-frame-vocabulary.md) |
| Decode to render | only such path, 3 platforms, 2 graphics APIs ([details](zerocopy.md#2b-decode-to-render-verdict-ours)) | downloads to I420 except NVDEC | [upstream ours](zerocopy.md#2b-decode-to-render-verdict-ours) | [vtb-mf-decode-surface](../codec/vtb-mf-decode-surface.md), [B3](../base/B3-decode-native-accessor.md) |
| Transcode (decode, scale, encode) | none ([details](zerocopy.md#2c-transcode-decode-then-scale-then-encode-verdict-theirs)) | NVDEC to NVENC GPU loop, decode-once fanout | [complementary](zerocopy.md#2c-transcode-decode-then-scale-then-encode-verdict-theirs) | none |
| Render | only renderer, Vulkan, EGL, Metal ([details](zerocopy.md#2d-render-itself-verdict-ours)) | none | keep-ours, [out-of-tree](zerocopy.md#4-render-upstreaming-decision) | [moq-video-render](../render/moq-video-render.md) |

### Audio device layer

| Component | iroh-live | moq main | Verdict | Plan |
|---|---|---|---|---|
| Playback / sink | full duplex engine, mixing, metering ([details](audio.md#3-device-io)) | [no playback surface](audio.md#7-verdict) | keep-ours | none |
| AEC | sonora acoustic echo cancellation ([details](audio.md#3-device-io)) | [none](capture.md#4-audio-capture) | keep-ours | none |
| Resample | higher quality, per-call alloc ([details](audio.md#2-resampling)) | leaner sinc-128, preallocated | [merge](audio.md#2-resampling) | none |
| System-audio capture | none ([the missing half](audio.md#34-the-missing-half)) | SCK loopback, TCC, `format()` ([details](audio.md#32-what-their-capture-does-and-does-better)) | [adopt-theirs](audio.md#7-verdict) | none |

## Key findings

- The codec seam is an open-versus-closed divide: rusty-codecs exposes public
  traits and factories, while moq keeps its `Backend` trait `pub(crate)` behind a
  concrete-struct facade ([traits-api.md](traits-api.md#1-design-philosophy-contrast)).
  Opening it is the enabling ask carried by
  [B4](../base/B4-backend-trait-registration.md), needed only for the backends that
  stay in our tree.
- The two zero-copy investments are complementary rather than competing, and
  decode to render is ours across three platforms and two graphics APIs, the only
  such path in either codebase ([zerocopy.md](zerocopy.md#6-verdict); the path
  detail is [decode to render, verdict ours](zerocopy.md#2b-decode-to-render-verdict-ours)).
- VAAPI is the most lopsided comparison in our favor: a validated zero-copy
  DMA-BUF encoder plus a full stateless decode against moq's 111-line unvalidated
  CPU-only encoder placeholder and no VAAPI decode at all
  ([codecs.md](codecs.md#vaapi-vs-vaapi--moq-vaapi)).
- The renderer becomes an out-of-tree `moq-video-render` crate over moq's public
  frame handles (Option B), which keeps moq-video render-free and the Intel
  Y_TILED re-tile where it is tested on hardware
  ([zerocopy.md](zerocopy.md#4-render-upstreaming-decision),
  [moq-changes.md](moq-changes.md#recommendation-per-backend),
  [render plan](../render/moq-video-render.md)).
- The moq-side keystone is a public GPU-frame vocabulary with a native-handle
  accessor (change 1 in the sequenced list); every GPU contribution rests on it
  ([moq-changes.md](moq-changes.md#section-5-sequenced-change-list),
  [B1](../base/B1-frame-vocabulary.md)).
- The Opus PLC story is open on both sides: neither stack actually performs PLC or
  FEC decode today, so a merged wrapper must add a `decode_lost` entry point that
  neither has ([audio.md](audio.md#15-verdict-on-the-codec-layer),
  [opus-improvements](../codec/opus-improvements.md)).
- moq-transcode is the complementary supply side of ABR: it mints a rendition
  ladder on demand but contains zero selection logic, which is exactly the gap
  iroh-live's adaptive path fills
  ([moq-inventory.md](moq-inventory.md#moq-transcode)).

See [../0-overview.md](../0-overview.md) for the campaign structure, the base and
leaf wave ordering, the coordination points, and the frozen base API contract that
every plan in [../base](../base), [../codec](../codec), [../capture](../capture),
and [../render](../render) codes against.
