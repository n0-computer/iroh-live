# The master comparison and verdict table

> Campaign: upstream (media stack) | Kind: the master index | Read `overview.md`
> first; the decisive detail is `zerocopy.md`. This table consolidates the
> capability comparison and the per-module disposition; each row links to its
> module doc under `modules/` and to the evidence under `comparison/`.

Every iroh-live media module has exactly one row and exactly one verdict; nothing
is dropped silently. The verdict is the first line of the linked module doc.

- **move** - moq lacks it; we contribute ours.
- **apply** - moq has it; we contribute our improvements, then use moq's.
- **use moq** - moq's implementation wins; we adopt it and remove ours.
- **remove** - deferred; dropped locally, no moq contribution this series.

The Zero-copy column flags whether the module is on a zero-copy path
(`zerocopy.md`); those modules are the campaign's decisive value and the reason
the base frame vocabulary (`base/B1-frame-vocabulary.md`) and the renderer land
first.

## Base layer (the enabling moq API changes)

Every zero-copy module rests on these; they land first. Detail in `base/`.

| Plan | Delivers | Zero-copy role |
|---|---|---|
| [B1 frame-vocabulary](base/B1-frame-vocabulary.md) | public `Native` handle enum + `DmaBuf` accessor + cfg-gated `Frame` variants | the keystone: names the GPU handles every zero-copy path passes |
| [B2 pts-through-encode](base/B2-pts-through-encode.md) | timestamp arg + `Packet` on `Backend::encode` | lets pipelined encoders (V4L2, Android) not mis-stamp |
| [B3 decode-native-accessor](base/B3-decode-native-accessor.md) | `decode::Frame::native() -> Option<Native>` | how the renderer imports decode surfaces |
| [B4 backend-trait-registration](base/B4-backend-trait-registration.md) | public `Backend` + registration (breaking; Android only) | the external-backend path |
| [B5 adaptation-conventions](base/B5-adaptation-conventions.md) | moq house style + `Error` variants | no ffmpeg, dlopen, crates.io only, feature-gate heavy deps |

## Codecs

| Module | iroh-live | moq | Zero-copy | Verdict |
|---|---|---|---|---|
| [codec-vaapi-encode](modules/codec-vaapi-encode.md) | validated DMA-BUF encoder + VPP | 111-line unvalidated CPU placeholder | yes: DMA-BUF import, VPP | move |
| [codec-vaapi-decode](modules/codec-vaapi-decode.md) | full stateless decode, PRIME export | none | yes: DMA-BUF export | move |
| [codec-v4l2-encode](modules/codec-v4l2-encode.md) | M2M encode, stride handling | none | partial: M2M, PTS via B2 | move |
| [codec-v4l2-decode](modules/codec-v4l2-decode.md) | M2M decode | none | EXPBUF follow-up on B1 | move |
| [codec-android-mediacodec](modules/codec-android-mediacodec.md) | encoder + two decoders, HardwareBuffer | none | yes: HardwareBuffer | move |
| [codec-decode-surface](modules/codec-decode-surface.md) | VT/MF decode retain GPU surface | VT/MF decode download to I420 | yes: keeps decode-to-render alive | apply |
| [codec-opus](modules/codec-opus.md) | libopus + runtime bitrate, pre-skip, remix | libopus + rate snap, validation | no | apply |
| [codec-pcm](modules/codec-pcm.md) | raw f32 PCM | none | no | move |
| [codec-bitstream-sps-vui](modules/codec-bitstream-sps-vui.md) | SPS VUI low-latency patcher (dead code) | none | no | move |
| [codec-openh264](modules/codec-openh264.md) | openh264 encode/decode | openh264, tested retune, per-frame IDR | no | use moq |
| [codec-videotoolbox-encode](modules/codec-videotoolbox-encode.md) | VT encode (Baseline) | VT encode (H.265, High, IDR) | macOS capture-to-encode is theirs | use moq |
| [codec-dispatch](modules/codec-dispatch.md) | Dynamic HW/SW probe | Candidate/Kind table | no | use moq |
| [codec-annexb](modules/codec-annexb.md) | Annex-B / avcC tooling | `moq_mux::codec` (more general) | no | use moq |
| [codec-config-mirror](modules/codec-config-mirror.md) | catalog mirror (broken vs hang 0.19.5) | hang catalog types | no | use moq |
| [codec-av1](modules/codec-av1.md) | rav1e encode, rav1d decode | NVDEC 8-bit only | no | remove |

## Audio

| Module | iroh-live | moq | Zero-copy | Verdict |
|---|---|---|---|---|
| [audio-backend](modules/audio-backend.md) | full-duplex playback sink + mixing + sonora AEC | capture only, no playback, no AEC | no | move |
| [audio-resample](modules/audio-resample.md) | higher quality, per-call alloc | leaner sinc-128, preallocated | no | use moq |

The Opus and PCM codecs are the `codec-opus` and `codec-pcm` rows above.

## Capture

| Module | iroh-live | moq | Zero-copy | Verdict |
|---|---|---|---|---|
| [capture-pipewire](modules/capture-pipewire.md) | DMA-BUF multi-fourcc into VAAPI | CPU-only | yes: DMA-BUF into encode | move |
| [capture-v4l2](modules/capture-v4l2.md) | enumeration, NV12 passthrough | none | EXPBUF follow-up on B1 | move |
| [capture-libcamera](modules/capture-libcamera.md) | raw + on-device H.264 pre-encoded | none | pre-encoded bitstream path | move |
| [capture-x11](modules/capture-x11.md) | portal-less MIT-SHM screen | `Unsupported` | no | move (leaf pending) |
| [capture-android](modules/capture-android.md) | HardwareBuffer plan (stub) | none | intended HardwareBuffer | move (future) |
| [capture-macos-screen](modules/capture-macos-screen.md) | functional subset + BGRA | app capture, NV12 surfaces | theirs: NV12 surfaces | use moq |
| [capture-macos-camera](modules/capture-macos-camera.md) | stub | complete, zero-copy, TCC | theirs | use moq |
| [capture-windows](modules/capture-windows.md) | doc stubs | MF + Desktop Duplication, GPU-resident | theirs: D3D11 | use moq |
| [capture-nokhwa](modules/capture-nokhwa.md) | fallback camera | native macOS/Windows backends | no | use moq |
| [capture-xcap](modules/capture-xcap.md) | fallback screen | native macOS/Windows backends | no | use moq |

## Render

| Module | iroh-live | moq | Zero-copy | Verdict |
|---|---|---|---|---|
| [render-video-render](modules/render-video-render.md) | the only renderer, Vulkan/EGL/Metal importers | none | yes: the decode-to-render path, three platforms, two graphics APIs | move |
| [render-egui](modules/render-egui.md) | egui integration over the renderer | none | inherits the renderer's path | move |

## Kept local, not upstreamed

For completeness, the media-adjacent code that stays in iroh-live and is neither
moved, applied, nor adopted. Listed so nothing is silently dropped; none of it
is a module.

| Kept | Why |
|---|---|
| test sources + conformance harness (~2,880 LOC, shrinking) | the gate for every adoption and cut; shrinks as adopted backends shed their vectors |
| device traits in `traits.rs` | iroh-live's app-facing device surface; shrinks as `modules/audio-backend.md` lands, but the seam stays |
| `format.rs` CPU half | the I420 and CPU frame plumbing every fallback path uses; the GPU half is the B1 donor |
| `processing/scale.rs` + `convert.rs` | serve capture and render locally; no moq counterpart |
| `audio_file_*` (472 LOC) | app-level decoded-PCM sources; open question on a moq home in `modules/audio-backend.md`, current proposal: stays |
| `moq-media` capture glue + util + test_util | local glue and test scaffolding |
| `moq-media-dioxus` | the dioxus UI integration stays in iroh-live for now (see `modules/render-egui.md`) |
| Android capture stub (34 LOC) | placeholder until Android capture exists anywhere (see `modules/capture-android.md`) |

The transport, pubsub, rooms, adaptive, and sync layers are out of this
campaign's media scope; a separate alignment effort owns them.

## Evidence

The detailed comparisons that back every verdict live in `comparison/`:

- [codecs.md](comparison/codecs.md) - codec-by-codec, backend-by-backend, with the per-backend verdict table.
- [capture.md](comparison/capture.md) - capture backends across Linux, macOS, Windows, Android, plus capture-side zero-copy and audio capture.
- [audio.md](comparison/audio.md) - Opus, resampling, device I/O, AEC, file sources, the A/V sync touchpoint.
- [traits-api.md](comparison/traits-api.md) - open traits versus closed `pub(crate)` backends, the signature tables, and the decision list.
- [moq-changes.md](comparison/moq-changes.md) - the concrete moq-side API changes the base contract is lifted from.
- [iroh-live-code-map.md](comparison/iroh-live-code-map.md) - what iroh-live owns, per-crate LOC.
- [moq-inventory.md](comparison/moq-inventory.md) - what moq offers today, crate by crate.
- [maps/](comparison/maps/) - per-crate code maps of both codebases.

And, above all, [zerocopy.md](zerocopy.md), the deep dive on the frame and
surface models and every zero-copy path.
