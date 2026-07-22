# Codec-by-Codec Comparison: rusty-codecs vs moq

Status: planning artifact (overnight session 2026-07-18, revised 2026-07-22
after moq merged dev into main). Compares iroh-live's `rusty-codecs` against
moq's `moq-video`, `moq-audio`, and `moq-nvenc`, per codec and per backend, to
decide for each piece: cut-and-replace, keep, or upstream. For every backend it
states which implementation is more capable, exactly how they differ, and what
would need to be upstreamed.

moq is now a single branch: the dev line merged into main on 2026-07-21, so the
full native codec stack (encode plus decode, every hardware backend, moq-nvenc,
moq-transcode, moq-stats) is moq main. Every verdict below is actionable against
the next moq release iroh-live bumps to; nothing here is conditional on a branch
that might not land.

Citation convention: unprefixed paths are `rusty-codecs/src/` in this repo;
`moq:` paths are `rs/moq-video/src/`, `rs/moq-audio/src/`, or `rs/moq-nvenc/src/`
in the moq repo at main HEAD `3a3e0ea8`. moq-video's core codec files are
byte-identical between the pre-merge analysis SHA `261c2048` and current main, so
every quote and line citation is exact against main. Evidence maps:
[maps/rusty-codecs.md](maps/rusty-codecs.md),
[maps/moq-video.md](maps/moq-video.md),
[maps/moq-audio-nvenc.md](maps/moq-audio-nvenc.md).

A structural difference that colors every row below: our backends implement a
push/pop trait pair (`VideoEncoder::push_frame` + `pop_packet`,
`traits.rs:311-410`) with `set_bitrate` defaulting to a **silent no-op**
(`traits.rs:352`), and keyframe forcing is not exposed through the trait at
all. moq's backend trait is synchronous call-in/packets-out
(`encode(&Frame, keyframe: bool) -> Vec<Bytes>`) with a mandatory
`set_bitrate` that must return `Error::BitrateUnsupported` rather than
"inherit a silent no-op and quietly ignore congestion"
(`moq:encode/backend/mod.rs:37-57`). Their contract is the better one, and it
is what their rate control (section 9) depends on. The trait and API-shape
questions this raises are analyzed in
[3t-compare-traits-api.md](3t-compare-traits-api.md), and the concrete
moq-side change proposals in [3u-moq-changes.md](3u-moq-changes.md).

---

## 1. H.264

### Encode backends

| Backend | Ours | Theirs | More capable |
|---|---|---|---|
| openh264 (SW) | `codec/h264/encoder.rs` (424 L) | `moq:encode/backend/openh264.rs` (241 L) | theirs (rate control), ours (framing modes) |
| VAAPI (HW, Linux) | `codec/vaapi/encoder.rs` (1,533 L) + `vpp_scaler.rs` (529 L) | `moq:encode/backend/vaapi.rs` (111 L) + external `moq-vaapi` 0.0.2 | **ours, decisively** |
| V4L2 M2M (HW, ARM Linux) | `codec/v4l2/encoder.rs` (1,253 L) | none | ours (only side) |
| VideoToolbox (HW, macOS) | `codec/vtb/encoder.rs` (895 L) | `moq:encode/backend/videotoolbox.rs` (483 L) | **theirs** |
| Android MediaCodec (HW) | `codec/android/encoder.rs` (366 L) | none | ours (only side) |
| NVENC (HW, Linux) | none | `moq:encode/backend/nvenc.rs` (548 L) + `moq-nvenc` | theirs (only side) |
| Media Foundation (HW, Windows) | none (`media-foundation` feature is an empty placeholder) | `moq:encode/backend/mediafoundation.rs` (615 L) | theirs (only side) |

### Decode backends

| Backend | Ours | Theirs | More capable |
|---|---|---|---|
| openh264 (SW) | `codec/h264/decoder.rs` (482 L) | `moq:decode/backend/openh264.rs` (81 L) | ours (output flexibility), parity on decode itself |
| VAAPI (HW, Linux) | `codec/vaapi/decoder.rs` (1,188 L) | **none** | ours (only side) |
| V4L2 M2M (HW) | `codec/v4l2/decoder.rs` (521 L) | none | ours (only side) |
| VideoToolbox (HW) | `codec/vtb/decoder.rs` (594 L) | `moq:decode/backend/videotoolbox.rs` (438 L) | split: theirs H.265, ours GPU output |
| Android MediaCodec | `codec/android/decoder.rs` (337 L) + `hw_decoder.rs` (325 L) | none | ours (only side) |
| NVDEC | none | `moq:decode/backend/nvdec.rs` (706 L) | theirs (only side) |
| Media Foundation / DXVA | none | `moq:decode/backend/mediafoundation.rs` (448 L) | theirs (only side) |

### openh264 vs openh264

Both sides wrap the same vendored Cisco encoder with the same core tuning:
`UsageType::CameraVideoRealTime`, `RateControlMode::Bitrate`, and GOP via
`IntraFramePeriod` (ours `h264/encoder.rs:118-123`, theirs
`moq:encode/backend/openh264.rs:35-41`). Both default bitrate from the same
0.07 bits-per-pixel heuristic (`h264/encoder.rs:106`,
`moq:encode/encoder.rs:94-100`). The differences are real:

- **Dynamic bitrate.** Theirs implements live retune through raw
  `openh264-sys2` FFI (`ENCODER_OPTION_BITRATE` + `SBitrateInfo`), including
  deferred application before the first frame (openh264 rejects `SetOption`
  with `cmInitExpected` until the encoder lazily exists) and
  supersede-the-deferred-value semantics, all pinned by four regression tests
  including read-back verification
  (`moq:encode/backend/openh264.rs`, `apply_bitrate`, `pending`/`started`
  fields, and the `a_live_set_supersedes_a_deferred_one` test). Ours has
  **no** `set_bitrate` implementation; it inherits the silent no-op default
  (`traits.rs:352`), so congestion adaptation on the software path is
  impossible today.
- **Forced IDR.** Theirs forces an IDR on any frame via
  `encode(frame, keyframe: true)` calling `force_intra_frame()`. Ours calls
  `force_intra_frame()` only once, internally, after consuming the priming
  frame in avcC mode (`h264/encoder.rs:136-137`); the trait offers no way for
  a caller to request a keyframe (needed for demand-driven reopen and late
  joiners).
- **Framing modes.** Ours supports both Annex-B in-band output and avcC
  (length-prefixed) output, extracting SPS/PPS by encoding a black priming
  frame and building the avcC description (`h264/encoder.rs:128-142,274-284`).
  Theirs emits Annex-B in-band only (avc3 shape); no avc1 output exists
  anywhere in moq-video.
- **Pipeline integration.** Ours embeds scaling (`scale_if_needed`,
  `h264/encoder.rs:168-177`) and a zero-copy I420 borrow path
  (`YuvSlices`, `h264/encoder.rs:225-238`); theirs takes tightly packed I420
  from its own `Frame` model and leaves scaling to the caller.

Decode: theirs is a minimal Annex-B-in, tight-I420-out shim
(`moq:decode/backend/openh264.rs:44-80`); avc1 handling, parameter-set
injection, and keyframe gating live in the shared front end
(`moq:decode/decoder.rs:94-160`). Ours handles avc3 and avc1 in the backend
itself (feeds avcC parameter sets to the decoder upfront,
`h264/decoder.rs:62-67`), converts to RGBA or BGRA with a reused pixel buffer
(`h264/decoder.rs:113-147`), and applies viewport downscaling after decode
(`set_viewport`, `h264/decoder.rs:83-85,163-166`). Their layering is cleaner
(one avc1 conversion for all backends instead of per-backend); our
presentation conveniences belong in a render layer, not a decoder.

**Verdict: cut and replace with theirs.** Their encoder is strictly more
capable where it matters for live publishing (retune, forced IDR, tested).
Nothing needs upstreaming on this backend if we align to avc3 in-band output;
our avcC output mode only matters for fMP4 export, which `moq-mux` containers
cover differently.

### VAAPI vs VAAPI (+ moq-vaapi)

This is the most lopsided comparison in the whole document, in our favor.

Theirs is a 111-line adapter over the external `moq-vaapi` 0.0.2 crate that
takes CPU frames only: "each frame is interleaved to NV12" on the CPU
(`moq:encode/backend/vaapi.rs:16-18` and `i420_to_nv12`), has no GPU-surface
input path, cannot retune (`set_bitrate` returns
`Error::BitrateUnsupported` because moq-vaapi 0.0.2 has no setter for its
private bitrate field, `moq:encode/backend/vaapi.rs:80-88`), hard-links
libva so a libva-less host fails to load the binary before fallback can
happen (#1837), and the header says outright "NOT YET VALIDATED ON HARDWARE"
(`moq:encode/backend/vaapi.rs:19-21`). It does support per-frame forced IDR
(`encode_nv12(&nv12, keyframe)`), and there is **no VAAPI decode at all** on
their side.

Ours is a full cros-codecs `StatelessEncoder` integration, hardware-validated
(Intel MTL):

- Zero-copy DMA-BUF input: capture frames carrying
  `NativeFrameHandle::DmaBuf` are imported directly as VA surfaces via
  `DRM_PRIME_2` descriptors (`vaapi/encoder.rs:73-128`), selected per frame in
  `push_frame` (`vaapi/encoder.rs:1272-1290`).
- VPP hardware scaling when DMA-BUF dimensions mismatch the target
  (`vpp_scale_or_cpu`, `vaapi/encoder.rs:1069-1099`) and VPP GPU color
  conversion for non-NV12 DMA-BUFs (`vpp_convert_or_cpu`,
  `vaapi/encoder.rs:1009-1064`), each with permanent CPU fallback after a
  failure so a broken VPP never loops.
- Rate-control quality clamping: CBR with QP constrained to 18-36 to prevent
  keyframe "compression bursts" (`vaapi/encoder.rs:850-860`), plus a
  `LowDelay` prediction structure with the H.264 `max_frame_num` floor
  handled (`vaapi/encoder.rs:861-865`).
- avcC extraction via a throwaway priming encoder (`vaapi/encoder.rs:913-946`)
  and input-timestamp propagation through cros metadata
  (`vaapi/encoder.rs:1302-1353`).

Gaps on our side: no `set_bitrate` (silent no-op; theirs at least fails
honestly), and no per-frame forced IDR through the trait
(`force_keyframe: false` hardcoded in `push_frame`,
`vaapi/encoder.rs:1352`); cros-codecs supports it (`FrameMetadata`), so the
capability exists one plumbing step away. cros-codecs `Tunings` also carries
`rate_control`, so a `set_bitrate` implementation is feasible where
moq-vaapi 0.0.2 structurally cannot.

Our decoder has no counterpart at all on their side: 1,188 lines of
cros-codecs stateless H.264 decode producing GPU-resident DMA-BUF frames with
cached PRIME export (`vaapi/decoder.rs:81-119`), a `reset()` implementation
for post-loss reinit (`vaapi/decoder.rs:731`), `burst_size()` for DPB flush
(`vaapi/decoder.rs:616`), and a Baseline `constraint_set0_flag` SPS patch
fixing a real cros-codecs profile-mapping failure
(`vaapi/decoder.rs:37-76`). Without it, moq's Linux decode story is
NVIDIA-only (NVDEC) with openh264 as the sole fallback; every Intel and AMD
Linux machine decodes in software.

**Verdict: upstream ours.** The whole VAAPI stack (encoder zero-copy import,
VPP scaling and conversion, and the entire decoder) is the strongest H.264
upstream candidate we have. What upstreaming requires: adapting to their
`Backend` traits (add honest `set_bitrate` via cros `Tunings`, wire
`force_keyframe` through), mapping our `DmaBufInfo` onto a moq-video frame
variant (their Linux `Frame` enum has only `Cuda` and `I420`,
`moq:frame.rs:23-36`, so a `DmaBuf` variant is a prerequisite), and replacing
or reconciling `moq-vaapi` with cros-codecs (moq-vaapi is itself "vendored
from cros-libva + cros-codecs", so this is a merge, not a rewrite).

### V4L2 M2M (ours only)

No moq counterpart exists. Our encoder drives the V4L2 M2M device with raw
ioctls: sets profile, level (auto-selected from resolution because
bcm2835-codec defaults to Level 1.0, `v4l2/encoder.rs:365-380`), bitrate,
GOP, and both SPS/PPS-repeat controls so every IDR is self-contained
(`v4l2/encoder.rs:536-553`). It respects the driver-negotiated stride and
16-aligned height when filling OUTPUT buffers, deriving the aligned height
from `sizeimage` and deinterleaving to YU12 when the driver rejects NV12
(`queue_frame`, `v4l2/encoder.rs:624-699`; this is the fix from commit
625c16f). The device runs on a dedicated OS thread with bounded channels
(`v4l2/encoder.rs:69-95`). The decoder uses the `v4l2r` crate. This is the
Raspberry Pi and ARM SoC story; moq has nothing for that hardware class.

**Verdict: keep now, upstream later.** It needs no per-frame keyframe or
retune plumbing changes beyond what VAAPI needs (V4L2 exposes
`V4L2_CID_MPEG_VIDEO_BITRATE` and `FORCE_KEY_FRAME` controls at runtime, so
honest `set_bitrate` is implementable). Lower priority than VAAPI because
the audience is smaller.

### VideoToolbox vs VideoToolbox

The closest matchup, and the one where theirs wins.

Shared ground: both create a `VTCompressionSession` over raw objc2 bindings,
set `RealTime`, disable frame reordering, set `AverageBitRate` and
`MaxKeyFrameInterval`, accept zero-copy `CVPixelBuffer` capture input (ours
`vtb/encoder.rs:248-255`, theirs `moq:encode/backend/videotoolbox.rs`,
`Frame::Surface` arm), upload CPU I420 into planar pixel buffers otherwise,
and implement live `set_bitrate` with no IDR (ours `vtb/encoder.rs:329-340`,
theirs `moq:encode/backend/videotoolbox.rs`, `set_bitrate` on
`AverageBitRate`).

Where theirs is ahead:

- **H.265.** Their backend encodes H.264 and HEVC from the same file
  (`kCMVideoCodecType_HEVC`, `kVTProfileLevel_HEVC_Main_AutoLevel`, VPS
  spliced with SPS/PPS on keyframes), with a hardware round-trip test. Ours
  is H.264 only.
- **Profile.** Theirs uses `kVTProfileLevel_H264_High_AutoLevel`; ours pins
  `kVTProfileLevel_H264_Baseline_AutoLevel` (`vtb/encoder.rs:151-155`), which
  costs meaningful compression efficiency on hardware that handles High
  trivially. (Our Baseline choice pairs with the SPS reorder assumptions in
  `sps.rs`, but VT with reordering disabled emits no B-frames on High
  either.)
- **Per-frame forced IDR.** Theirs passes the force-keyframe dictionary
  whenever the caller asks (`keyframe.then_some(&*self.force_keyframe)`).
  Ours has the same dictionary (`build_force_keyframe_props`,
  `vtb/encoder.rs:406-424`) but only an internal `force_next_keyframe` flag;
  the trait cannot request it.
- **Output latency shape.** Theirs calls `complete_frames` after every
  encode, making output synchronous per frame; ours relies on the async
  callback and drains on `pop_packet`, which fits our push/pop model but
  gives less deterministic latency. Theirs also sets `ExpectedFrameRate`,
  which ours does not.

Where ours is ahead: the encoder additionally supports avcC output mode
(description assembled from the format description in the callback), and,
much more importantly, our **decoder** keeps decoded frames on the GPU: each
output is a retained NV12 `CVPixelBuffer` wrapped as a `GpuFrame`
(`vtb/decoder.rs:47-56`), feeding the zero-copy Metal import path in
`render/metal_import.rs`. Their decoder always downloads to packed CPU I420
("macOS decoded frames are always CPU I420",
`moq:decode/backend/videotoolbox.rs:56-60`). Ours also supports deferred
session creation from the first keyframe when no description exists and
detects mid-stream SPS changes (`current_sps`, `vtb/decoder.rs:72-74`);
theirs rebuilds the session on parameter-set change too, and adds H.265.

**Verdict: cut and replace with theirs, then upstream GPU-resident decode
output.** Their encoder is better (H.265, High profile, IDR contract); their
decoder's CPU-I420-only output is a regression for our renderer, so the
concrete upstream item is a macOS decode path that can hand back the
`CVPixelBuffer` (their `Frame` model already has `Surface` on the capture
side, `moq:frame.rs:23-36`; the decode side needs the same variant).

### Android MediaCodec (ours only)

moq-video has no Android backend of any kind. Our encoder configures NV12
MediaCodec sessions with an error counter that resets the codec after
repeated failures (`android/encoder.rs:240-256,323-338`); its `set_bitrate`
merely stores the target and applies it on the next codec reset because
`AMediaCodec_setParameters` needs API 26+ plumbing (`android/encoder.rs:349-359`),
so it is honest but weak. Two decoders exist: a ByteBuffer decoder (CPU
NV12) and a zero-copy ImageReader decoder producing `HardwareBuffer` frames
(`android/hw_decoder.rs`), selected in order by the dynamic dispatcher
(`codec/dynamic.rs:113-128`).

**Verdict: keep; upstream when moq wants an Android target.** Nothing on
their side to compare against, and moq-ffi suggests they will eventually
want it.

### NVENC/NVDEC and Media Foundation (theirs only)

For completeness of the H.264 grid: their NVENC backend (low-latency P4
preset, CBR, `repeatSPSPPS`, `FORCEIDR` flag, `reconfigure` retune with no
IDR, dlopen-only driver loading with a pre-probe,
`moq:encode/backend/nvenc.rs:89-125,251-294`) and NVDEC backend (synchronous
cuvid parsing, zero display delay, hardware scaling via `ulTargetWidth`,
CUDA-resident output frames feeding NVENC zero-copy,
`moq:decode/backend/nvdec.rs`) plus the Media Foundation encode/decode pair
(async MFT with D3D11 zero-copy texture input; DXVA decode with the NV12
allocated-height offset fix, `moq:frame.rs:791-796`) are capabilities we do
not have at all. Adopting moq gains us NVIDIA Linux and all of Windows, and all
of it is on moq main today (`maps/moq-video.md` sections 1 and 2).

---

## 2. H.265

`grep -ri "h265\|hevc" rusty-codecs/src/` returns zero hits: we have no
H.265 support anywhere. moq has hardware-only H.265: encode via
VideoToolbox, Media Foundation, and NVENC; decode via VideoToolbox (#1859),
Media Foundation with the HEVC MFT (#1854), and NVDEC
(`moq:encode/backend/mod.rs:68-102`, `moq:decode/backend/mod.rs:89-114`).
There is deliberately no software path ("H.265 has no software encoder or
decoder", `moq:decode/decoder.rs` test comments), so H.265 is
hardware-capable-peers only.

**Verdict: pure gain from adopting moq.** Nothing to keep or upstream. This is
one of the capabilities the full native stack brings, and it is on moq main
today.

---

## 3. AV1

We are the only party with a software AV1 path, on either side, in either
direction.

Ours: `Av1Encoder` on rav1e 0.8 with a live-streaming configuration
(speed preset 10, `low_latency = true`, `error_resilient = true`, bitrate
rate control with quantizer floor, `av1/encoder.rs:46-59`), a timestamp map
that survives rav1e's lookahead reordering (`av1/encoder.rs:29,239-241`),
and a full ISOBMFF codec-string parameter set in the catalog config
including color metadata matched to our BT.601 conversion pipeline
(`av1/encoder.rs:148-178`, `config.rs:90-116`). `Av1VideoDecoder` wraps
rav1d through a safe shim (`av1/rav1d_safe.rs`, 196 L) with
`max_frame_delay = 1` for latency (`av1/decoder.rs:46-48`) and stride-checked
plane conversion (`av1/decoder.rs:95-98`). No `set_bitrate` (rav1e cannot
retune a live context cheaply; this is a real limitation for rate control).

Theirs: **decode only, NVDEC only** (#2178), gated to 8-bit 4:2:0
non-monochrome (`is_supported_av1`, `moq:decode/decoder.rs:187-189`), and no
AV1 encode anywhere ("AV1 is decode-only (no encoder anywhere)",
`maps/moq-video.md` section 5). Their own test pins that software AV1
decode fails to open (`av1_is_supported_by_hardware_only`,
`moq:decode/decoder.rs` tests). Their public encode `Codec` enum is
`{H264, H265}` only (`moq:encode/encoder.rs:21-40`).

**Verdict: strong upstream candidate, both directions.** Upstreaming our
rav1e encoder gives moq its first AV1 encode of any kind; upstreaming the
rav1d decoder gives every non-NVIDIA machine an AV1 decode fallback and
completes their hardware-then-software fallback story for AV1 the way
openh264 completes it for H.264.

Dependency risk that must be resolved first: our decode depends on a **git
dependency on the memorysafety rav1d fork** (`rav1d` git, `bitdepth_8/16`,
`asm` features; `Cargo.toml` per `maps/rusty-codecs.md` section 5). moq
pins crates.io versions throughout and runs release-plz; a git dependency
will not be accepted. Options: publish the fork pin, move to a released
rav1d/dav1d-rs, or vendor the safe wrapper the way they vendored moq-nvenc.
rav1e 0.8 is crates.io and unproblematic. Encoder CPU cost also needs
stating honestly upstream: rav1e at speed 10 is usable at conference
resolutions, not at 1080p60 on small cores.

---

## 4. VP9

Neither side implements VP9 in any form. Ours: no encoder, no decoder, no
config type (`config.rs` has `H264`, `AV1`, `Other(String)` only,
`config.rs:53-61`). Theirs: "VP9 appears nowhere in the crate"
(`maps/moq-video.md` section 5). The hang catalog can describe it
(`VideoCodec::VP9` with a `vp9.rs` descriptor), and moq-mux has a `vp9` codec module
for container import, so a browser publishing VP9 can be cataloged but not
natively decoded by either stack.

**Verdict: non-issue.** No work, no verdict beyond noting the shared gap.

---

## 5. Opus

Both sides use `unsafe-libopus = "0.2"`, the same pure-Rust c2rust
transpile, chosen for the same RUSTSEC-2026-0150 reason. The engines are
identical; the wrappers differ in configuration surface and runtime
adaptability.

| Capability | Ours (`codec/opus/`, 804 L) | Theirs (`moq:rs/moq-audio`) |
|---|---|---|
| Application mode | `OPUS_APPLICATION_VOIP` (`opus/encoder.rs:58`) | `OPUS_APPLICATION_AUDIO` (`moq:encode/encoder.rs:177`) |
| Codec rate | fixed 48 kHz (`opus/encoder.rs:15,160-164`) | 8/12/16/24/48 kHz, snapped up from input (`moq:opus.rs:13,20-22`) |
| Frame duration | fixed 20 ms (960 samples, `opus/encoder.rs:16-17`) | 2.5/5/10/20/40/60 ms configurable (`moq:opus.rs:16`) |
| Runtime bitrate | **yes**, `set_bitrate` via `OPUS_SET_BITRATE` (`opus/encoder.rs:206-219`) | **no**, bitrate applied at construction only (`moq:encode/encoder.rs:182-188`) |
| In-band FEC | explicitly disabled, phase-3 TODO (`opus/encoder.rs:76-83`) | never touched (defaults off); no ctl call at all |
| DTX | explicitly disabled, phase-3 TODO (`opus/encoder.rs:84-88`) | never touched |
| Complexity setting | none (libopus default) | none |
| PLC / lost-packet decode | none; `opus_decode_float(..., 0)` with real data only (`opus/decoder.rs:80-89`) | none; same, fec flag 0 (`moq:decode/decoder.rs:117-133`), no `decode_lost` |
| OpusHead description | built with **queried** encoder lookahead as pre-skip (`OPUS_GET_LOOKAHEAD`, `opus/encoder.rs:91-108,222-237`) | built via `moq_mux::codec::opus::Config { sample_rate, channel_count }.encode()` (`moq:encode/encoder.rs:263`) |
| Decoder output shaping | resample + channel remap built in, including N-to-M mixdown (`opus/decoder.rs:97-111,136-186`) | resample at the Consumer layer; channel remap **rejected** ("remapping isn't implemented", `moq:decode/decoder.rs:16-46`) |
| PCM format flexibility | interleaved f32 only (`traits.rs:81`) | U8/S16/S32/F32, interleaved and planar, WebCodecs-shaped (`moq:format.rs:5-35`) |
| Grouping | transport-agnostic packets; grouping lives in moq-media | one packet per moq-lite group, dropped groups left to "Opus PLC" that the decoder never actually invokes (`moq:encode/producer.rs:219-233`) |

Which is more complete: neither dominates. Theirs has the richer
configuration surface (rates, frame durations, PCM formats) and the cleaner
sans-I/O layering; ours has the two runtime capabilities theirs lacks
(live bitrate retune, channel remap) and a more correct OpusHead (their
pre-skip does not come from the actual encoder lookahead). Critically,
**neither side implements loss concealment**: their producer comment claims
"Opus PLC handles dropped groups", but their decoder has no lost-packet
entry point, so a dropped group is simply a gap. Our phase-3c plan
(`plans/media-pipeline/phase-3c-fec.md`) covers exactly this and applies to
both.

**Verdict: cut and replace the wrapper with moq-audio, and upstream three
things.** A merged implementation needs, in order: (1) runtime
`set_bitrate` on their `Encoder` (one ctl call; also the prerequisite for
extending their video-side rate control to audio), (2) FEC/PLC/DTX per our
phase-3c design, which their packet-per-group transport makes more
valuable, not less, and (3) either channel remap or an explicit resolved
policy for mono/stereo mismatch, since today their Consumer errors where
ours mixes. The lookahead-derived pre-skip is a one-line correctness fix
worth carrying along.

---

## 6. PCM

Ours only: `PcmEncoder` (234 L) and `PcmAudioDecoder` (310 L) pass raw
interleaved little-endian f32 in fixed 20 ms frames, deliberately matching
Opus framing so the pipeline behaves identically without compression
(`pcm/encoder.rs:11-16`), with resample and channel conversion on the
decode side (`pcm/decoder.rs:51-85`). It is used as the uncompressed
publish path behind the `pcm` feature in moq-media
(`moq-media/src/publish.rs:819-821`) and serves latency and pipeline tests
where codec artifacts and lookahead would confound measurements.

moq has no PCM codec and, more decisively, the hang catalog has no PCM
codec variant: `AudioCodec` is `{AAC, Opus, Mp2, Ac3, Ec3, Unknown}`, so a
PCM track would ride `Unknown("pcm")` with no interop meaning. Browsers
cannot consume it either.

**Verdict: keep local.** Upstreaming would require a catalog codec
definition for a format with no cross-implementation value; as a local
test and diagnostics codec it costs 559 lines and earns them. Do not
upstream; drop only if the moq-media publish layer it serves is itself
deleted.

---

## 7. Bitstream helpers

Ours: `codec/h264/annexb.rs` (364 L) provides a lazy Annex-B NAL iterator
(`annexb.rs:1-61`), SPS/PPS extraction (`annexb.rs:72-87`), avcC
**construction** (`build_avcc`, `annexb.rs:90-111`), avcC parsing back to
Annex-B (`annexb.rs:115-161`), and both directions of Annex-B and
length-prefixed conversion (`annexb.rs:164-192`). `codec/h264/sps.rs`
(586 L) is an exp-golomb SPS VUI patcher that rewrites
`max_num_reorder_frames = 0` and `max_dec_frame_buffering = 1` to strip DPB
reordering latency on Baseline streams (`sps.rs:1-13`); it is currently
`#[allow(dead_code)]`.

Theirs: the equivalent logic lives in `moq_mux::codec` and the decode front
end. `h264::Avcc::parse` and `h265::Hvcc::parse` handle the description
records, `annexb::build_prefix` assembles parameter-set prefixes, and
`annexb::from_length_prefixed(payload, length_size, prefix)` converts with
the **actual** length size from the record while injecting parameter sets
ahead of keyframes (`moq:decode/decoder.rs:94-140,163-176`). On the encode
side the VideoToolbox backend does its own AVCC-to-Annex-B rewrite with
format-description splicing (`moq:encode/backend/videotoolbox.rs:1-14`).

Honest comparison: theirs is more general where the two overlap. Our
`length_prefixed_to_annex_b` hardcodes 4-byte lengths (`annexb.rs:164-178`)
where theirs honors `lengthSizeMinusOne`; our `build_avcc` emits exactly
one SPS and one PPS where their parser accepts several; and theirs covers
H.265 (hvcC, VPS) which we do not touch. Ours is more general in the one
direction theirs lacks entirely: producing avcC (moq never emits avc1), and
the SPS VUI patcher has no counterpart anywhere in moq.

**Verdict: cut and replace with `moq_mux::codec`.** Two residual pieces:
`build_avcc` only matters if an avc1 output mode is ever wanted upstream
(park it), and the VUI patcher is a genuinely useful decoder-latency trick
worth offering upstream as an optional pass, but it is dead code today and
should not block anything.

---

## 8. Dynamic dispatch and selection

Ours: two layers. `codec.rs` enumerates concrete encoder backends as a
strum enum (`"h264-vaapi"`, `"h264-vtb"`, ...) with `available()`,
`best_available()` (hardware preferred), and `create_encoder()`
(`codec.rs:97-216`). `codec/dynamic.rs` holds `DynamicVideoDecoder`, whose
`new()` hardcodes the hardware probe order VAAPI, V4L2, VideoToolbox,
Android HW, Android ByteBuffer, then falls back to openh264
(`dynamic.rs:83-134`), governed only by `DecoderBackend::Auto | Software`
(`format.rs:905-916`). The decoder trait carries `reset()`,
`set_viewport()`, and `burst_size()` (`traits.rs:379-410`), which the
dispatcher forwards.

Theirs: a data-driven `Candidate` table per direction (name, supported
codecs, `open` fn pointer), filtered by codec, ordered by
`Kind::{Auto, Hardware, Software, Named(String)}`, trying each in order
and returning `NoEncoder`/`NoDecoder` errors that **list what was tried**
(`moq:encode/backend/mod.rs:60-133`, `moq:decode/backend/mod.rs:89-145`).
The decode config adds `latency_max` and best-effort `resize`
(`moq:decode/decoder.rs:43-58`).

Honest comparison: the runtime behavior of `Auto` is the same on both
sides (attempt hardware construction, fall through on error, land on
software). Theirs is better engineering and more capable at the selection
level: `Named` enables pinning a backend (their tests depend on it, ours
cannot express it), `Hardware` enables fail-fast policies, adding a
backend is one table row instead of edits to an enum, a macro, and a
probe chain, and the tried-list errors are worth real debugging time.
Ours is more capable at the decoder-lifecycle level: their trait has no
`reset()` for post-loss hardware reinit and no `burst_size()`, which our
HW decoders need and which will have to come along when the VAAPI, V4L2,
and Android decoders are upstreamed. Their `resize` is honored by NVDEC
for free; our `set_viewport` is a CPU post-scale, which is presentation
logic and should die with the rest of it.

**Verdict: cut and replace with their Candidate/Kind model.** Upstream
requirement created elsewhere in this document: their `Backend` decode
trait needs `reset` (and possibly a burst hint) once stateful hardware
decoders beyond NVDEC land.

---

## 9. Rate control

Theirs (#2303, `moq:encode/rate.rs`): a pure `Policy`/`Control` object fed
by `moq_net::bandwidth::Consumer`. Headroom 0.9 of the estimate, ceiling
at the configured bitrate, floor at a tenth, 5% hysteresis that does not
starve suppressed raises, immediate drops, 25%/s ramped recovery
(`moq:encode/rate.rs:22-50,116-160`). Wired in the capture loop: retunes
between frames, and a backend answering `BitrateUnsupported` retires rate
control cleanly (`moq:encode/producer.rs:257-284,353-379`). Every backend
must implement `set_bitrate` without forcing an IDR; NVENC reconfigures in
place, VideoToolbox sets `AverageBitRate`, Media Foundation sets
`CODECAPI_AVEncCommonMeanBitRate`, openh264 goes through raw FFI with
deferred application, and VAAPI honestly declines.

Ours: the trait default is a silent no-op (`traits.rs:352`); only three
backends implement it (VideoToolbox live, `vtb/encoder.rs:329-340`; Opus
live, `opus/encoder.rs:206-219`; Android deferred to the next codec reset,
`android/encoder.rs:349-359`). Nothing calls `set_bitrate` from a
congestion signal anywhere in the workspace: adaptive encoding is
phase 3d, planned and not implemented
(`plans/media-pipeline/phase-3d-adaptive-encoding.md`). Our adaptive work
to date (phase 3a) is receive-side rendition switching, a different layer.

**Verdict: theirs is ahead publish-side, full stop.** Adopt their
mechanism and policy. The obligations flow into our upstreaming verdicts:
every backend we upstream (VAAPI, V4L2, Android) must arrive with an
honest `set_bitrate`, because their trait makes silence a compile-time
impossibility, and that is the correct design.

---

## 10. Verdict table

Per codec and backend. "Adopt" means the capability comes from moq by aligning
to the next release; all of it is on moq main today, so nothing here is gated on
an unmerged branch.

| Codec / piece | Backend | Verdict | Reason |
|---|---|---|---|
| H.264 encode | openh264 | cut, replace (adopt moq) | theirs adds tested live retune and per-frame IDR; ours adds nothing they need |
| H.264 decode | openh264 | cut, replace (adopt moq) | parity on decode; their avc1 front-end layering is better; our RGBA/viewport logic is presentation, not decode |
| H.264 encode | VAAPI | **upstream ours** | zero-copy DMA-BUF + VPP + validated vs their 111-line unvalidated CPU-only placeholder; add `set_bitrate` + forced-IDR plumbing in the port |
| H.264 decode | VAAPI | **upstream ours** | theirs does not exist; fills moq's Intel/AMD Linux decode gap; brings `reset()`/`burst_size()` needs into their trait |
| H.264 enc+dec | V4L2 M2M | keep, upstream later | ours only; ARM SoC (Pi) class moq lacks; stride/alignment handling (625c16f) is the hard-won part |
| H.264 encode | VideoToolbox | cut, replace (adopt moq) | theirs: H.265, High profile, per-frame IDR, ExpectedFrameRate; ours has no advantage encode-side |
| H.264 decode | VideoToolbox | replace, upstream GPU output | theirs adds H.265 but downloads every frame to CPU I420; upstream a CVPixelBuffer-out path for zero-copy render |
| H.264 enc+dec | Android MediaCodec | keep, upstream on demand | ours only; includes zero-copy HardwareBuffer decoder; moq has no Android story yet |
| H.264 enc+dec | NVENC/NVDEC | adopt (moq gain) | we have nothing on NVIDIA; includes zero-copy transcode and retune |
| H.264 enc+dec | Media Foundation | adopt (moq gain) | our `media-foundation` feature is an empty placeholder; theirs is complete |
| H.265 all | VT / MF / NVENC / NVDEC | adopt (moq gain) | zero hits for h265/hevc in rusty-codecs |
| AV1 encode | rav1e (SW) | **upstream ours** | the only AV1 encoder in either stack |
| AV1 decode | rav1d (SW) | **upstream ours** | their AV1 decode is NVDEC-only 8-bit 4:2:0; ours is the universal fallback; resolve the git-fork dependency first |
| AV1 decode | NVDEC | adopt (moq gain) | hardware path we lack |
| VP9 | none | non-issue | neither side implements it; catalog-only |
| Opus | unsafe-libopus | cut, replace; upstream 3 items | same engine; adopt their layering; upstream runtime `set_bitrate`, FEC/PLC/DTX (phase 3c), channel-remap policy, lookahead pre-skip fix |
| PCM | raw f32 | keep local | no hang catalog codec, no interop value; useful for tests and diagnostics |
| Bitstream helpers | annexb/sps | cut, replace with moq-mux | theirs handles variable length size, multi param sets, and hvcC; park `build_avcc`; offer the SPS VUI patcher upstream as optional |
| Dispatch/selection | enums vs Candidate | cut, replace (adopt moq) | `Kind::Named`/`Hardware` and tried-list errors beat our hardcoded probe; carry `reset()`/`burst_size()` into their decode trait |
| Rate control | Policy/Control | adopt (moq gain) | complete, tested, wired to bandwidth estimation; we have a plan (3d), they have an implementation |

### Top upstream candidates, in priority order

1. **VAAPI encoder and decoder** (zero-copy DMA-BUF import, VPP scaling
   and conversion, full stateless decode with GPU-resident output). Fills
   moq's largest platform gap (Intel/AMD Linux) and replaces a placeholder
   their own header calls unvalidated.
2. **Software AV1** (rav1e encode, rav1d decode). The only AV1 encode in
   existence on either side and the software decode fallback their
   NVDEC-only path needs. Gated on resolving the rav1d git-fork pin.
3. **GPU-resident decode output** (VideoToolbox CVPixelBuffer, VAAPI
   DMA-BUF). moq decode output is CPU I420 everywhere except NVDEC; this
   is the difference between a render pipeline and a benchmark.
4. **V4L2 M2M encode and decode.** ARM SoC support with the driver stride
   and alignment handling already debugged on real hardware.
5. **Android MediaCodec** (encoder plus dual decoders including zero-copy
   HardwareBuffer). Whole platform moq lacks.
6. **Opus runtime bitrate, FEC/PLC/DTX, and channel remap.** Small
   changes; the FEC/PLC work (phase 3c) benefits both stacks and their
   packet-per-group design especially.
7. **SPS VUI patcher.** Minor, optional, currently dead code.

### No branch gamble, stated plainly

Every "adopt" and every comparison target in this document is on moq main at
`3a3e0ea8`. The dev line merged into main on 2026-07-21, so moq-video is now the
full native stack (encode plus decode, VideoToolbox, Media Foundation, NVENC,
NVDEC, VAAPI, openh264, candidate dispatch, H.265, NVDEC AV1, and rate control;
`maps/moq-video.md` sections 1, 2, and 7), and moq-audio, moq-nvenc,
moq-transcode, and moq-stats ship alongside it. The thin ffmpeg-only main that an
earlier draft weighed against no longer exists. Every verdict above is therefore
actionable against the next moq release iroh-live bumps to, with no dependency on
an unmerged branch.

One current-main delta strengthens the case for dropping our config mirror:
`hang` is now 0.19.5 and #2420 renamed its catalog `displayRatio*` fields to
`displayAspect*`, so `rusty-codecs` `config.rs` (which still mirrors
`display_ratio_*`, `config.rs:11-33`) no longer compiles against it. The mirror
was always a transport-agnostic copy of the hang catalog shapes; keeping it in
lockstep with an evolving `hang` is exactly the maintenance the alignment is
meant to retire.
