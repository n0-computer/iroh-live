# Coverage audit: rusty-codecs + rusty-capture vs the upstream plan set

Goal: verify no iroh-live codec, capture, or optimization is silently lost in the
moq-upstreaming plans (`base/`, `codec/`, `capture/`, `render/`).

## Counts

- **COVERED** (a plan carries it upstream): 28
- **dropped-by-decision** (a comparison verdict says "adopt moq's" / "keep local, do
  not upstream", cited): 19
- **PARTIAL** (partly carried; one aspect unassigned by any plan): 1
- **LOST** (accidentally missing, no plan and no verdict): 0

Total distinct items tracked: 48.

## Verdict

**The plan set is comprehensive.** Every codec backend, capture backend, and
non-obvious optimization is either carried by a named plan or explicitly disposed of
by a cited comparison verdict (adopt moq's equivalent, or keep local). The single
gap is the **PipeWire portal *camera* capturer**, which `comparisons/capture.md`
says to keep but which `capture/pipewire-dmabuf.md` (screen-only) drops without an
explicit keep-local verdict. Nothing is silently lost.

## PARTIAL / LOST items (one line each)

- **PARTIAL — PipeWire portal camera capturer** (`PipeWireCameraCapturer`,
  `rusty-capture/.../pipewire.rs:1513`): `pipewire-dmabuf.md` is screen-only and drops
  our camera code as "moq already owns portal negotiation" (true for screen, not
  camera; moq has no portal camera at all), yet `capture.md` verdict says "keep ours
  ... and the camera support". Should be carried by `capture/pipewire-dmabuf.md` (a
  camera sibling) or a new capture plan, or given an explicit keep-local verdict.

No LOST items.

---

## Full coverage matrix

### Codec backends

| Feature | Status | Plan / verdict |
|---|---|---|
| openh264 encode | dropped-by-decision | `codecs.md` §1: cut & replace, adopt moq's (tested live retune + per-frame IDR) |
| openh264 decode | dropped-by-decision | `codecs.md` §1: parity; moq's avc1 front-end layering is better |
| VAAPI encode | COVERED | `codec/vaapi-encode.md` |
| VAAPI decode | COVERED | `codec/vaapi-decode.md` (owns moq-vaapi decode+export growth) |
| V4L2 M2M encode | COVERED | `codec/v4l2-encode.md` |
| V4L2 decode | COVERED | `codec/v4l2-decode.md` |
| VideoToolbox encode | dropped-by-decision | `codecs.md` §1: adopt moq's (H.265, High profile, IDR contract) |
| VideoToolbox decode | COVERED | `codec/vtb-mf-decode-surface.md` (GPU CVPixelBuffer-out) |
| Android MediaCodec encode | COVERED | `codec/android-mediacodec.md` |
| Android MediaCodec decode + hw_decoder | COVERED | `codec/android-mediacodec.md` (both ByteBuffer + ImageReader/HardwareBuffer) |
| rav1e AV1 encode | COVERED | `codec/av1-software.md` (gated on rav1d fork resolution, CP4) |
| rav1d AV1 decode | COVERED | `codec/av1-software.md` |
| Opus enc/dec | COVERED | `codec/opus-improvements.md` (merge into moq-audio) |
| PCM enc/dec | COVERED | `codec/pcm.md` (offer; likely declined, plan states low value) |

### Codec optimizations / details

| Feature | Status | Plan / verdict |
|---|---|---|
| VAAPI decode PRIME export + vaSyncSurface-before-export + per-frame OnceCell export cache | COVERED | `codec/vaapi-decode.md` (preserved verbatim, tested) |
| VAAPI encode DMA-BUF PRIME import + VPP scale + VPP color-convert + forced-IDR | COVERED | `codec/vaapi-encode.md` |
| V4L2 driver stride/height align (625c16f) + profile/level auto-select + SPS/PPS-repeat + pipelined SyncSender | COVERED | `codec/v4l2-encode.md` (all explicitly ported) |
| H.264 annexb (NAL iter, AnnexB↔avcC, SPS/PPS extraction) | dropped-by-decision | `codecs.md` §7: replace with `moq_mux::codec` (variable length-size, multi param-set, hvcC); `build_avcc`/avc1 parked |
| SPS VUI low-latency patcher (`sps.rs`) | COVERED | `codec/bitstream-sps-vui.md` (opt-in pass) |
| SPS Baseline constraint_set0 patch | COVERED | `codec/vaapi-decode.md` (`patch_baseline_constraint_flag` ported + unit-tested) |
| Dynamic HW→SW decoder probe (Dynamic{Video,Audio}Decoder) | dropped-by-decision | `codecs.md` §8: replace with moq Candidate/Kind table (Named/Hardware + tried-list errors) |
| `reset()` / `burst_size()` on decoders | COVERED | `codec/vaapi-decode.md` (threaded into moq decode Backend trait or folded into decode) |
| `set_viewport()` on decoders | dropped-by-decision | `codecs.md` §8: CPU post-scale is presentation logic; moq uses `Config::resize` |
| Opus runtime set_bitrate, lookahead pre-skip, FEC/DTX ctls, channel remix | COVERED | `codec/opus-improvements.md` (FEC/PLC reserved as groundwork per phase-3c) |
| PCM uncompressed test path | COVERED | `codec/pcm.md` |
| processing/scale (pic-scale CPU scaler; encoder `scale_if_needed`) | dropped-by-decision | `codecs.md` §1: moq leaves scaling to caller; moq uses `fast_image_resize` (B5 evidence). Not named in any plan — implicit |
| processing/convert (yuv colorspace) | dropped-by-decision | moq already uses the `yuv` crate (B5 evidence) |
| resample (rubato) | dropped-by-decision | `audio.md` §2/§7: converge on moq's leaner rubato wrapper; remix kept via opus-improvements |
| Render zero-copy importers (Vulkan/ash DMA-BUF + VppRetiler Y_TILED→CCS, GLES EGLImage, Metal CVMetalTextureCache) | COVERED | `render/moq-video-render.md` (~3,500 LOC ported to out-of-tree crate, Option B) |
| WgpuVideoRenderer NV12/I420→RGBA shaders + render_cached + 3-strike fallback | COVERED | `render/moq-video-render.md` |

### Capture backends / optimizations

| Feature | Status | Plan / verdict |
|---|---|---|
| PipeWire DMA-BUF (multi-fourcc BGRA/BGRx/RGBA/RGBx/NV12/YUYV via DRM mapping) | COVERED | `capture/pipewire-dmabuf.md` (ports `spa_format_to_drm_fourcc`, drops NV12-only gate) |
| PipeWire portal + restore-token | dropped-by-decision | `capture.md`: moq already owns portal + restore-token replay (better than ours) |
| PipeWire portal camera capturer | **PARTIAL** | screen plan drops it; `capture.md` verdict says keep — no plan carries it |
| V4L2 camera (MMAP, MJPEG, enumeration, EXPBUF-dead) | COVERED | `capture/v4l2-camera-enum.md` (enum + capture; adopts moq's zune-jpeg; EXPBUF zero-copy scoped as follow-up on B1) |
| X11 (MIT-SHM / RANDR) | dropped-by-decision | `capture.md` §5: keep as fallback for portal-less Linux; do not upstream |
| libcamera raw (rpicam yuv420) | COVERED | `capture/libcamera-preencoded.md` (raw companion, optional/same-or-follow-up PR) |
| libcamera_h264 on-device H.264 (rpicam-vid pre-encoded source) | COVERED | `capture/libcamera-preencoded.md` (+ `publish_preencoded` concept, CP5 gate) |
| ScreenCaptureKit | dropped-by-decision | `capture.md`: adopt moq's (app capture, NV12 surfaces, fail-fast perms) |
| AVFoundation camera | dropped-by-decision | `capture.md`: ours is a stub; adopt moq's working zero-copy backend |
| nokhwa | dropped-by-decision | `capture.md` §5: keep as fallback; not upstreamed |
| xcap | dropped-by-decision | `capture.md` §5: keep as fallback; not upstreamed |
| GpuFrame / NativeFrameHandle model | COVERED | `base/B1-frame-vocabulary.md` (public `Native` + `DmaBuf`/`HwBuffer`/CvPixelBuffer) |
| Demand-gating (SharedVideoSource / track.unused()) | dropped-by-decision | `capture.md` §5: adopt moq's drop-to-release `FrameStream`/`publish_capture` (better contract) |
| VideoFormat pixel-format model | dropped-by-decision | `capture.md` §5: adopt moq's normalized I420/typed-surface frame model (fixes our Rgba/Bgra misreport) |

### Base infrastructure (enabling changes)

| Feature | Status | Plan |
|---|---|---|
| Public GPU-frame vocabulary + `Frame::DmaBuf`/`HardwareBuffer` | COVERED | `base/B1-frame-vocabulary.md` |
| PTS through encode (pipelined V4L2/Android) | COVERED | `base/B2-pts-through-encode.md` |
| `decode::Frame::native()` accessor | COVERED | `base/B3-decode-native-accessor.md` |
| Public registerable Backend trait (Android Path B) | COVERED | `base/B4-backend-trait-registration.md` (CP6-gated) |
| Adaptation conventions + moq Error variants | COVERED | `base/B5-adaptation-conventions.md` |

### Audio device layer (moq-media adjacent, outside rusty-codecs/rusty-capture)

| Feature | Status | Verdict |
|---|---|---|
| AEC / audio_backend duplex, mixing, declicker, metering, device switching | dropped-by-decision | `audio.md` §7: keep local, no moq equivalent; must not be cut (standalone-crate upstream candidate, no plan yet) |
| audio_file_symphonia PCM file source | dropped-by-decision | `audio.md` §4/§7: keep ours; use moq-mux importers alongside |

## Notes on the thin spots

- **processing/scale**: disposed only in the comparison prose (`codecs.md` §1), never
  restated in a plan. Not a lost capability (moq ships `fast_image_resize` and pushes
  scale to the caller / VPP), but worth a one-line note in a codec-encode plan or B5.
- **PipeWire camera** is the one genuine gap; see PARTIAL above.
- Everything the task's checklist enumerated is present. `reset()`/`burst_size()`,
  the vaSyncSurface-before-export ordering, the OnceCell export cache, the 625c16f
  stride/alignment fix, the VppRetiler Y_TILED→CCS re-tile, the Baseline
  constraint_set0 patch, the lookahead pre-skip, and both Android decoders are each
  explicitly named in their carrying plans.
