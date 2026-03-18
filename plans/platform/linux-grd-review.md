# GNOME Remote Desktop review: lessons for iroh-live on Linux

Source: [gnome-remote-desktop](https://gitlab.gnome.org/GNOME/gnome-remote-desktop),
GNOME 50 release (2025). Reviewed against our Linux pipeline (PipeWire
capture → VAAPI encode → MoQ transport → VAAPI decode → DMA-BUF import
→ wgpu render) on Intel Meteor Lake.

## Architecture comparison

GRD is a server-side remote desktop encoder: it captures the compositor's
output via PipeWire, encodes it as H.264, and sends it over RDP. We share
the same capture→encode→transport pipeline shape, but we also handle the
decode→render side, which GRD delegates to the RDP client. Where GRD
optimizes for single-stream, single-viewer latency with frame-level
feedback from the RDP protocol, we optimize for multi-rendition
publish/subscribe with transport-level adaptation.

The comparison is most valuable for the server side (capture, encode, GPU
resource management) and for general pipeline discipline (buffer
lifecycle, frame pacing, damage detection). The client-side patterns are
less directly applicable because GRD relies on the RDP client for decode
and render.

## Findings worth adopting

### 1. Drain-all-keep-latest dequeue

GRD's PipeWire `on_stream_process` drains *all* pending `pw_buffer`s in
a loop, keeping only the most recent frame and cursor update. Intermediate
buffers are re-queued immediately. This bounds capture-to-encode latency
at one frame interval regardless of how long encoding takes.

Our PipeWire capture uses a bounded `SyncSender` that drops on overflow,
which achieves the same "latest wins" effect but at the channel level
rather than the PipeWire buffer level. The difference matters: we hold
PipeWire buffers longer than necessary because we dequeue one at a time
and the channel may hold several frames before the encoder pops them.
This delays buffer recycling and can cause PipeWire to stall when its
buffer pool runs dry.

**Action:** Drain all pending PipeWire buffers in our callback, keep only
the latest, and re-queue the rest immediately. This returns buffers to the
compositor faster and reduces capture-side latency.

### 2. Explicit sync via DRM timeline syncobjs

GRD handles `SPA_META_SyncTimeline` metadata on PipeWire buffers: it
waits on the acquire syncobj point before reading buffer contents and
signals the release point when done. This implements the explicit sync
protocol that modern compositors (Mutter 46+) use when implicit sync is
not available — a requirement for NVIDIA drivers and increasingly for
Intel and AMD as the kernel moves away from implicit sync.

We do not handle `SPA_META_SyncTimeline` at all. On current Intel
hardware with Mesa's ANV driver, implicit sync still works, so we have
not hit this bug yet. On NVIDIA hardware (which uses
`nvidia-vaapi-driver` for VA-API) or on future kernel versions that drop
implicit sync support, we will get tearing or stale frames.

**Action:** Implement explicit sync support in `rusty-capture`'s PipeWire
backend. Read `SPA_META_SyncTimeline` from each buffer, wait on the
acquire point before consuming the DMA-BUF, and signal the release point
before re-queuing.

### 3. Frame pacing with client feedback

GRD's `GrdRdpGfxFrameController` implements a three-state throttling
machine driven by client frame acknowledgments:

- *Inactive*: unlimited frame submission.
- *Active*: `total_frame_slots = ack_rate + 2 - enc_rate`. Frames are
  rate-limited to match the client's consumption capacity.
- *Active lowering latency*: full stop on new frames, drain pipeline to a
  lower threshold when RTT improves.

The activation threshold adapts to RTT:
`activate_th = max(2, min(delayed_frames + 2, refresh_rate))` where
`delayed_frames = rtt × refresh_rate / 1e6`. This means a LAN connection
(16 ms RTT at 60 fps) activates at two frames in flight, while a WAN
connection (50 ms RTT) activates at five.

Our pipeline is source-driven: the encoder runs at the configured frame
rate regardless of subscriber capacity. The playout clock handles
late-arriving frames by re-anchoring, and `hang`'s `max_latency` drops
old groups at the transport level, but neither mechanism feeds back into
the encoder to *stop producing frames*. This wastes encode cycles when
the subscriber cannot keep up, and accumulated latency drains slowly
through group skips rather than instantly through pipeline drain.

**Action:** Implement encoder-side pacing. When transport-level signals
(unacked groups, rising RTT) indicate the subscriber is falling behind,
reduce or pause encoding. When conditions improve, resume. This is
complementary to our existing adaptive bitrate — ABR adjusts quality
while pacing adjusts quantity.

### 4. Buffer pool with lazy shrink

GRD's `GrdRdpBufferPool` grows on demand (one buffer at a time when all
are taken) and shrinks lazily via a deferred GLib source that fires after
the demand subsides. The pool never shrinks below a configured minimum.
CUDA resource unmapping is also deferred to a low-priority source to
avoid stalling the critical path.

Our VA-API encoder relies on `cros_codecs`' internal frame pool, which is
reasonable, but we have no pooling for decoded frames on the subscribe
side or for wgpu textures in the renderer. Each decoded frame allocates
fresh Vulkan resources (DMA-BUF import, `VkImage`, wgpu texture), and the
previous frame's resources are dropped immediately. At 30 fps this is 30
Vulkan resource creation/destruction cycles per second.

**Action:** Pool decoded frame resources (imported `VkImage` handles,
wgpu textures) on the render side. Allocate a small pool (four to eight
entries), reuse on resolution match, grow on demand, shrink lazily.

### 5. Fused color conversion and damage detection in one compute dispatch

GRD's Vulkan compute shader (`grd-avc-dual-view.comp`) performs three
operations in a single dispatch: BGRX→NV12 color conversion, AVC444
auxiliary view construction, and per-tile damage detection via shared
memory. By fusing these operations, the shader reads source pixels once
and produces both the encode input and the damage map without a second
pass.

Damage detection uses a 64×64 tile grid. Each 32×32 workgroup compares
new and old source textures per pixel, writing a shared `have_block_damage`
flag. This avoids re-encoding unchanged regions entirely.

We do not perform damage detection at all — every frame is a full encode.
For screen sharing and remote desktop scenarios, where large regions of
the screen are often static (text editors, terminals, presentations),
this wastes significant encode bandwidth and GPU cycles.

**Action:** For screen-capture sources, add tile-based damage detection.
The simplest path is a compute shader that compares the current and
previous capture buffers per 64×64 tile and produces a damage bitmap.
Pass the damage bitmap to the encoder as a region-of-interest hint. For
VA-API, this means setting per-macroblock QP or using skip macroblocks
for undamaged regions. For software H.264 (openh264), the `SEncParamExt`
structure supports dirty rect hints. This is orthogonal to our current
pipeline and does not require architectural changes.

### 6. DMA-BUF modifier negotiation with format intersection

GRD queries both EGL and Vulkan for supported DRM format modifiers, then
computes their intersection. Only modifiers in the intersection are
advertised to PipeWire, ensuring the compositor delivers buffers that both
the color conversion shader (Vulkan) and the encode surface import
(VA-API via Vulkan interop) can consume without re-tiling.

Our PipeWire capture assumes linear modifier (`DRM_FORMAT_MOD_LINEAR`) and
falls back to CPU mmap when the modifier is incompatible. Our VPP retiler
handles the Intel Y-tiled→CCS case, but this is reactive (detect
incompatibility at import time, then re-tile) rather than proactive
(negotiate compatible modifiers upfront).

**Action:** Query Vulkan for supported NV12 DMA-BUF modifiers at
PipeWire stream setup time and include them in the SPA format negotiation.
This lets the compositor choose a modifier we can import directly,
avoiding the VPP re-tile in most cases. Keep the VPP retiler as a
fallback for edge cases where the compositor ignores modifier hints.

### 7. Corrupted frame detection

GRD checks `SPA_META_Header` for the `SPA_META_HEADER_FLAG_CORRUPTED`
flag on every PipeWire buffer. Corrupted buffers are immediately
re-queued without processing. We do not check this flag. A corrupted
buffer passed to the encoder produces garbage output that propagates to
all subscribers until the next keyframe.

**Action:** Check `SPA_META_Header` flags in our PipeWire callback and
drop corrupted buffers.

## Patterns we already handle well

### Adaptive bitrate

GRD uses fixed CQP with QP=22 and no rate control — quality never
changes. Rate adaptation is left entirely to the RDP protocol layer
(frame throttling, progressive refinement). Our ABR system is
substantially more capable: loss-based primary signal, bandwidth
secondary, asymmetric timers (500 ms downgrade / 4 s upgrade),
1.2× headroom probing, and seamless rendition switching with parallel
decoder startup. This is closer to WebRTC's approach than GRD's.

### Multi-rendition simulcast

GRD encodes a single stream. We publish multiple renditions from a single
source, letting the subscriber or an intermediary select the appropriate
quality tier. This is architecturally more flexible for multi-subscriber
scenarios.

### Playout timing

GRD's frame clock uses `timerfd` for grid-aligned absolute timing, which
prevents drift. Our `PlayoutClock` achieves the same effect with
PTS→wall-clock mapping, RFC 3550 jitter estimation, and re-anchoring on
buffer underrun. The re-anchor approach is better suited to variable-rate
transport (MoQ groups arrive in bursts, not at fixed intervals).

### Zero-copy decode→render

Both systems import VA-API decoder output as Vulkan images via DMA-BUF.
Our VPP retiler handles the Intel modifier incompatibility (Y-tiled from
decoder vs CCS required by Vulkan ANV), which GRD sidesteps by
negotiating modifiers upfront. Both approaches work; ours is more
defensive.

### Audio pipeline

GRD has no audio handling (RDP audio uses a separate channel). Our
audio pipeline — Opus encode/decode, cpal/Oboe backend, acoustic echo
cancellation — is complete and tested.

## Patterns that do not apply to us

### RDP-specific frame protocol

GRD's progressive chroma refinement (AVC444 main/auxiliary views),
surface-level damage tracking, and GFX frame acknowledgment protocol
are specific to the RDP graphics pipeline. We use MoQ (QUIC-based), which
has different flow control semantics.

### CUDA path

GRD maintains a parallel NVIDIA/CUDA path for color conversion and
damage detection. Our NVIDIA support goes through `nvidia-vaapi-driver`
(VA-API translation layer), which is simpler and sufficient for our
encode/decode needs. We do not need a separate CUDA path.

### EGL thread for buffer download

GRD uses a dedicated EGL thread with slot-based task replacement for
GPU→CPU buffer readback. We do not need CPU readback in the zero-copy
path (DMA-BUF stays on GPU from decode through render), and for the
CPU fallback path, our existing thread model is adequate.

## Priority ranking

Ranked by impact on real-world performance:

1. **Drain-all-keep-latest dequeue** — low effort, immediate latency win,
   reduces PipeWire buffer stalls.
2. **Corrupted frame detection** — trivial, prevents garbage propagation.
3. **Explicit sync support** — medium effort, required for NVIDIA and
   future kernel changes, prevents tearing.
4. **DMA-BUF modifier negotiation** — medium effort, eliminates VPP
   re-tile overhead in common case.
5. **Render-side resource pooling** — medium effort, reduces per-frame
   Vulkan allocation churn.
6. **Damage detection for screen capture** — high effort, large win for
   screen-sharing workloads with mostly static content.
7. **Encoder-side pacing** — high effort, prevents wasted encode cycles,
   reduces latency accumulation under congestion.
