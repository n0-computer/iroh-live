# Review: moq no-regression + per-segment transcoding / FETCH alignment

Scope: audits the upstream plan set against (1) moq no-regression — every plan
that edits moq's own code must be strictly additive — and (2) the maintainer's
stated codec direction: *"per segment transcoding with FETCH support is the goal
/ so 1080p -> 360p for group 45, which means rate control needs to be pretty
custom"*, source *"relay memory, possibly disk too"*. Verified against
`/home/bit/Code/rust/moq` at HEAD `3a3e0ea8` and the plans under
`plans/upstream/`.

## Verdict

**No-regression: PASS.** Every moq-editing plan preserves the existing capability
it touches. B2 keeps all five encode backends working and keeps `rate::Control`
untouched; vaapi-encode replaces a placeholder that carries no behavior moq relies
on; vtb-mf-decode-surface keeps the `into_i420()` CPU path; opus-improvements is
additive to moq-audio's rate-snapping/validation/frame-duration logic. One item to
watch, not a regression: B2 changes the public `Encoder::encode_rgba/encode_i420/
encode` signatures (adds a `timestamp` arg) — behavior-preserving but a public
ripple already flagged in the plan.

**Per-segment-FETCH-transcoding compatibility: MOSTLY.** The plans are
architecturally aligned and the encoder/decoder contributions are directly usable
by moq-transcode with no integration work, because moq-transcode drives encoders
only through the public `moq_video::encode::{Kind, Config, Encoder}` front-end and
selects them by `Kind` — exactly the seam our in-tree backends plug into. The
adaptations needed are notes, not redesigns: the encoder plans must (a) state that
per-segment rate control is owned by moq-transcode (a fixed per-rung CBR target set
via `Config.bitrate` at construction, plus a future custom per-GOP target), not by
a streaming rate controller the backend owns, and (b) honestly characterise the
per-group *reconfigure/re-open* cost, since the FETCH path builds a **fresh
encoder per group**.

Top adaptations: (1) add coordination point #7 (below); (2) vaapi-encode,
v4l2-encode, av1-software each add a "per-segment note" that they honor
`Config.bitrate` as a per-encode CBR target, force IDR per GOP, and expose cheap
reconfigure; (3) v4l2-encode must flag that a full device re-open per fetched group
is expensive and offer a reset/reuse path; (4) vaapi-decode must state that its
`Config::resize` decode-time GPU scale is the Intel/AMD analog of NVDEC's free
scaling that the transcode fanout depends on.

---

## PART 1 — moq no-regression audit

The moq codec direction is set by `rate::Control` (encode/rate.rs, #2303) and the
congestion-estimate bitrate adaptation wired only into the **capture publish** path
(`encode/producer.rs:356`, `Control::new(Policy::new(...))`). moq-transcode does
**not** use `rate::Control` at all (grep confirms zero uses outside
producer.rs/rate.rs). Any plan must leave both mechanisms intact.

**B2 (pts-through-encode) — PASS.** Adds a `timestamp` arg and returns
`Vec<Packet>` from `Backend::encode`/`finish`; every existing backend echoes the
current frame's timestamp, so behavior is unchanged for the five zero-delay
backends (openh264, videotoolbox, mediafoundation, nvenc, vaapi). It does not touch
the `Candidate` tables, `rate::Control`, `Policy`, `set_bitrate`, or the congestion
wiring in `producer.rs:356`. `Producer::publish(Vec<Bytes>, Timestamp)` stays
unchanged. The only non-additive surface is the public `Encoder::encode_*`
signature change, which the plan itself calls out for the base RFC. No regression;
in fact `Packet`+per-frame timestamp + `finish()` draining is *required* by the
transcode fetch path (see Part 2). Verified `Backend` trait at
`encode/backend/mod.rs:37-57`.

**B4 (public Backend + registration) — PASS (breaking, gated).** Publishes the
`pub(crate)` traits as additive-sealed and converts the `const HARDWARE/SOFTWARE`
tables to `OnceLock<Vec<Candidate>>` seeded from the built-ins first, so priority
order is unchanged when nothing is registered. `Kind` is not changed. All five
encode backends and all four decode backends remain in the seed. The only breaking
aspect is publishing the trait (semver surface), correctly marked `!` and gated on
the Android decision. The decode-side non-mirror (`supports` predicate + codec-
taking opener) is respected, so NVDEC-serves-three-codecs still works. No capability
removed.

**vaapi-encode (REPLACES moq's vaapi backend) — PASS.** moq's current backend is a
111-line CPU-only placeholder whose own header says "NOT YET VALIDATED ON
HARDWARE", whose `set_bitrate` returns `BitrateUnsupported`, and which has no GPU
input path (`encode/backend/vaapi.rs:16-21,80-111`). The replacement is a strict
superset: DMA-BUF zero-copy input, VPP GPU scale/convert, an *honest* `set_bitrate`
(real retune or `BitrateUnsupported`, never a silent no-op), and per-frame forced
IDR. It keeps the existing candidate-table row (H.264 only), keeps the CPU I420
upload as the non-DMA-BUF fallback, and keeps the `unsafe impl Send` thread-
confinement justification. Nothing moq relies on is dropped: the placeholder had no
real behavior to preserve. Watch: keep the `set_bitrate`-forces-no-IDR contract
(the plan does).

**vtb-mf-decode-surface (changes VT/MF decoders) — PASS.** Changes VideoToolbox and
Media Foundation decode to retain their GPU surface (`Frame::Surface` /
`Frame::Texture`) instead of downloading to I420. The plan explicitly preserves the
`into_i420()` CPU path (implementation step 4: "download the retained surface or
texture on demand"), so the container/CPU consumers (moq-mux `container::Consumer`,
anything calling `decode::Frame::into_i420()`) keep working, and pay the download
only when they ask. It preserves the #2034 DXVA allocated-height offset fix for the
fallback and preserves the `decode::Frame: Send + Sync` compile-time pin. No
candidate-table edit. No regression.

**opus-improvements (changes moq-audio Opus) — PASS.** Additive to moq-audio's
existing concrete `Encoder`/`Decoder`: adds runtime `set_bitrate`, real lookahead-
derived OpusHead pre-skip (replacing a hardcoded 0 — a correctness *fix*, not a
regression), FEC/DTX ctls behind config fields defaulting off, and channel remix.
It explicitly *adopts and leaves in place* moq's `pick_rate` rate-snapping,
`validate_rate`/`validate_channels`, and `FRAME_DURATIONS` variable frame durations
(`opus.rs:13-51`) — "no change needed" to `opus.rs`. The one cross-crate touch is an
additive pre-skip field on moq-mux's opus `Config`, with `encode_rejects_multichannel`
kept intact. Application mode becomes a config field rather than silently flipping
moq's `OPUS_APPLICATION_AUDIO`. No regression.

**Regression risks found:** none that remove a moq feature. Two things to keep on
the acceptance gate: (1) B2's public `Encoder::encode_*` signature change must be
approved in the base RFC (already noted); (2) every encoder plan must preserve the
"`set_bitrate` never forces an IDR / never silently no-ops" contract from
`encode/backend/mod.rs:53` — all current plans state this.

---

## PART 2 — per-segment transcoding + FETCH alignment

**How moq-transcode drives an encoder (the integration seam), from source.**
`rung.rs` builds encoders only through the public front-end:
`Encoder::new(&Config)` with `config.kind = self.encoder` (a
`moq_video::encode::Kind`), `config.bitrate = Some(rung.bitrate)` (a fixed per-rung
CBR target), and `config.gop = framerate*8` as a *backstop* only. Keyframes are
forced at every group boundary via the `keyframe` argument to
`encoder.encode(&frame, keyframe)`. Two demand paths (`rung.rs`):

- **Live** (`live`, rung.rs:98-214): one encoder per demand session, "rate control
  persists across groups, while every group still opens with a forced IDR".
- **Fetch** (`fetch` -> `Pipeline::new` -> `transcode_group_inner`, rung.rs:223-430):
  a **fresh decoder+encoder per fetched group**, encodes the isolated GOP starting
  with a forced IDR (`keyframe = frame.keyframe || first`), then `pipeline.finish
  (last_timestamp)` drains the tail. Bounded by `MAX_CONCURRENT_FETCHES = 4`
  because "hardware encoders expose only a few simultaneous sessions".

Crucially, moq-transcode never calls `set_bitrate` and never touches
`rate::Control` — it owns rate control entirely by setting the target at
construction. The maintainer's "rate control needs to be pretty custom" therefore
means: **moq-transcode is the rate-control owner**, and the custom part (hit a
target size/bitrate for one isolated GoP of group 45) is future policy that will
also live in moq-transcode, driven through the encoder `Config` / a future per-GOP
knob — not inside our backends. The FETCH primitives are real and stable:
`track::Consumer::fetch_group` / `group::Fetch` (moq-net track.rs:1454,
group.rs:752) and `track::Dynamic::requested_group` (track.rs:1125); the source
group is held in relay memory (moq-net cached groups) with disk as a future tier.

### 1. Alignment — where our contributions HELP the goal

- **VAAPI decode (DMA-BUF export) + VPP GPU scale + VAAPI encode is the Intel/AMD
  zero-copy `decode -> scale -> encode` analog of NVDEC -> NVENC.** moq-transcode's
  fanout is built on decode-once, then per-rung GPU resize + encode keeping "a GPU
  frame on the GPU" (feed.rs, rung.rs `Pipeline::process`). Today only NVDEC ->
  NVENC gets this on Linux (`Frame::Cuda`, cuvid `ulTargetWidth/Height` free scale,
  `RegisteredResource` in-place encode). Our vaapi-decode (`Frame::DmaBuf` +
  `Config::resize` hardware scale) and vaapi-encode (DMA-BUF surface import + VPP
  scale) give the *same shape* to every Intel/AMD relay: the decoder emits a GPU
  DMA-BUF at the rung size, the encoder imports it without a CPU round-trip. This
  makes moq-transcode GPU-accelerated on non-NVIDIA relays, which it cannot be
  today. Directly usable because moq-transcode selects the decoder/encoder by
  `Kind` (`config.encoder`/`config.decoder`) and our backends register as the
  built-in `vaapi` rows.
- **rav1e/rav1d give software transcode for HW-less hosts.** moq-transcode falls
  back to openh264 for H.264 when no hardware is present; av1-software extends the
  same hardware-then-software fallback to AV1 (the only AV1 encode anywhere, plus
  the only software AV1 decode). A CPU-only relay can then transcode an AV1 ladder,
  or transcode an H.264/H.265 source down to AV1 rungs.
- **Forced-IDR-per-segment fits per-group transcoding exactly.** Every encoder plan
  ships per-frame forced IDR through the `keyframe` argument. That is precisely what
  both transcode paths need: the live path opens every group with a forced IDR, and
  the fetch path forces IDR on the first frame of the isolated GOP. Our encoders'
  independently-decodable-GOP behavior maps 1:1 onto the "output group N mirrors
  source group N" contract.

### 2. Collision / adaptation — where our plans must change

The one real collision is the **rate-control ownership model**. Our
iroh-live encoders and moq's `rate::Control` are both *continuous-stream* designs:
adapt bitrate over a live stream toward a congestion estimate. Per-segment FETCH
transcoding is the opposite — hit a target for **one isolated GoP**, with no
streaming history and no congestion loop. moq-transcode already owns this by
construction, so the rule for our contributions is: **the encoder Backend exposes
per-segment rate-control primitives; it must not embed or assume a streaming rate
controller.** Concretely the Backend/front-end must expose, and our plans must NOT
bake in continuous-stream-only assumptions around:

1. **Honest immediate `set_bitrate`, no IDR side effect** — already the moq
   contract (`encode/backend/mod.rs:53`). Keep it; do not let any backend force an
   IDR or rebuild on retune. (Transcode does not call it, but the live-publish path
   does.)
2. **A per-encode target knob** — `Config.bitrate` as a per-encoder CBR target
   (moq-transcode sets it per rung at construction). Our backends must honor it as a
   *target for this encoder instance*, not merely a ceiling for a rate controller.
   For the maintainer's "custom" per-GOP size targeting, the useful additive surface
   is a target-quality/QP or target-size knob; our VAAPI encoder already has the
   QP 18-36 clamp and cros-codecs `Tunings.rate_control`, and rav1e has quantizer /
   min_quantizer — expose these rather than hiding them behind a streaming policy.
3. **Cheap reconfigure/reset between segments of different resolution/bitrate.**
   Correction to the task premise: our *decoders* have `reset()`
   (vaapi decoder.rs:731); our *encoders do not* — they construct fresh via `new`
   (vaapi encoder.rs:885 opens a VA `Display`+config+context; v4l2 encoder.rs:58
   opens `/dev/video11`, S_FMT, REQBUFS 4+8, STREAMON; rav1e encoder.rs:38 builds a
   fresh `Context`). moq-transcode's fetch path already builds a fresh encoder per
   group, so "cheap reconfigure" today means "cheap `Encoder::new`". Cost ranking:
   rav1e cheap (pure allocation), VAAPI moderate (VA context open), **V4L2
   expensive** (full device open + buffer alloc + streamon — genuinely costly per
   group on a Pi). The plans must state this and, where feasible, offer an encoder
   `reset()`/session-reuse path so a future moq-transcode that reuses one encoder
   across fetched groups (re-targeting bitrate/size per GOP) does not pay a full
   re-open each time.
4. **Encoding an isolated GoP starting with a forced IDR** — already covered.

### 3. Proposed coordination point #7

Add to `0-overview.md` after coordination point 6:

> **7. Per-segment transcoding and FETCH.** The maintainer's stated codec direction
> is per-segment (per-GROUP) transcoding with FETCH support — a fetch for group 45
> of a lower rendition transcodes that single GoP from the source held in relay
> memory (and possibly disk) down to e.g. 360p, with custom per-GOP rate control.
> moq-transcode already owns this: it drives encoders only through the public
> `moq_video::encode::{Kind, Config, Encoder}` front-end, selects them by `Kind`,
> sets a per-rung CBR target via `Config.bitrate` at construction, forces an IDR per
> group, and builds a fresh encoder per fetched group (`rung.rs`); it never uses
> `rate::Control`. Our zero-copy VAAPI decode -> VPP scale -> VAAPI encode is the
> Intel/AMD analog of NVDEC -> NVENC and plugs into this seam directly, and
> rav1e/rav1d give the software fallback. **Rule:** encoder contributions expose
> per-segment rate-control primitives — an honest no-IDR `set_bitrate`, a per-encode
> target-bitrate/QP knob honoring `Config.bitrate`, per-frame forced IDR, and cheap
> reconfigure/reset between segments — and defer the rate-control *policy* to
> moq-transcode. No contributed encoder may embed a streaming rate controller that
> owns bitrate adaptation or forces an IDR on retune. Plans affected: vaapi-encode,
> v4l2-encode, av1-software (encoder primitives), vaapi-decode (decode-time GPU
> `resize` feeding the transcode fanout), B2 (`Packet`+`finish` draining serves the
> one-shot fetch group), B4 (registered/Named backends are transcode-selectable).

### Specific per-plan notes to add

- **B2:** note that the per-packet `Packet.timestamp` return and `finish()`
  draining are consumed by moq-transcode's one-shot fetch path
  (`transcode_group_inner` + `Pipeline::finish(last_timestamp)`), not only by the
  capture loop — so a pipelined backend serving a fetched GoP must drain correctly
  on `finish`. No signature change beyond B2.
- **B4:** note that `Kind::{Named,Auto,Hardware}` selection is exactly how
  moq-transcode's `config.encoder`/`config.decoder` pick a backend, so a registered
  (out-of-tree) or in-tree backend is transcode-usable with zero extra wiring.
- **vaapi-encode:** add a per-segment note — honor `Config.bitrate` as a per-encoder
  CBR target (not just a rate-controller ceiling); keep `set_bitrate` no-IDR; expose
  the QP/`Tunings.rate_control` knob for future custom per-GOP targeting; confirm the
  cros-codecs `StatelessEncoder` construction cost is acceptable for a fresh-encoder-
  per-fetch model and ideally add a session-reuse/reset path.
- **v4l2-encode:** add a per-segment note flagging that a **full V4L2 device re-open
  per fetched group is expensive** (open + S_FMT + REQBUFS 4+8 + STREAMON); state the
  cost honestly and expose an encoder `reset()`/reuse so moq-transcode can re-target
  a single session across GoPs rather than re-opening. `set_bitrate` via
  `V4L2_CID_MPEG_VIDEO_BITRATE` and forced IDR already fit per-segment.
- **av1-software:** note rav1e's fresh-`Context`-per-group model fits per-segment
  FETCH cheaply (pure-Rust construction), it is the software transcode analog for
  HW-less relays, and `set_bitrate` returning `BitrateUnsupported` is fine because
  transcode sets the target at construction; carry the CPU-cost caveat into the
  per-segment context (speed-10 rav1e per fetched GoP).
- **vaapi-decode:** note that `Config::resize` decode-time hardware VPP scaling is
  the Intel/AMD analog of NVDEC's free `ulTargetWidth/Height` scaling that
  moq-transcode's fanout relies on to keep frames on the GPU; expose the `resize`-
  honoring path and the decoder `reset()`/`burst_size()` so per-group fetch decode
  reinitialises cleanly at each GoP keyframe.
