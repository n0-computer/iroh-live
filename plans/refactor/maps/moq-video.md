# Map: moq main `rs/moq-video` (HEAD 3a3e0ea8, 2026-07-21)

SOURCE: moq main, HEAD 3a3e0ea8 (2026-07-21); dev merged into main.

Evidence-backed map of the native video codec + capture stack. All `file:line`
references are relative to `rs/moq-video/` and use line numbers from
`git show 3a3e0ea8:<path>`. This crate is the moq counterpart to iroh-live's
rusty-codecs/rusty-capture and is the single most important overlap for the refactor.

Note on provenance: the native codec/backend/frame/trait content below was first
analysed on the pre-merge dev branch at SHA 261c2048. moq-video's core codec files
(`encode/encoder.rs`, `encode/backend/mod.rs`, `decode/decoder.rs`,
`decode/backend/mod.rs`, `frame.rs`, `lib.rs`) are **byte-identical** between 261c2048
and current main, so every quote and citation here is exact against main. The codec API
is unchanged since the pre-merge analysis.

Crate: `moq-video 0.0.6`, "Native video capture/encoding/decoding for Media over QUIC"
(Cargo.toml). Layout (`src/lib.rs:46-58`): public modules `capture`, `decode`,
`encode`; private `error`, `frame`, `size`, and Windows-only `mf`. The public API is
deliberately codec/backend-agnostic — no public type names a backend, all configs are
`#[non_exhaustive]` (`lib.rs:35-44`).

Sibling context: `rs/moq-nvenc` is an in-tree fork of nvidia-video-codec-sdk (dlopen
NVENC + a cuvid table for NVDEC). `moq-vaapi 0.0.2` is an external crates.io crate
(repo moq-dev/vaapi, "vendored from cros-libva + cros-codecs" per workspace
`Cargo.toml:94-95`). `rs/moq-transcode` is the in-tree consumer of the zero-copy
decode->encode path.

---

## 1. Encode API

### Core types (encode/encoder.rs)

The public output-codec and backend-selection enums, verbatim
(`encode/encoder.rs:21-48`):

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
	/// H.264 / AVC, Annex-B with in-band SPS/PPS (the "avc3" shape). The widest
	/// support and the default.
	#[default]
	H264,
	/// H.265 / HEVC, Annex-B with in-band VPS/SPS/PPS (the "hev1" shape).
	H265,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform hardware encoder, falling back to the openh264 software
	/// encoder when none is available.
	#[default]
	Auto,
	/// Hardware only; error if none is available.
	Hardware,
	/// Software only (openh264 for H.264).
	Software,
	/// A specific backend by name, e.g. `"videotoolbox"`, `"nvenc"`, `"vaapi"`,
	/// or `"openh264"`.
	Named(String),
}
```

`Config` (`encode/encoder.rs:55-70`): `width`, `height`, `framerate`, `bitrate:
Option<u64>` (default derives ~0.07 bits/pixel/s, ~4.4 Mbps for 1080p30,
`encoder.rs:94-100`), `gop: u32` (default 2s worth of frames), `codec: Codec`,
`kind: Kind`. `#[non_exhaustive]`.

The public `Encoder` front end (`encode/encoder.rs:106-111`):

```rust
pub struct Encoder {
	backend: Box<dyn Backend>,
	codec: Codec,
	size: Size,
	bitrate: u64,
}
```

Public surface:
- `Encoder::new(&Config)` — validates framerate != 0 and even dimensions, then
  `backend::open` (`encoder.rs:115-133`).
- `encode_rgba(&[u8], Size, keyframe: bool) -> Result<Vec<Bytes>>` — tightly packed
  RGBA, converted to I420 via the `yuv` crate (BT.601 limited) (`encoder.rs:219-227`).
- `encode_i420(&[u8], Size, keyframe) -> Result<Vec<Bytes>>` — bring-your-own I420,
  copies to take ownership (`encoder.rs:240-249`).
- `encode(&crate::decode::Frame, keyframe)` — **the transcode input path**: takes a
  decoded frame directly and keeps a GPU frame on the GPU (NVDEC -> NVENC)
  (`encoder.rs:279-281`).
- `set_bitrate(u64)` — live retune, no IDR forced; state updated only after the
  backend accepts (`encoder.rs:196-205`).
- `finish(self) -> Result<Vec<Bytes>>` — consuming flush (`encoder.rs:300-302`).

Raw input type: the crate-private `Frame` enum (see §4). Encoded output type:
`Vec<Bytes>`, one Annex-B access unit per packet, in-band parameter sets.

### Backend trait + dispatch (encode/backend/mod.rs)

The trait, verbatim (`encode/backend/mod.rs:37-57`):

```rust
/// An opened video encoder. Feed it frames at the configured resolution; get
/// back zero or more packets in the codec's wire framing.
pub(crate) trait Backend: Send {
	/// Encode one frame. Set `keyframe` to force an IDR (e.g. on resume so a
	/// re-subscribing viewer can start decoding at once).
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>;

	/// Flush the encoder, returning any buffered packets.
	fn finish(&mut self) -> Result<Vec<Bytes>, Error>;

	/// Retune the live encoder to `bitrate` bits per second, taking effect from
	/// roughly the next frame. Called as the congestion controller's estimate
	/// moves, so it must not force an IDR or rebuild the session: a keyframe on
	/// every bandwidth change is exactly the burst a closing uplink can't take.
	///
	/// No default: a backend that can't retune has to say so with
	/// [`Error::BitrateUnsupported`](crate::Error::BitrateUnsupported) rather
	/// than inherit a silent no-op and quietly ignore congestion.
	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;

	/// The encoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}
```

(Verified byte-identical against `git show 3a3e0ea8:rs/moq-video/src/encode/backend/mod.rs`.)

Dispatch is a data-driven candidate table, not a match (`backend/mod.rs:60-102`):

```rust
struct Candidate {
	name: &'static str,
	codecs: &'static [Codec],
	open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
}

const HARDWARE: &[Candidate] = &[
	#[cfg(target_os = "macos")]   Candidate { name: videotoolbox::NAME,   codecs: &[Codec::H264, Codec::H265], .. },
	#[cfg(target_os = "windows")] Candidate { name: mediafoundation::NAME, codecs: &[Codec::H264, Codec::H265], .. },
	#[cfg(all(target_os = "linux", feature = "nvenc"))] Candidate { name: nvenc::NAME, codecs: &[Codec::H264, Codec::H265], .. },
	#[cfg(all(target_os = "linux", feature = "vaapi"))] Candidate { name: vaapi::NAME, codecs: &[Codec::H264], .. },
];
const SOFTWARE: &[Candidate] = &[Candidate { name: openh264::NAME, codecs: &[Codec::H264], .. }];
```

`open(config)` (`backend/mod.rs:106-133`) filters candidates by codec support, orders
them per `Kind` (Auto = hardware then software), and tries each in order; a failed
`open` logs at debug and falls through; exhaustion returns
`Error::NoEncoder("tried, list")`.

### Per-backend encode notes

- **openh264** (`encode/backend/openh264.rs`, 241 lines): vendored/static Cisco
  encoder, always compiled, the universal H.264 fallback. `RateControlMode::Bitrate`,
  `UsageType::CameraVideoRealTime`, GOP via `IntraFramePeriod` (:35-41). Keyframe
  forcing via `force_intra_frame()` (:110-112). Live bitrate retune goes through raw
  `openh264-sys2` FFI (`ENCODER_OPTION_BITRATE` + `SBitrateInfo`, :76-96) because the
  safe wrapper only takes bitrate at construction; a retune before the first frame is
  deferred (openh264 builds its encoder lazily and rejects `SetOption` with
  `cmInitExpected` until then, :143-154). openh264 also rejects a target above the
  opening rate (`cmInitParaError`), which is why the rate policy's ceiling is the
  opening bitrate (test :199-205).
- **videotoolbox** (`encode/backend/videotoolbox.rs`, 483 lines): hand-written on raw
  `objc2-video-toolbox` (`VTCompressionSession`), H.264 + H.265. VT emits
  AVCC/HVCC; the backend rewrites to Annex-B and splices SPS/PPS (+VPS) from the
  format description ahead of every keyframe (:1-14, `annexb_from_sample` :253-256).
  Zero-copy input: `Frame::Surface(surface) => surface.buffer.clone()` — the captured
  `CVPixelBuffer` goes straight into `encode_frame` (:162-166); CPU I420 is uploaded
  into a planar pixel buffer. Synchronous output via `complete_frames` per encode
  (:193-199). `set_bitrate` = live `kVTCompressionPropertyKey_AverageBitRate`, no IDR
  (:214-222). `unsafe impl Send` with a driven-sequentially justification (:66-70).
- **mediafoundation** (`encode/backend/mediafoundation.rs`, 615 lines): enumerates a
  hardware encoder MFT (`MFT_ENUM_FLAG_HARDWARE`) and drives the async-MFT event
  model. `Frame::Texture` runs the MFT on the capture texture's D3D11 device via a
  DXGI device manager, consuming the NV12 surface zero-copy; `Frame::I420` is uploaded
  to a system-memory NV12 sample (:1-14). The MFT natively emits Annex-B with in-band
  parameter sets, so no rewrite is needed (:9-12). D3D device binding is lazy, taken
  from the first texture frame (:62-64). CBR + `CODECAPI_AVEncCommonMeanBitRate`,
  keyframe force via `CODECAPI_AVEncVideoForceKeyFrame`.
- **nvenc** (`encode/backend/nvenc.rs`, 548 lines; Linux, feature `nvenc`): via
  `moq-nvenc` + cudarc, fully dlopen'd (no CUDA toolkit at build). Session input
  format is NV12; low-latency P4 preset, CBR, `frameIntervalP = 1` (no B-frames)
  (:89-102). Three hardware-verified details from #1997 (:10-27): forced keyframes
  use the `FORCEIDR` picture flag (not `pictureType`); `repeatSPSPPS` makes every IDR
  carry in-band SPS/PPS (+VPS for HEVC) so mid-stream joiners can decode
  (:104-126); input is written row-by-row at NVENC's chosen pitch (a flat copy would
  shear). Zero-copy input: a `Frame::Cuda` (NVDEC output, already NV12 in device
  memory) is registered as an external resource
  (`NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR`) and encoded in place — "no CPU round
  trip and no GPU copy" (:177-200). `set_bitrate` = `session.reconfigure(rate)`, in
  place, no IDR (:251-257). A `driver_libs_present()` libloading probe of
  `libcuda.so.1` / `libnvidia-encode.so.1` prevents cudarc's panic-on-missing-driver
  (process abort under `panic = "abort"`) so `Kind::Auto` can fall back on GPU-less
  hosts (:275-294).
- **vaapi** (`encode/backend/vaapi.rs`, 111 lines; Linux, feature `vaapi`): via
  `moq-vaapi 0.0.2` (trimmed cros-libva/cros-codecs), H.264 only. Takes tightly
  packed NV12 (the backend interleaves I420 -> NV12 on the CPU each frame, :95-111 —
  no GPU-surface input path). libva is dynamically *linked*, not dlopen'd: a
  libva-less host fails to load the binary before fallback can happen (#1837 tracks
  making it dlopen) (:6-14). `set_bitrate` returns `Error::BitrateUnsupported`
  because moq-vaapi 0.0.2 has no setter for its private bitrate field (:80-88).
  Header caveat: "NOT YET VALIDATED ON HARDWARE" (:19-21).

### Rate control (#2303) — encode/rate.rs

Commit `821dc951 feat(moq-video)!: adapt the encoder bitrate to the
congestion-control estimate (#2303)`. The mechanism is a pure policy object fed by
`moq_net::bandwidth::Consumer`. Verbatim core (`encode/rate.rs:22-50, 86-92`):

```rust
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Policy {
	/// Fraction of the estimate to target, reserving room for the other tracks
	/// sharing this connection (audio) and for transport overhead. Defaults to
	/// 0.9. ...
	pub headroom: f64,
	/// Upper bound in bits per second, normally the bitrate the caller asked
	/// for. The estimate can only ever take the target *down* from here ...
	pub max: u64,
	/// Lower bound in bits per second. ... Defaults to a tenth of `max`.
	pub min: u64,
	/// Ignore moves smaller than this fraction of the current target ...
	/// Defaults to 0.05 (5%).
	pub hysteresis: f64,
	/// How fast the target may climb back, as a fraction of the current target
	/// per second. Defaults to 0.25 (25%/s ...). Drops ignore this and apply at
	/// once ...
	pub ramp: f64,
}

pub struct Control {
	policy: Policy,
	target: u64,
	applied: Option<Instant>,
}
```

`Control::update(estimate: Option<u64>, now: Instant) -> Option<u64>`
(`rate.rs:116-160`): asymmetric attack/decay — a drop applies immediately
(`desired <= target` branch), a raise is ramp-limited from the last applied change;
hysteresis suppresses <5% moves without resetting the ramp anchor (so suppressed
raises are not starved, :147-154); a `None` estimate holds the current target rather
than resetting to max. `Control::new` starts optimistic at `policy.max`
(:97-103).

Wiring (`encode/producer.rs`): `Options.bandwidth: Option<moq_net::bandwidth::Consumer>`
(:151-160) comes from `Session::send_bandwidth` (or `moq_native::Reconnect::send_bandwidth`).
The capture loop builds `Control::new(Policy::new(encoder_config.resolved_bitrate()))`
per encoder open (:353-356), races `bandwidth.changed()` against frames in a `select!`
(:362-379, retunes "between frames rather than mid-encode"), and `apply_estimate`
(:257-284) calls `Sink::set_bitrate`; a `BitrateUnsupported` backend retires rate
control (drops the consumer so the select arm stops waking), a transient failure keeps
retrying.

Note on #2246 ("upload bitrate from encoder Stats", commit `b03fd264`): this landed
**only in the JS publisher** (`js/publish/src/{audio,video}/encoder.ts`,
`video/index.ts`, `ui/components/stats-tab.ts`) — a `Stats { frames, bytes, keyframes }`
getter aggregated across simulcast renditions. There is no `Stats` type in the Rust
`rs/moq-video` crate (grep confirms zero hits).

### Sink threading (encode/sink.rs)

Off macOS the encoder runs on a dedicated OS thread (`encode/sink.rs:18-25`): the
Windows MFT's COM apartment is per-thread and its encode blocks on MFT events, so the
whole encoder lifetime is confined to one thread, requests (`Encode { frame, keyframe,
resp }` / `SetBitrate`) go down one unbounded mpsc so retunes stay ordered with frames
(:38-54), and `Drop` joins the thread. macOS keeps encoding inline because
VideoToolbox needs no apartment and its zero-copy `CVPixelBuffer` is `!Send`
(:14-16, inline `Sink` at :166-197). `publish_capture`'s future is compile-time
asserted `Send` off macOS (`producer.rs:222-233`).

### Reaching a moq track (encode/producer.rs)

`Producer` holds a per-codec splitter + importer pair (`producer.rs:25-34`):

```rust
enum Codecs {
	H264 { split: moq_mux::codec::h264::Split, import: moq_mux::codec::h264::Import },
	H265 { split: moq_mux::codec::h265::Split, import: moq_mux::codec::h265::Import },
}
```

`Producer::new(broadcast, catalog, codec)` creates a unique track named `*.avc3` /
`*.hev1` and an importer with a catalog reservation (:51-73). `publish(packets,
Timestamp)` runs each Annex-B access unit through `split.decode` + `split.flush` and
`import.decode` (:87-104); the importer parses SPS out of the keyframe and registers
the catalog rendition. `demand()` exposes `moq_net::track::Demand` (:78-83), which is
what drives on-demand capture: `publish_capture` (:183-215) advertises the track up
front, captures until the catalog rendition exists (needs one encoded SPS,
:317-319), then releases the camera whenever the last viewer leaves (`demand.unused()`
in a biased select, :362-379) and reopens on `demand.used()`. Frames are stamped from
a shared `moq_mux::Clock` so audio/video stay aligned (:183-188). A forced IDR is
emitted on the first frame of each (re)open (:344-346).

NOTE: `moq_mux::codec::{h264,h265}::Import` are the video importers reworked by moq-mux
#2425 (shared video-import catalog helper) — see the moq-mux map. The moq-video
producer wiring is unchanged; only the moq-mux internals it calls were refactored.

---

## 2. Decode API

### Backend trait + dispatch (decode/backend/mod.rs)

Verbatim (`decode/backend/mod.rs:36-75`):

```rust
/// The video codec a decoder handles. Derived from the catalog, not chosen by the
/// caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
	H264,
	H265,
	Av1,
}

/// One decoded picture: the raw frame plus its presentation timestamp.
pub(crate) struct Decoded {
	/// Presentation timestamp. Backends that decode one-in one-out echo the input
	/// timestamp; NVDEC threads timestamps through its parser, so they survive
	/// decoder delay and frame reordering.
	pub timestamp: Timestamp,
	/// The decoded picture: CPU I420, or a GPU frame the encode side can consume
	/// without a CPU round trip.
	pub frame: Frame,
}

/// An opened decoder. Feed it prepared access units in decode order; get back
/// zero or more decoded frames ...
pub(crate) trait Backend: Send {
	/// Decode one access unit stamped with its presentation `timestamp`.
	/// `keyframe` marks a random-access frame. Takes an owned [`Bytes`] so a
	/// backend can split codec units without copying.
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Decoded>, Error>;

	/// The decoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}
```

(Verified byte-identical against `git show 3a3e0ea8:rs/moq-video/src/decode/backend/mod.rs`.)

Same candidate-table dispatch as encode (`decode/backend/mod.rs:89-114`):
hardware = VideoToolbox (H264|H265, macOS), MediaFoundation (H264|H265, Windows),
NVDEC (H264|H265|Av1, Linux + `nvdec` feature); software = openh264 (H264 only).
`open(codec, config)` skips non-supporting candidates and distinguishes "none support
H.265" from "all tried failed" in the `NoDecoder` message (:119-145).

### Decoder front end (decode/decoder.rs)

`Kind` mirrors the encode enum (Auto/Hardware/Software/Named, `decoder.rs:24-37`).
`Config` (`decoder.rs:43-58`):

```rust
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Which backend to use.
	pub kind: Kind,
	/// Upper bound on buffering before a stalled group is skipped. `None` uses
	/// the moq-mux default (skip aggressively) ... Forwarded to the container
	/// consumer's `with_latency`.
	pub latency_max: Option<Duration>,
	/// Ask the decoder to emit frames at this size ... Best effort: a hardware
	/// decoder with a built-in scaler (NVDEC) honors it for free, other backends
	/// ignore it. ...
	pub resize: Option<Size>,
}
```

`Decoder::new(&VideoConfig, &Config)` maps the hang catalog codec to a backend
`Codec` and a payload `Conversion` (`decoder.rs:94-140`): avc3/hev1 (in-band Annex-B)
and AV1 OBU temporal units pass through; avc1/hvc1 parse avcC/hvcC descriptions and
convert length-prefixed NALs to Annex-B, prepending parameter sets ahead of keyframes
(`Conversion::LengthPrefixed { length_size, keyframe_prefix }`, :68-76). AV1 is
gated to 8-bit 4:2:0 non-monochrome (`is_supported_av1`, :187-189). Output is gated
until the first keyframe (:155-160). `decode(&Bytes, Timestamp, keyframe) ->
Result<Vec<Frame>>` (:152-184) wraps each backend `Decoded` in the public
`decode::Frame`.

Public output type (`decode/mod.rs:36-46`), verbatim:

```rust
pub struct Frame {
	/// Presentation timestamp, carried through from the container. It rides out of
	/// the decoder with each picture, so a reordered frame (B-frames) keeps its own
	/// time rather than the input access unit's.
	pub timestamp: Timestamp,
	/// The decoded resolution, which is [`Config::resize`] when the backend
	/// honored it and the stream's native size otherwise.
	pub size: Size,
	/// The pixels: CPU I420 or a GPU surface.
	pub(crate) inner: crate::frame::Frame,
}
```

`Frame::resize(Size)` scales on the GPU for CUDA frames (with a warn-once CPU
fallback if the PTX is rejected) and on the CPU otherwise (`mod.rs:57-85`);
`Frame::into_i420() -> Bytes` downloads GPU frames (`mod.rs:94-101`). A compile-time
test pins `Frame: Send + Sync` and `Consumer: Send` on every platform
(`mod.rs:111-118`; the macOS fix is #2162 `eb7d2867`, the `Sync` requirement for
moq-transcode's `Arc<decode::Frame>` fanout is #2225 `608ee81f`).

Timestamps are `moq_net::Timestamp` end to end since #2146 (`c6908c43`).

### Track consumption (decode/consumer.rs)

`Consumer` = `Decoder` + a moq-mux container consumer + a pending queue
(`consumer.rs:15-22`):

```rust
pub struct Consumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,
	pending: VecDeque<Frame>,
}
```

`Consumer::new(&broadcast::Consumer, &VideoConfig, name, Config)` subscribes to the
track with the catalog video priority and applies `latency_max` via `with_latency`
(:27-50). `read() -> Result<Option<Frame>>` loops: drain pending, else read one mux
frame (`payload`, `timestamp`, `keyframe`) and decode (:58-73). Direct mirror of
`moq_audio::decode::Consumer`.

NOTE: the `moq_mux::container::Consumer` this wraps got the #2426 empty-batch contract
clarification (a wire frame may decode to zero media frames; only `None` ends the
group). moq-video's `read` loop already keys completion off `None`, so it is
unaffected — but see the moq-mux map before writing a new consumer against that trait.

### Per-backend decode notes

- **openh264** (`decode/backend/openh264.rs`, 81 lines): H.264 only, Annex-B in,
  strided `DecodedYUV` repacked to tight I420 (`I420::from_planes`). Echoes the
  input timestamp (one-in/one-out).
- **videotoolbox** (`decode/backend/videotoolbox.rs`, 438 lines; #1859 `27ca3e7a`
  added H.265): `VTDecompressionSession`, hand-written objc2 bindings. Pulls
  VPS/SPS/PPS out of the Annex-B stream, builds a `CMVideoFormatDescription`
  (rebuilding the session when parameter sets change, :62-78), repackages slices as
  AVCC/HVCC in a `CMSampleBuffer`, requests NV12 output and downloads to packed I420
  (:1-18) — macOS decoded frames are always CPU I420 (`Sink { frames: Vec<I420> }`,
  :56-60). Decoding is synchronous (callback fires inside `decode_frame`), which is
  what keeps the `!Send` CF handles thread-confined (:16-18).
- **mediafoundation** (`decode/backend/mediafoundation.rs`, 448 lines; #1854
  `42949ea9` added HEVC): the Microsoft decoder MFT driven synchronously
  (`MFT_ENUM_FLAG_SYNCMFT`) with a D3D11 device manager bound, routing decode to
  DXVA (NVDEC/Intel/AMD under the hood); requires the D3D device, so GPU-less hosts
  fail `open` (H.264 then falls back to openh264; H.265 has no fallback) (:4-13).
  H.265 needs an HEVC decoder MFT present (HEVC Video Extensions) (:10-12). Output
  NV12 textures from the DXVA pool are downloaded and deinterleaved to `Frame::I420`
  (:391-393) — Windows decode output is CPU I420, not a retained texture. Picture
  size is learned from the first output-type negotiation (:18-22).
- **nvdec** (`decode/backend/nvdec.rs`, 706 lines; #2145 `5fa30c72` introduced it,
  #2178 `7a9d1844` added AV1): cuvid parser + decoder via moq-nvenc's dlopen'd cuvid
  table; same `driver_libs_present` probe as NVENC (:100-108). H.264/H.265/AV1
  8-bit 4:2:0 only (`on_sequence` rejects other chroma/bit depth, :217-226).
  Parser driven synchronously: each AU pushed with `CUVID_PKT_ENDOFPICTURE`,
  `ulMaxDisplayDelay: 0`, `ulClockRate: 1_000_000` so timestamps ride the parser in
  microseconds and survive reordering (:135-148, :173-183). The sequence callback
  (re)creates the decoder on geometry/crop changes and maps `Config::resize` to
  cuvid's `ulTargetWidth/Height` — **hardware scaling during decode, free**
  (:284-311). `map_frame` copies each mapped surface device-to-device into an owned
  `cuda::Frame` (the fixed surface pool must be released promptly), producing
  `Frame::Cuda` that NVENC registers directly (:339-392). Decoder uses the device's
  primary CUDA context, the same one NVENC retains, "so frames pass between them
  without a copy" (:122-124).

---

## 3. Capture

### Abstraction (capture/mod.rs)

Public source selection + enumeration (#2293 `9090a68b`), verbatim core
(`capture/mod.rs:65-93`):

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
	/// A camera / webcam. `None` opens the default camera.
	Camera(Option<String>),
	/// A whole display. `None` opens the main display.
	Display(Option<String>),
	/// A single window, by the id [`windows`] reports. macOS only.
	Window(String),
	/// Every window belonging to one application ... macOS only.
	App(String),
}
```

Enumeration types `Camera { id, name }`, `Display { id, name, width, height }`,
`Window { id, title, app, width, height }`, `App { id, name }` each with a
`.source()` builder (:117-190). The free functions `cameras()` / `displays()` /
`windows()` / `apps()` are **macOS-only today** (every other platform returns
`Error::Unsupported`; on Linux the xdg portal picker owns display selection)
(:365-412).

`Config` (:197-220): `source`, `width/height/framerate: Option<_>` (hints; backend
picks the closest mode), `cursor: bool` (default true). `#[non_exhaustive]`.

The internal stream handed to the encode loop (`capture/mod.rs:230-242`):

```rust
pub(crate) struct FrameStream {
	chan: Arc<FrameChannel>,
	width: u32,
	height: u32,
	framerate: Option<u32>,
	device: String,
	/// First frame captured during [`open`] (some backends learn their geometry
	/// only from a frame); returned by the first [`read`](Self::read).
	pending: Option<Frame>,
	/// Keeps the backend alive and releases it on drop. Type-erased because it
	/// differs per platform (objc session + delegate, or pump-thread guard).
	_backend: Keepalive,
}
```

`read().await -> Option<Frame>` is cancel-safe; dropping the stream releases the
device (:222-229). `Keepalive = Box<dyn Any + Send>` off macOS, `Box<dyn Any>` on
macOS (`AVCaptureSession` is `!Send`), which is why only macOS's publish future is
`!Send` (:20-30). `open(config)` dispatches per `Source` variant per platform
(:294-363). Capture itself is crate-private: only `Config`/`Source`/enumeration are
public; the way in is `encode::publish_capture`.

### Plumbing

- **channel.rs**: `FrameChannel` — a hand-rolled bounded (DEPTH = 4) MPSC with
  `Mutex<VecDeque<Frame>> + Notify`; `push` from foreign threads drops the **oldest**
  frame when full (latency over completeness, :18-57); async `recv` uses the
  register-before-check Notify pattern and is cancel-safe (:66-82).
- **pump.rs**: bridges blocking pull-style devices (V4L2, MF source reader) to the
  channel. `spawn(chan, init, read)` builds the device **on** the thread (so `!Send`
  handles like `IMFSourceReader` are fine), reports `Geometry { width, height,
  framerate, device }` over a oneshot, loops `read` until a stop flag; `PumpGuard`
  stops + joins on drop so the device fd is closed before a reopen (:24-111).

### Per-platform backends

- **avfoundation.rs** (macOS camera, 244 lines): `AVCaptureVideoDataOutput` delegate
  wraps each IOSurface-backed `CVPixelBuffer` as `Frame::Surface` — "Frames reach
  VideoToolbox with no copy and no color conversion" (:1-6). Handles TCC permission
  prompts (60s access timeout) and a 5s first-frame timeout. `cameras()` enumerates
  via `AVCaptureDevice::devicesWithMediaType` and returns `uniqueID`s (:38-54).
- **screencapture.rs** (macOS display/window/app, 434 lines): ScreenCaptureKit
  `SCStream` delivering the same zero-copy NV12 `CVPixelBuffer`s; the three sources
  differ only in `SCContentFilter` (:1-14). `displays`/`windows`/`apps` enumeration
  via `SCShareableContent`; window listing filters to layer 0 (:40-43).
  `capture/surface.rs` is the shared `CMSampleBuffer -> Frame::Surface` extraction
  (12-19).
- **v4l2.rs** (Linux camera, 204 lines): `v4l` crate MMAP streaming on the pump
  thread; YUYV resampled (`I420::from_yuyv`) or MJPEG decoded with pure-Rust
  `zune-jpeg` then `I420::from_rgb`. "This is the CPU path feeding NVENC / VAAPI /
  openh264; there's no GPU surface here" (:1-7). Replaced nokhwa.
- **pipewire.rs** (Linux screen, 581 lines, feature `pipewire`): xdg-desktop-portal
  ScreenCast (ashpd) negotiates; the compositor picker owns source selection; a
  dedicated thread runs the PipeWire main loop converting BGRx/BGRA shm buffers to
  CPU I420 (`I420::from_bgra`). Portal restore token cached process-wide so
  demand-driven reopen skips the picker; a damage-driven compositor is re-paced by
  re-emitting the last frame each interval (:1-19, :50-53). No dmabuf import — CPU
  path only.
- **mediafoundation.rs** (Windows camera, 403 lines): `IMFSourceReader` with a D3D11
  DXGI manager + advanced video processor; each sample arrives as a GPU NV12 texture
  (`Frame::Texture`) consumed zero-copy by the encoder MFT; GPU-less hosts fall back
  to the software video processor emitting CPU I420 (:1-9). Device selection via
  `MFEnumDeviceSources` friendly names / index.
- **desktopduplication.rs** (Windows screen, 351 lines): DXGI Desktop Duplication;
  BGRA desktop texture -> CPU staging -> `I420::from_bgra`. Whole-monitor only;
  paced re-emission for static screens; runs on the pump thread since
  `AcquireNextFrame` blocks and `IDXGIOutputDuplication` is `!Send` (:1-14). Not
  zero-copy into the encoder (screen path converts on CPU).

---

## 4. Frame model + zero-copy

### The raw frame enum (frame.rs)

Verbatim (`frame.rs:23-36`):

```rust
pub(crate) enum Frame {
	/// Zero-copy GPU surface (macOS `CVPixelBuffer`).
	#[cfg(target_os = "macos")]
	Surface(macos::Surface),
	/// Zero-copy GPU texture (Windows Direct3D11 NV12).
	#[cfg(target_os = "windows")]
	Texture(d3d11::Texture),
	/// Zero-copy GPU buffer (Linux CUDA NV12). Produced only by the NVDEC
	/// decoder, consumed in place by the NVENC encoder.
	#[cfg(all(target_os = "linux", feature = "nvdec"))]
	Cuda(cuda::Frame),
	/// CPU-resident planar I420.
	I420(I420),
}
```

(Verified byte-identical against `git show 3a3e0ea8:rs/moq-video/src/frame.rs`.)

`Frame::to_i420() -> Cow<'_, I420>` downloads GPU variants only on demand
(:63-74). The CPU type (`frame.rs:79-85`):

```rust
/// A raw video frame in planar I420 (YUV 4:2:0), tightly packed (no padding),
/// at the encoder resolution. Width and height are even (chroma is 2x2).
#[derive(Clone)]
pub(crate) struct I420 {
	pub width: u32,
	pub height: u32,
	/// Y plane (`width * height`) then U then V (`width/2 * height/2` each).
	pub data: Vec<u8>,
}
```

Pixel-format converters on `I420`, all via the `yuv` crate (BT.601 limited range,
replacing ffmpeg swscale): `from_rgba` (:98-110), `from_bgra` (Windows DD + Linux
PipeWire, :117-131), `from_planes` (strided decoder output, :138-163), `from_rgb`
(MJPEG, :168-182), `from_yuyv` (V4L2 raw, :188-201), `from_nv12` (Windows MF,
:208-225), plus `interleave_uv`/`deinterleave_uv` helpers (:316-331) and a
SIMD bilinear `resize` using a thread-local `fast_image_resize::Resizer` (:230-278).

### GPU surface types

**macOS** (`frame.rs:353-357` + #2225):

```rust
/// A captured GPU surface. Cloning is a cheap retain (no pixel copy), which
/// is what keeps the capture -> encode path zero-copy.
pub(crate) struct Surface {
	pub(crate) buffer: CFRetained<CVPixelBuffer>,
	pub(crate) width: u32,
	pub(crate) height: u32,
}
```

`unsafe impl Send/Sync for Surface` (:368-369) with a long safety comment
(:359-367): retain/release are thread-safe, all `&self` access is read-only;
`Sync` exists specifically because moq-transcode shares `Arc<decode::Frame>` (#2225,
commit `608ee81f`). `download_i420` locks read-only and copies honoring per-plane
stride (:377-429).

**Linux CUDA** (`frame.rs:501-515`, #2145):

```rust
/// A GPU NV12 frame in CUDA device memory: NVDEC's output and NVENC's
/// zero-copy input. One buffer holds both planes at a shared row `pitch`:
/// `height` luma rows, then `height / 2` interleaved-UV rows. Cloning bumps
/// refcounts (no pixel copy), which keeps decode -> encode on the GPU.
///
/// Both codecs use the device's primary CUDA context (`CudaContext::new`
/// retains it), so a frame decoded by NVDEC is directly addressable by NVENC.
#[derive(Clone)]
pub(crate) struct Frame {
	buf: Arc<Buffer>,
	pub(crate) width: u32,
	pub(crate) height: u32,
	/// Row pitch in bytes of both planes (>= `width`).
	pub(crate) pitch: u32,
}
```

The backing `Buffer` deliberately uses plain `cuMemAlloc` because NVENC's resource
registration rejects stream-ordered pool memory (`cuMemAllocAsync`, what cudarc's
`CudaSlice` uses) (:481-489). `device_ptr()` exposes the raw pointer for NVDEC's
copy destination / NVENC's registration (:541-545). `resize` runs the vendored
box-filter kernels (`frame/nv12_resize.cu`, compiled offline to
`frame/nv12_resize.ptx`, embedded via `include_str!` and driver-JIT'd so no CUDA
toolkit is needed — :450-479, cu file :9-13); destination pitch is aligned to 256
(NVENC-friendly), and the stream is synchronized before return because NVENC does
not order against it (:594-658). Kernels: `resize_luma` / `resize_chroma`, one
thread per destination pixel / UV pair, box-average over the full source box so
arbitrary downscales don't alias (cu :1-7).

**Windows D3D11** (`frame.rs:716-729`):

```rust
/// A captured GPU texture (NV12) on the Media Foundation source reader's
/// Direct3D11 device. Holds the device so the download fallback and the
/// hardware encoder run on the same device that owns the texture. Cloning the
/// COM handles is a cheap `AddRef`, which is what keeps capture -> encode
/// zero-copy.
pub(crate) struct Texture {
	pub(crate) device: ID3D11Device,
	pub(crate) texture: ID3D11Texture2D,
	/// The texture-array slice this frame lives in. Media Foundation pools the
	/// reader's output as one texture array and reports the index per sample.
	pub(crate) subresource: u32,
	pub(crate) width: u32,
	pub(crate) height: u32,
}
```

`download_i420` (the DXVA NV12 offset fix, #2034 `6684226e`, `frame.rs:791-796`):
the UV plane begins after the *texture's* Y plane spanning the **allocated** height,
not the display height — "A DXVA decode pool allocates textures at the coded size
(e.g. 1088 rows for a 1080p display), so keying the offset off `self.height` would
read chroma from inside the still-luma padding rows and produce garbage color."
The shared multithread-protected `d3d11::create_device` (:689-714) underpins MF
capture, MF encode, and DXVA decode; `mf.rs` adds the `IMFDXGIDeviceManager` pairing
and a refcounted `ComGuard` (MTA CoInitializeEx + MFStartup) (`mf.rs:37-82`).

### Zero-copy paths, complete inventory

| Path | Mechanism | Evidence |
|---|---|---|
| macOS capture -> VT encode | AVFoundation/SCK `CVPixelBuffer` (IOSurface NV12) fed directly to `VTCompressionSession`; clone = retain | `capture/avfoundation.rs:1-6`, `encode/backend/videotoolbox.rs:162-166`, test `encoder.rs:530-554` |
| Windows capture -> MF encode | Source reader NV12 texture on a shared D3D11 device; encoder MFT bound to the same device via DXGI manager; clone = AddRef | `capture/mediafoundation.rs:1-9`, `encode/backend/mediafoundation.rs:1-7`, test `encoder.rs:659-697` |
| Linux NVDEC -> NVENC transcode (#2145) | Decoded NV12 stays in CUDA device memory (`Frame::Cuda`); NVENC registers the device pointer as an external resource and encodes in place; both use the device's primary context | `decode/backend/nvdec.rs:8-15`, `encode/backend/nvenc.rs:177-200`, `frame.rs:501-507` |
| Linux GPU resize in the transcode path | Either free (cuvid target size at decode, `Config::resize` -> `ulTargetWidth/Height`) or the CUDA box-filter kernel staying in device memory | `nvdec.rs:296-311`, `frame.rs:587-659`, `decode/mod.rs:57-73` |
| Cross-thread / cross-task GPU frame sharing (#2225, #2162) | `Surface: Send + Sync`, `cuda::Frame: Clone` (Arc), `Texture: Clone` (AddRef); `decode::Frame: Send + Sync`, pinned by a compile-time test | `frame.rs:359-369`, `decode/mod.rs:104-118` |

Not zero-copy: VAAPI encode input (CPU NV12 interleave), all Linux/Windows *screen*
capture (BGRA -> CPU I420), macOS and Windows *decode* output (downloaded to I420 —
only NVDEC keeps decoded frames on the GPU), and there is no decode -> render GPU
handoff API at all: the only public escape hatches are `decode::Frame::into_i420()`
and `Encoder::encode(&decode::Frame)`.

---

## 5. Codec coverage matrix (derived from the candidate tables)

Encode (`encode/backend/mod.rs:68-102`):

| Codec | macOS | Windows | Linux | Software (all platforms) |
|---|---|---|---|---|
| H.264 | VideoToolbox | Media Foundation MFT | NVENC, VAAPI | openh264 (always compiled) |
| H.265 | VideoToolbox | Media Foundation MFT | NVENC | none (hardware-only) |
| AV1 | — | — | — | — |
| VP9 | — | — | — | — |

Decode (`decode/backend/mod.rs:89-114`, `decode/mod.rs:9-12`):

| Codec | macOS | Windows | Linux | Software |
|---|---|---|---|---|
| H.264 | VideoToolbox (#1859 infra) | MF/DXVA | NVDEC | openh264 |
| H.265 | VideoToolbox (#1859) | MF/DXVA + HEVC MFT (#1854) | NVDEC | none |
| AV1 | — | — | NVDEC only (#2178), 8-bit 4:2:0 non-mono (`decoder.rs:187-189`) | none |
| VP9 | — | — | — | — |

AV1 is decode-only (no encoder anywhere). VP9 appears nowhere in the crate (the only
grep hit is a historical note in `DESIGN-native-codecs.md`). Public encode `Codec`
enum = {H264, H265}; internal decode `Codec` enum = {H264, H265, Av1}.

---

## 6. Dependencies + features (Cargo.toml)

**ffmpeg is fully gone.** No `ffmpeg-next` anywhere; the replacement stack is:
`openh264` 0.9.3 + `openh264-sys2` (vendored static software H.264, always on),
`yuv` 0.8.14 (RGBA->I420, "replaces ffmpeg swscale"), `fast_image_resize` 5 (CPU
resize), `zune-jpeg` (MJPEG, "no libjpeg"), `v4l` 0.14 ("replaces nokhwa"), and
per-platform native APIs (objc2-* on macOS incl. `objc2-video-toolbox` /
`objc2-screen-capture-kit`; `windows` 0.62 on Windows; `cudarc` 0.19 + in-tree
`moq-nvenc` + `moq-vaapi` 0.0.2 + `pipewire`/`ashpd` on Linux). moq deps: `hang`
(catalog types), `moq-mux`, `moq-net`.

Features (Linux-only in effect; #1860 `6868f967` made hardware codecs default-on
opt-out):

```toml
default = ["nvenc", "vaapi", "nvdec"]
nvenc = ["dep:cudarc", "dep:moq-nvenc", "dep:libloading"]
nvdec = ["dep:cudarc", "dep:moq-nvenc", "dep:libloading"]
vaapi = ["dep:moq-vaapi"]
pipewire = ["dep:pipewire", "dep:ashpd"]   # off by default (links libpipewire at build)
```

Runtime-dependency character differs per feature (all documented inline in
Cargo.toml): `nvenc`/`nvdec` are fully dlopen'd (cudarc `fallback-dynamic-loading` +
`cuda-12020` pin, pre-built PTX so libnvrtc is never loaded) — a GPU-less build links
and a driverless host falls back; `vaapi` hard-links libva.so.2 (a libva-less host
fails to *load*; #1837 tracks dlopen); `pipewire` needs pkg-config + libclang at
build. macOS/Windows backends are cfg-gated by target, not features.

---

## 7. Historical note: the pre-merge ffmpeg crate is gone

Before the dev->main merge on 2026-07-21, `main`'s `rs/moq-video` was a different, far
smaller crate: a thin ffmpeg wrapper (`capture.rs` + `encode/{mod,encoder,producer}.rs`
+ `error.rs` + `lib.rs`, 5 source files) with a single `ffmpeg-next = { version = "8" }`
dependency covering H.264 encode + libavdevice capture, encode-only and H.264-only, no
decode module, no GPU frame types, everything through CPU swscale. **That crate no
longer exists** — the merge replaced it wholesale with the 41-file native stack mapped
above. What survived shape-wise: the `Kind` selection enum, the `#[non_exhaustive]`
Config pattern with the same ~0.07 bpp default bitrate and 2s GOP,
`Producer`/`publish_capture` with demand-driven on-demand capture, and the "no backend
types in the public API" stability rule. This section is retained only so a reader
who remembers the old ffmpeg crate knows it is superseded; there is no live
main-vs-dev split anymore.

---

## 8. Cross-cutting design notes worth stealing / knowing

- Both encode and decode dispatch through a flat `Candidate` table (name + supported
  codecs + `open` fn pointer), platform/feature-gated at the table entry, with
  try-in-order fallback and a `tried` list in the error. Cheap to extend.
- Backend trait objects are `Box<dyn Backend>` where `Backend: Send`; several
  concrete backends are `!Send` internally and use documented `unsafe impl Send`
  justified by thread confinement (VT: driven sequentially; MF: dedicated encode
  thread; VAAPI: encode thread; NVDEC/NVENC: context rebound per call).
- Threading policy is explicit and per-platform: dedicated OS thread for COM/MFT
  encoders and blocking capture devices (`encode/sink.rs`, `capture/pump.rs`),
  inline on macOS because the zero-copy surface is `!Send`.
- The keyframe contract runs the whole stack: `encode(frame, keyframe: bool)` forces
  an IDR; every backend guarantees in-band parameter sets on every IDR (repeatSPSPPS
  on NVENC, format-description splice on VT, native MFT behavior); decode gates
  output until the first keyframe and re-injects out-of-band parameter sets for
  avc1/hvc1.
- Rate control is cleanly layered: backend = mechanical `set_bitrate` (no IDR);
  `rate::Control` = pure policy (attack/decay/hysteresis/ramp, time injected);
  `producer` = plumbing from `moq_net::bandwidth::Consumer`, with per-open reset and
  graceful retirement for backends that can't retune.
- Frames flow one way: capture (Surface/Texture/I420) -> encoder; decoder
  (Cuda/I420) -> `Encoder::encode` or `into_i420`. There is no shared "video frame"
  public type between capture and decode — `crate::frame::Frame` is the private
  union, and the public decode `Frame` wraps it with timestamp + size.
