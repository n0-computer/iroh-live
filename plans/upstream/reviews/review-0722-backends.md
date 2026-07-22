# Review 0722 — V4L2 / Android / AV1 / bitstream backend plans

> Campaign: upstream | Kind: review | Read ../0-overview.md first.

Adversarial staff review of `../codec/{v4l2-encode,v4l2-decode,android-mediacodec,av1-software,bitstream-sps-vui}.md`
cross-checked against real source: moq `/home/bit/Code/rust/moq` (HEAD 3a3e0ea8) and
iroh-live `/home/bit/Code/rust/iroh-live` (rusty-codecs working tree).

## Verdict

The plans are unusually accurate. Nearly every load-bearing source anchor resolves to the
claimed symbol, all LOC counts match, and the base-plan prerequisites (B1/B2/B3/B4) are
genuinely unmerged in moq as the plans assume. Two substantive buildability errors need
fixing before the leaves are actionable (a false `libc` claim; a Windows-gated, stride-less
`from_nv12` the decode plan leans on). The rest are line-drift nits and missing-detail asks.
No blocking design flaw: the approach, the candidate shapes, the pipelined/B2 rationale, and
the transcode cost ranking all hold against source.

**Counts:** blocking 0 · substantive 5 · nit 10 · missing-detail 6

## Ground truth confirmed (so the plans stand on it)

- moq encode `Backend::encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>`
  (`encode/backend/mod.rs:40`) — no timestamp, no `Packet`. B2 genuinely unmerged.
- No `Packet` type, no `register_encoder`/`register_decoder`, no `Native`/`native()`, no
  `Frame::DmaBuf`/`Frame::HardwareBuffer` in moq. B1/B2/B3/B4 all unmerged as assumed.
- Encode `Candidate { name, codecs, open: fn(&Config)->... }` (`encode/backend/mod.rs:60-64`)
  and decode `Candidate { name, supports: fn(Codec)->bool, open }` (`decode/backend/mod.rs:81-85`):
  both plans construct the candidate with the right field set.
- Decode `Codec` enum already has `Av1` (`decode/backend/mod.rs:40`); encode `Codec` is
  `#[non_exhaustive]` H264/H265 (`encode/encoder.rs:22-30`). AV1 plan's "additive" claim holds.
- `Frame::to_i420` (`frame.rs:64`) and `Error::BitrateUnsupported(&'static str)` (`error.rs:32`) exist.
- rav1e wrapping is real: `Context<u8>`, `send_frame`/`receive_packet`, and the lookahead
  timestamp map keyed by `frame_count` on insert (`av1/encoder.rs:241`) and recovered by
  `packet.input_frameno` on drain (`:110-117`). This is a genuine reorder map and the real
  reason B2 matters for AV1.
- Cost ranking (point 4) is correct: V4L2 `new()` blocks on `init_rx.recv()` only after the
  thread opens the device + `REQBUFS` + `STREAMON` (`v4l2/encoder.rs:92-95`, sequence at
  `:525-604`), so re-open is genuinely the most expensive; rav1e `Context` construction does
  no syscalls. V4L2-most-expensive / rav1e-cheapest is defensible.

## Substantive

1. **v4l2-encode — "libc is already a moq dependency" is false.**
   Plan (Target-in-moq / Adaptation): "it pulls in no external system-library crate: the
   backend is pure `libc` ioctl, and `libc` is already a moq dependency. The feature exists
   only to gate the module and the candidate."
   Evidence: `libc` is **not** declared in `rs/moq-video/Cargo.toml` and there is no `libc::`
   use anywhere in `rs/moq-video/src/` (it appears only transitively in `Cargo.lock`).
   Severity: substantive. Fix: the `v4l2` feature must add `dep:libc` (optional). Reword — the
   "feature exists only to gate the module" claim is wrong; it also pulls `libc`. (libc is a
   crates.io crate, not a system `.so`, so the "no system-library" spirit survives, but the
   dependency-already-present claim does not.)

2. **v4l2-decode — `I420::from_nv12` is Windows-only and stride-less.**
   Plan (moq-API-consumed + step 5): convert the decoder's strided NV12 to `Frame::I420` "via
   moq's `I420::from_nv12` (`frame.rs:208-225`)", "honoring the per-plane stride the `DqBuffer`
   reports." Evidence: `from_nv12` is `#[cfg(target_os = "windows")]` (`frame.rs:210`) and its
   signature is `from_nv12(nv12: &[u8], width, height)` assuming tightly-packed NV12
   (`luma = w*h`, no stride). The V4L2 decoder runs on Linux and emits **strided** planes
   (our `extract_decoded_frame` honors `stride`, `decoder.rs:360-411`). So the cited helper
   (a) does not compile on Linux and (b) cannot honor stride.
   Severity: substantive. Fix: the leaf must supply its own stride-aware NV12→I420 pack (port
   our `copy_plane`/`interleave_uv`), or de-gate + extend `from_nv12` — not call it as-is.

3. **v4l2-decode — the ported function is `decoder_thread`, not `run_decoder`.**
   Plan (source-to-port and step 2): "All `v4l2r` generics stay local to `run_decoder`
   (`decoder.rs:161`)" / "Port the dedicated-thread stateful decoder from `run_decoder`."
   Evidence: the function is `fn decoder_thread(...)` at `decoder.rs:162`; line 161 is its doc
   comment. There is no `run_decoder` symbol.
   Severity: substantive (a named port target that does not exist). Fix: rename references to
   `decoder_thread` (`decoder.rs:162`).

4. **v4l2-decode — "the only zero-copy export machinery … is the FFI in the encoder's
   raw_v4l2 submodule" mischaracterizes that module.**
   Evidence: `grep -rn EXPBUF|DmaBuf|DMABUF|dma_buf src/codec/v4l2/` returns nothing. The
   encoder's `raw_v4l2` (`encoder.rs:311-315`) is `QBUF`/`DQBUF`/`S_FMT` encode FFI with no
   `VIDIOC_EXPBUF` and no DMA-BUF export. The conclusion "zero-copy decode is new work, not a
   port" is correct and well supported (no EXPBUF anywhere), but the supporting sentence is
   inaccurate: `raw_v4l2` is not "zero-copy export machinery."
   Severity: substantive (wrong evidence for a right conclusion). Fix: drop the raw_v4l2
   reference; state plainly there is no EXPBUF/DMA-BUF code in the tree at all.

5. **av1-software — the "pure Rust, compile-everywhere" story collides with the fork
   resolution.** The decoder uses the dav1d-rs API surface (`Settings::new`,
   `Decoder::with_settings`, `get_picture`, `PlanarImageComponent`; `av1/decoder.rs:46-93`).
   The published crates.io option is `dav1d-rs`, which wraps **C libdav1d** — a system library
   plus build tooling, contradicting the plan's "rav1d is pure Rust … builds everywhere" and
   "no dlopen needed." The pure-Rust option is `memorysafety/rav1d`, which is the git pin the
   plan is trying to eliminate, and the current pin enables `asm` (`Cargo.toml:33`), needing a
   NASM assembler at build. Severity: substantive missing detail. Fix: the fork-resolution
   section must state that dav1d-rs ≠ pure Rust (adds a libdav1d system dep), that the
   compile-everywhere claim only holds for memorysafety/rav1d, and that `asm` must likely be
   dropped for moq's default build; then pick one path explicitly.

## Nits (line drift and imprecision)

1. av1 & mod cites: encode `Codec` enum cited `encode/encoder.rs:21-40`, actually `:21-30`
   (`Kind` is `:32-48`); `#[non_exhaustive]` at `:22` is correct. `Config` `:55-70` is exact.
2. v4l2-encode cites the encode HARDWARE slice `encode/backend/mod.rs:68-102`; HARDWARE ends
   at `:93` (`:98-102` is the SOFTWARE slice). Add the v4l2 candidate in `:68-93`.
3. v4l2-encode "profile and level are auto-selected": the **profile** is hardcoded
   `CONSTRAINED_BASELINE` (`v4l2/encoder.rs:539`); only the **level** is auto-selected
   (`h264_level_for_resolution`, `:541-542`).
4. Device paths: `encoder_device_path` is `v4l2.rs:57-63` (plan `:57-62`); `decoder_device_path`
   is `:68-74` (plan `:67-73`).
5. `h264_level_for_resolution` cited `:365-388`; the fn body is `:365-380` (`:382+` is other code).
6. android: release-with-`render=true` call is `hw_decoder.rs:249` (plan `:247` — that's the
   comment); `try_reset` fn is `encoder.rs:238` (plan `:237`).
7. v4l2-decode "per-plane stride reported by the `DqBuffer` (`decoder.rs:352-354`)": `:352-354`
   is the `DqBuffer` type parameter in the fn signature; the stride actually comes from
   `FormatState.stride` (`decoder.rs:360`), populated in the format-changed callback.
8. Test gating: plans say mark hardware tests `#[ignore]`; moq's own hardware round-trip tests
   (`decode/backend/nvdec.rs:513` `round_trip`, `:534`/`:596`) are `#[cfg(feature)]`
   module-gated, not `#[ignore]`. Match moq's actual pattern or justify the divergence.
9. av1/android `set_bitrate` "return `Error::BitrateUnsupported`": the variant takes a
   `&'static str` reason (`error.rs:32`) — supply one.
10. v4l2-encode / android "match `Frame::I420` and pack; for any other CPU-representable
    variant call `frame.to_i420()`": moq's non-I420 `Frame` variants
    (`Surface`/`Texture`/`Cuda`, `frame.rs:24-33`) are all **GPU**, not "CPU-representable";
    `to_i420()` downloads them. Reword.

## Missing details to add

1. **rav1d fork resolution (av1).** Spell out: `dav1d-rs` (crates.io) links C libdav1d and is
   not pure Rust; `memorysafety/rav1d` is pure Rust but the git pin the plan forbids; the pin
   at `Cargo.toml:33` enables `bitdepth_8/16` + `asm` (NASM at build). These are not
   interchangeable — the choice changes the compile-everywhere and system-dependency story.
2. **V4L2 encoder session-reuse (v4l2-encode, transcode section).** There is no reset path
   today: `EncoderCmd` has only `Encode` (`encoder.rs:45-47`); `new()` blocks on init after
   device open+REQBUFS+STREAMON; `Drop` just closes the channel and joins (`:280-291`). Adding
   session reuse means a new `EncoderCmd::Reset { config }` (or a `RawV4l2Encoder` re-negotiate
   method) so the thread re-runs `S_FMT`/controls without re-opening the fd — or keeping one
   session and forcing an IDR at group boundaries. Name the concrete change.
3. **Android HwBuffer ↔ B1 reconciliation (android).** Our
   `NativeFrameHandle::HardwareBuffer(HardwareBufferInfo { buffer: <acquired ref>, width,
   height, y_stride, uv_offset, uv_stride })` (`gpu_frame.rs:195-200`, layout from
   `AndroidGpuFrame::new`, `hw_decoder.rs:268`) must map onto B1's `android::HwBuffer`. B1 does
   not exist yet, so state the exact field set B1 must carry (the acquired buffer handle +
   y_stride/uv_offset/uv_stride) so the coordination-point-1 gap check is concrete.
4. **v4l2-decode CPU-first / EXPBUF-followup split is justified** — EXPBUF is genuinely new
   (no export code in tree) and the Pi DRM modifier import into wgpu/Vulkan is unvalidated
   (consistent with our DMA-BUF notes). Keep the split; but note the CPU path still needs the
   stride-aware packer from finding S2 (moq has none).
5. **Licensing.** None of the five plans state the license of the ported code or that
   rusty-codecs → moq is license-compatible. Add a one-line licensing note per leaf.
6. **Test scaffolding.** moq's reusable `round_trip(encoder, decoder, w, h)` helper lives in
   `decode/backend/nvdec.rs:513`; model new hardware tests on it (and on its cfg-gating) rather
   than an unspecified "moq style."

## Not independently verified

The plans' cross-references into `../comparisons/maps/moq-video.md` and the other `../comparisons/`
map docs (e.g. `moq-video.md:100-153`, `:711-713`) are plan artifacts, not source; only the
real source anchors above were checked. The sibling `vaapi-decode` leaf is referenced for
rebase ordering — moq has no vaapi **decode** backend yet (`decode/backend/` has none), which
is expected since that leaf is also un-landed.
