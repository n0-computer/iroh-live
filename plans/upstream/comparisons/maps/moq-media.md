# moq-media crate map (refactor planning)

> Campaign: upstream | Kind: map | Read ../../0-overview.md first; index at ../0-index.md.

Scope: `moq-media/src/**` only. Crate role: iroh-live's transport-agnostic
publish / subscribe / adaptive / A-V-sync layer built on `hang` +
`moq-lite` (aliased in the workspace as `moq-net`) + `moq-mux` +
`rusty-codecs` + `rusty-capture` + `cpal`. All `file:line` citations
verified against the tree at time of writing.

Key architectural fact established up front: this crate no longer talks to
raw `hang::container` framing. It goes through **`moq_mux`**, a container /
catalog mux abstraction. The core alias is:

```rust
// moq-media/src/lib.rs:31
pub(crate) type OrderedConsumer = moq_mux::container::Consumer<moq_mux::catalog::hang::Container>;
```

`moq_mux::catalog::hang::*` re-exports the hang catalog types (`Catalog`,
`Container`, `CatalogExt`), so "hang catalog" in this crate almost always
means "hang catalog reached via moq_mux". That indirection is the main
thing a refactor has to reason about.

---

## 0. lib.rs - public API surface (`moq-media/src/lib.rs`)

Module doc (lib.rs:1-7) states the crate has no iroh dependency and works
with any transport implementing `transport::PacketSource` /
`transport::PacketSink`. Public modules (lib.rs:9-29): `adaptive`
(cfg `any_video_codec`), `audio_backend`, `audio_file_source`,
`catalog`, `chat`, `frame_channel`, `net`, `pipeline`, `playout`,
`publish`, `source_spec`, `stats`, `subscribe`, `sync`, `transport`;
private `processing`, `util`; `test_util` under `test-util`.

Re-exports (lib.rs:33-39): `audio_backend::{AudioBackend,
AudioBackendOpts, AudioDevice}`; `rusty_capture as capture` (under
capture features); `rusty_codecs::render` (under `wgpu`); and
`rusty_codecs::{codec, config, format, test_sources, traits}` - meaning
the codec traits (`VideoEncoder`, `AudioDecoder`, …), pixel/format
types, and `codec` enums all live in **rusty-codecs**, not here. This
crate consumes them.

Cargo deps of note (`moq-media/Cargo.toml`): `hang`, `moq-lite`,
`moq-mux` (all `workspace = true`); `rusty-codecs` (feature `hang`);
optional `rusty-capture`; `cpal` (git pin), `fixed-resample`, `ringbuf`,
`rubato`, `symphonia`, `sonora` (AEC), `n0-watcher`, `n0-future`,
`n0-error`, `tokio` (sync only), `throttled-tracing`. Linux-only:
`cros-codecs` (VAAPI). The codec/capture backends are all feature-gated
and delegate to rusty-codecs / rusty-capture.

---

## 1. publish.rs + publish/controller.rs

### publish.rs (`moq-media/src/publish.rs`, 1508 LOC)

Module doc (publish.rs:1-6): main entry is `LocalBroadcast`, configured
via `VideoPublisher` / `AudioPublisher`.

**`LocalBroadcast`** (publish.rs:214-223) - the primary publish type:

```rust
#[derive(derive_more::Debug, Clone)]
pub struct LocalBroadcast {
    producer: BroadcastProducer,          // moq_lite
    state: Arc<Mutex<State>>,
    _task: Arc<AbortOnDropHandle<()>>,
    stats: crate::stats::PublishStats,
}
```

Construction (`new`, publish.rs:236-260): produces a `moq_lite::Broadcast`,
wraps a `CatalogProducer`, registers a `BroadcastDynamic` handler
(`producer.dynamic()`) and spawns `run_dynamic`. The dynamic handler is
the core mechanism: tracks are **started lazily on subscriber demand**.
`run_dynamic` (publish.rs:333-363) loops on `dynamic.requested_track()`,
calls `State::start_track`, and spawns a per-track watcher that calls
`track.unused().await` then `stop_track` when the last subscriber leaves.

Key methods (signatures): `video() -> VideoPublisher` (274),
`audio() -> AudioPublisher` (279), `enable_chat() -> Result<ChatPublisher>`
(300), `set_user(User)` (322), `consume() -> moq_lite::BroadcastConsumer`
(329), `producer() -> BroadcastProducer` (263), `preview() -> Option<VideoTrack>`
(389, raw-source tap, no re-encode), `subscribe_preview::<D>(...)` (372,
full mux→demux→decode round trip). Internal `set_video` (406) / `set_audio`
(438) install inputs and publish catalog sections in a strict order
(input available before catalog published, comment at 423-425).

**`State`** (publish.rs:609-618) holds the `CatalogProducer`, two
`CancellationToken`s, the configured `VideoInput`/`AudioRenditions`, and
`HashMap`s of active pipelines keyed by track name. `start_track`
(publish.rs:655-701) dispatches an incoming track request to a video
rendition encoder, a pre-encoded passthrough, or an audio rendition
encoder, building a `MoqPacketSink` for each.

**`VideoInput`** (publish.rs:128-134) enum: `Renditions(VideoRenditions)`
or `PreEncoded(Vec<PreEncodedTrack>)`. **`VideoRenditions`**
(publish.rs:944-950) and **`AudioRenditions`** (publish.rs:721-728) each
hold a shared source plus a `HashMap<String, RenditionEntry>` where each
entry pairs a hang `VideoConfig`/`AudioConfig` with an encoder factory
closure. Rendition names are `format!("video/{}-{}", E::ID, preset)`
(publish.rs:1016) / `format!("audio/{}-{preset}", …)` (publish.rs:828).
`add` (publish.rs:992) dispatches codec enum → static encoder factory
(`add_with_generic::<E>`), so the codec matrix (h264/av1/vtb/vaapi/v4l2/
android) is dispatched here but implemented in rusty-codecs.

**`SharedVideoSource`** (publish.rs:1087-1095) - a fan-out wrapper around
a single OS capture thread using `tokio::sync::watch<Option<VideoFrame>>`.
It reference-counts subscribers (`subscriber_count`, AtomicU32) and
parks/unparks the capture thread to release the camera when idle
(publish.rs:1119-1148). Implements `VideoSource` (publish.rs:1193). This
is genuine iroh-live glue: it does PTS-following pacing (1164-1170) and
per-subscriber lifecycle that moq/hang do not provide.

**`CatalogProducer`** (publish.rs:572-607) wraps
`moq_mux::catalog::Producer<crate::catalog::IrohLiveExt>`. Its setters
(`set_video`/`set_audio`/`set_chat`/`set_user`) mutate the locked
catalog; moq_mux publishes a fresh snapshot on mutation. This is a thin
wrapper over moq_mux - **not** a reimplementation.

**`ActiveVideoPipeline`** (publish.rs:98-110) bundles a pipeline with its
`TrackProducer` and aborts the producer on drop (moq-lite close
semantics). `AudioSourceLease` (publish.rs:913-938) leases a single
audio source to one pipeline at a time (for `AudioSourceMode::Single`).

### publish/controller.rs (`moq-media/src/publish/controller.rs`, 322 LOC)

Higher-level capture orchestrator. **`PublishCaptureController`**
(controller.rs:114-125):

```rust
pub struct PublishCaptureController {
    audio_ctx: AudioBackend,
    camera: Arc<Mutex<LocalBroadcast>>,
    screen: Option<LocalBroadcast>,
    state: Watchable<PublishOpts>,
    previous_capture: CaptureConfig,
}
```

`PublishOpts` (controller.rs:46-52: camera/screen/audio bools +
`CaptureConfig`) and `CaptureConfig` (controller.rs:31-43: device indices
+ optional codec choices). `set_opts` (controller.rs:162-190) diffs old vs
new opts and calls `apply_camera` / `apply_screen` / `apply_audio`, each
of which constructs a `CameraCapturer`/`ScreenCapturer` (rusty-capture),
picks a codec via `VideoCodec::best_available`, builds `VideoRenditions`
with `VideoPreset::all()`, and pushes into the relevant `LocalBroadcast`.
State exposed reactively via `n0_watcher::Watchable`. Camera/main
broadcast eager; screen lazy. This is app-facing convenience glue, fully
iroh-live-specific.

**Duplication verdict (publish):** No hang/moq duplication. Catalog
production is delegated to `moq_mux::catalog::Producer`; track producers
are moq-lite; encoding is rusty-codecs. The novel logic - dynamic
on-demand track start, per-subscriber source parking, simulcast rendition
registry, source leasing - is legitimately iroh-live's. One thing to
watch in refactor: the manual "publish empty catalog once to unblock
early subscribers" hack (publish.rs:582-585) and the dynamic-handler
race workaround (publish.rs:246-252) are compensating for moq-lite/hang
API sharp edges.

---

## 2. subscribe.rs (`moq-media/src/subscribe.rs`, 1566 LOC)

Module doc (subscribe.rs:1-6): `RemoteBroadcast` wraps a catalog consumer
and yields `VideoTrack`/`AudioTrack`; `VideoTrack::enable_adaptation`
adds rendition switching.

**`RemoteBroadcast`** (subscribe.rs:228-242):

```rust
#[derive(derive_more::Debug, Clone)]
pub struct RemoteBroadcast {
    broadcast_name: String,
    broadcast: BroadcastConsumer,                 // moq_lite
    catalog_watchable: Watchable<CatalogSnapshot>,
    playback_policy: PlaybackPolicy,
    shutdown: CancellationToken,
    _catalog_task: Arc<AbortOnDropHandle<()>>,
    stats: crate::stats::SubscribeStats,
    sync: crate::sync::Sync,                       // shared playout clock
}
```

`with_playback_policy` (subscribe.rs:326-398) subscribes the catalog
track via `hang::catalog::Catalog::default_track()`, wraps it in a
`CatalogConsumer` (from catalog.rs), awaits the first catalog, then spawns
a task that watches for catalog updates and bumps a sequence number. It
always creates a `Sync` (100 ms jitter default), passed to pipelines only
under `SyncMode::Synced` (see `pipeline_ctx`, subscribe.rs:420-429).

**`CatalogSnapshot`** (subscribe.rs:250-256) derefs to `Catalog`, carries
a `seq`, and equality compares only `seq`. Selection helpers
`select_video_rendition` / `select_audio_rendition` (subscribe.rs:807-826)
implement a **preset-suffix ordering** table per `Quality` - this is
iroh-live selection logic on top of the hang catalog's rendition map, not
provided by hang.

**Subscription entry points**: `media` / `media_with_decoders::<D>`
(subscribe.rs:502-520), `video_rendition::<D>` (533-568),
`audio_rendition::<D>` (581-607), plus `video`/`audio`/`video_with`/
`audio_with` dynamic-dispatch conveniences (613-654), `video_ready`/
`audio_ready` (688-700, wait for catalog), and `raw_video_track` /
`raw_audio_track` (732-782) returning a bare `MoqPacketSource` for
relay/record without decode. Every one of these builds the same
`OrderedConsumer::new(track_consumer, Container::Legacy).with_latency(...)`
(e.g. subscribe.rs:558-560, 597, 750-752) - the `moq_mux` ordered
consumer is where group-level latency skipping happens.

**`VideoTrack`** (subscribe.rs:895-903):

```rust
pub struct VideoTrack {
    rx: crate::frame_channel::FrameReceiver<VideoFrame>,
    inner: VideoTrackInner,
    adaptation: Option<AdaptationState>,   // cfg any_video_codec
}
```

`VideoTrackInner` (subscribe.rs:916-928) is either `Pipeline(VideoDecoderHandle)`
or a raw `VideoSource` capture thread (used by `from_video_source`,
936-1027, for local preview - it runs its own 30fps loop with a `Scaler`).
Frame delivery uses the single-slot `frame_channel` (latest-wins). Methods:
`try_recv` (non-blocking, 1089), `next_frame` (async, 1103), `has_frame`,
`set_viewport`, and the adaptation surface (`enable_adaptation` 1123,
`disable_adaptation`, `selected_rendition`, `rendition_watcher`,
`set_rendition_mode`). `enable_adaptation` reuses the same
`frame_channel` via `self.rx.new_sender()` (subscribe.rs:1150) so the
decoder can be swapped underneath the consumer without the consumer
changing how it reads.

**`AudioTrack`** (subscribe.rs:832-836) wraps an `AudioDecoderPipeline`;
`spawn::<D>` (839-851) builds a `MoqPacketSource` from the ordered
consumer and an `AudioDecoderPipeline`. Exposes volume/handle/stopped.

**`MediaTracks`** (subscribe.rs:1233-1240) bundles the broadcast + optional
video + optional audio; `new::<D>` (1244-1277) selects renditions by
quality and subscribes both.

**Adaptation task** (`adaptation_task_v2`, subscribe.rs:1293-1493) - the
runtime driver for the `adaptive.rs` state machine (see §3). It holds the
current `VideoDecoderHandle`, runs an interval loop, refreshes rendition
ranking on catalog change, handles `Fixed` mode, runs upgrade "probes"
(subscribe to a higher rendition in parallel, commit or abort based on
loss/congestion), and applies `Downgrade`/`Emergency`/`StartProbe`
decisions via `switch_rendition_v2` (1497-1533), which builds a new
`VideoDecoderPipeline::with_sender` writing into the shared frame sender.
Note the doc at 1281-1287 references an older `adaptation_task` in
adaptive.rs that no longer exists - a stale comment worth cleaning up.

**Duplication verdict (subscribe):** No hang/moq container reimplementation
- it wraps `BroadcastConsumer` (moq-lite) and the `moq_mux` ordered
consumer. Rendition selection-by-quality, the latest-wins frame channel,
seamless decoder swapping, and adaptation orchestration are genuine
iroh-live glue. The `NoCatalog`/`RenditionNotFound`/`Ended` error enum
(subscribe.rs:180-206) is iroh-live-specific.

---

## 3. adaptive.rs (`moq-media/src/adaptive.rs`, 592 LOC)

Pure decision logic (no I/O), consumed by `adaptation_task_v2` in
subscribe.rs. Module gated on `any_video_codec`.

**Types.** `RenditionMode` (adaptive.rs:19-26): `Auto` | `Fixed(String)`.
`AdaptiveConfig` (adaptive.rs:29-74) holds thresholds/timers. Defaults
encode the documented design: `upgrade_hold=4s`, `downgrade_hold=500ms`,
`probe_duration=3s`, `probe_cooldown=8s`, `post_downgrade_cooldown=4s`,
`loss_downgrade=0.10`, `loss_emergency=0.20`, `loss_good=0.02`,
`loss_probe_abort=0.05`, `bw_downgrade_ratio=0.85`, `bw_probe_headroom=1.2`,
`check_interval=200ms`. `RankedRendition` (adaptive.rs:79-90) and
`rank_renditions` (93-110, sort by pixel count descending, index 0 =
highest). `Decision` (115-125): `Hold | Downgrade(idx) | Emergency |
StartProbe(idx)`. `AdaptationTimers` (128-144) is the mutable state
carried across ticks (`bad_since`, `good_since`, `last_downgrade`,
`last_probe`, `probe_congestion_baseline`, `last_switch_failure`).

**Algorithm** (`evaluate`, adaptive.rs:150-223). Per tick, in order:
(1) **Emergency** - if `loss >= loss_emergency` and not already lowest,
drop straight to lowest immediately. (2) **Downgrade** - bandwidth-primary:
`bandwidth_stressed = available_bps < current.bitrate_bps * 0.85` OR
`loss >= 0.10`; if sustained for `downgrade_hold`, step down one rendition.
(3) **Upgrade gating** - blocked while highest, during
`post_downgrade_cooldown`, or during `probe_cooldown`; otherwise if the
next-higher rendition has bandwidth headroom
(`available_bps >= next.bitrate_bps * 1.2`) AND `loss <= loss_good`
sustained for `upgrade_hold`, emit `StartProbe(next)`.
`should_abort_probe` (226-232) aborts an in-flight probe if
`loss >= loss_probe_abort` or `congestion_events` rose above baseline.
The failure-cooldown (`last_switch_failure`) is enforced in the *task*,
not in `evaluate` (see subscribe.rs:1402-1404), except Emergency bypasses
it. The seamless "probe" (parallel decode, commit on success) lives in
the task, not here. Extensive unit tests (adaptive.rs:236-592).

**Duplication verdict (adaptive):** Nothing to do with hang/moq - this is
original ABR logic. Depends only on `hang::catalog::VideoConfig` (to read
width/height/bitrate) and `crate::net::NetworkSignals`. Fully
iroh-live-specific.

---

## 4. sync.rs + playout.rs

### sync.rs (`moq-media/src/sync.rs`, 420 LOC)

**Direct port of `moq/js` `js/watch/src/sync.ts` @ 53fe78d8** (module doc
sync.rs:1-40). Not a wrapper of any hang/moq Rust type - a reimplementation
of the JS playout clock.

**`Sync`** (sync.rs:56-59) is an `Arc<SyncInner>`; `SyncInner`
(sync.rs:72-85) holds a base `Instant`, a `Mutex<SyncState>`, and a
`Condvar`. `SyncState` (sync.rs:90-117) stores everything as `i64`
milliseconds: `reference` (earliest `now_ms - pts_ms` ever seen),
`jitter_ms` (default 100), `audio_ms`/`video_ms` (per-codec latency,
currently always `None` because the Rust catalog doesn't carry the JS
`jitter` field - noted at sync.rs:99-106), `latency_ms = max(audio,video)+
jitter`, and `closed`.

**Algorithm.** `received(pts)` (sync.rs:151-164): update `reference` to
`min(reference, now_ms - pts_ms)` - only tightens. Called on the video
receive path only. `wait(pts)` (sync.rs:187-224): computes
`sleep = (reference - (now - pts)) + latency`; if `<= 0` render now, else
`Condvar::wait_timeout`; woken early when reference/latency change; returns
`false` if closed (pipeline teardown). Frame renders at wall time
`reference + pts + latency`. Audio does not participate - it paces via its
own sink ring buffer (doc sync.rs:36-40). Setters recompute latency and
notify. `Drop for SyncInner` (sync.rs:65-70) sets closed + notifies so
blocked decode threads wake.

### playout.rs (`moq-media/src/playout.rs`, 92 LOC)

Policy layer. **`SyncMode`** (playout.rs:16-34): `Synced` (default, uses
`Sync::wait`) | `Unmanaged` (PTS-cadence `FramePacer`). **`PlaybackPolicy`**
(playout.rs:42-54):

```rust
pub struct PlaybackPolicy {
    pub sync: SyncMode,
    pub max_latency: Duration,   // → moq_mux ordered consumer max_latency
}
```

Default `Synced` + 150 ms `max_latency` (playout.rs:56-63). `max_latency`
is passed straight to the `moq_mux`/hang ordered consumer as the
group-skip threshold; the doc explicitly maps it to the JS container
consumer `latency` param (playout.rs:47-53).

**Duplication verdict (sync/playout):** `sync.rs` is a deliberate port of
the JS `sync.ts`, so it does *reimplement* moq-js behavior - but there is
no Rust hang/moq-lite equivalent to wrap (the Rust hang stack has no
playout clock; group-latency skipping in `moq_mux` is a different,
coarser mechanism). The `audio_ms`/`video_ms` fields are dead until the
Rust catalog gains the per-codec `jitter` field that hang-js has -
flagged as a real hang-catalog feature gap. `playout.rs` is thin policy
glue.

---

## 5. pipeline.rs + pipeline/{video,audio}_{encode,decode}.rs

`pipeline.rs` (`moq-media/src/pipeline.rs`, 88 LOC) is the module hub.
`PipelineContext` (pipeline.rs:30-42) bundles `DecodeStats` + optional
shared `Sync`. `forward_packets` (pipeline.rs:45-88) is the shared async
bridge: read from a `PacketSource` and push into a bounded
`mpsc::channel(32)` that the decode OS thread drains. Every pipeline runs
on a dedicated OS thread (per the crate convention: decoders are OS
threads, not tokio tasks) and bridges sync codec APIs ↔ async transport
via that channel.

**video_encode.rs** (249 LOC). `VideoEncoderPipeline::new`
(video_encode.rs:25-170): spawns a thread that `source.start()`, then loops
`source.pop_frame()` → `encoder.push_frame` → `encoder.pop_packet` →
`sink.write(pkt)`. It waits for the first keyframe (102-105), records
encode/fps/bitrate stats, and paces to the encoder's configured framerate
(154). `PreEncodedVideoPipeline` (179-249) is a passthrough: source
produces packets directly, forwarded to the sink after the first keyframe.
Both cancel via a `CancellationToken` on drop. Source = rusty-codecs
`VideoEncoder` trait; sink = `MoqPacketSink`.

**video_decode.rs** (446 LOC). `VideoDecoderPipeline` (video_decode.rs:24-27)
splits into `VideoDecoderFrames` (the `frame_channel` receiver) and
`VideoDecoderHandle` (control: rendition, decoder name, viewport, RAII
guard). `new` / `with_sender` (88-119) - the latter lets the adaptation
layer inject an external `FrameSender`. `build` (121-178) spawns the decode
OS thread and a `forward_packets` tokio task. The core is `decode_loop`
(213-409): drains packets into the decoder, buffers decoded frames, and
either gates each frame on `Sync::wait` (when `opts.sync.is_some()`) or
paces via the legacy `FramePacer` (411-446, PTS-delta sleep clamped to
2× frame time). Handles decode errors by `reset()` + wait-for-keyframe
(331-336). Records rich timing/lag/AV-delta stats. This decode loop is a
close Rust analogue of the JS `video/decoder.ts` (doc 181-212).

**audio_encode.rs** (155 LOC). `AudioEncoderPipeline::new` (audio_encode.rs:27)
pulls a source from an `AudioStreamFactory`; `with_source` (38) takes a
pre-made source and asserts format match. `build` (55-149) spawns a thread
that ticks every 20 ms, pulls `samples_per_frame` from the source, feeds
`encoder.push_samples` → `pop_packet` → `sink.write`, stamping packet PTS
from thread start (92). Drains remaining packets on shutdown.

**audio_decode.rs** (274 LOC). `AudioDecoderPipeline` (audio_decode.rs:23-32)
holds the sink handle + shutdown + task/thread. `audio_decode_loop`
(116-274) ticks every 10 ms, decodes packets, pushes samples straight to
the sink's ring buffer (which does the jitter smoothing), and inserts a
960-sample silence chunk when the sink buffer drops below 20 ms to avoid
underruns. Records audio lag/buffer stats. Does **not** use `Sync` - audio
is the pacing master; video syncs to it.

**Duplication verdict (pipeline):** This is the glue that will change most
in a refactor. It reimplements, in Rust OS-thread form, what the moq-js
watch/publish pipelines do (the code says so). It does not duplicate hang
container framing - that is delegated to `MoqPacketSink`/`MoqPacketSource`
(transport.rs) → `moq_mux`. The keyframe-gating, jitter/silence handling,
FramePacer, and stats plumbing are genuine iroh-live logic. The 20ms/10ms
tick loops and `mpsc(32)` handoff are candidate simplification targets.

---

## 6. Support modules (one paragraph each)

**catalog.rs** (`moq-media/src/catalog.rs`, 75 LOC). Defines the
iroh-live catalog as a thin extension over hang's. `Catalog` /
`CatalogConsumer` are aliases of `moq_mux::catalog::hang::Catalog<IrohLiveExt>` /
`Consumer` (catalog.rs:9,12). The new type is `IrohLiveExt`
(catalog.rs:22-25: optional `chat`, `user`) implementing hang's
`CatalogExt` (catalog.rs:27); supporting `Chat` (32) holds `moq_lite::Track`
handles, `User` (40). **WRAPS/extends** via the sanctioned `CatalogExt`
mechanism; wire-compatible with hang; no duplication.

**frame_channel.rs** (299 LOC). A generic single-slot latest-value channel
(`SlotInner<T>` at 21, `FrameSender<T>` at 38, `FrameReceiver<T>` at 47,
`frame_channel<T>()` at 55) replacing a drained-to-latest `mpsc`. `send`
overwrites; `new_sender` (129) lets the adaptation layer re-point the
producer during decoder swaps. **Neither wraps nor reimplements** hang/moq
- a std/tokio concurrency primitive; no moq imports.

**source_spec.rs** (499 LOC). Pure CLI/source-string parsing.
`VideoSourceSpec` (48: DefaultCamera/Camera/Screen/Test/File/PreEncoded/None)
and `AudioSourceSpec` (242) with `parse(&str)` (183/264), plus
`BackendRef`/`DeviceRef`/`CaptureSpec`/`CaptureKind` and a `KNOWN_BACKENDS`
table (293). **No moq involvement**; iroh-live/rusty-capture-specific
config parsing.

**transport.rs** (204 LOC). The codec-facing packet abstraction and the
seam between iroh-live media and the container layer. Traits `PacketSource`
(16) / `PacketSink` (24). `MoqPacketSource` (34) wraps a hang
`OrderedConsumer`; `MoqPacketSink` (89) wraps
`moq_mux::container::Producer<...hang::Container>` and delegates
framing/keyframe-grouping to it (94-116). `media_pipe`/`PipeSink`/`PipeSource`
(125-168) give an in-memory non-network implementation. `moq_frame_to_media_packet`
(77) converts `moq_mux::container::Frame` → `MediaPacket`. **WRAPS** -
container framing and keyframe grouping are delegated to `moq_mux`, not
reimplemented. This trait pair is the intended refactor boundary and the
crate's only structural coupling to the container format.

**net.rs** (29 LOC). One POD struct `NetworkSignals` (9: `rtt`, `loss_rate`,
`available_bps`, `congestion_events`) populated externally from QUIC stats
and consumed by `adaptive.rs`. **No moq involvement.**

**stats.rs** (494 LOC). Typed, atomic metrics/observability layer for debug
overlays (no string keys). Building blocks `Metric` (73, EMA + history) and
`Label` (211); category structs `NetStats`/`EncodeStats`/`RenderStats`/
`TimingStats`/`DecodeStats`/`SubscribeStats`/`PublishStats`; `LagTracker`
(176) for PTS-vs-wall lag. **Self-contained; no moq involvement.**

**chat.rs** (182 LOC). Text chat over a MoQ track: one group per message,
one UTF-8 frame per group (doc 1-9). `ChatPublisher` (47) wraps
`moq_lite::TrackProducer`; `ChatSubscriber` (78) wraps `TrackConsumer` and
yields `ChatMessage` (34). Track descriptor via `chat_track()`/
`CHAT_TRACK_NAME`/`CHAT_PRIORITY` (18-25). **WRAPS** moq-lite group/frame
APIs directly; the only new logic is trivial UTF-8 framing + `received_at`
timestamp - application payload, not container duplication.

**audio_file_source.rs** (104 LOC). Facade: `pub use
crate::audio_file_symphonia::AudioFileSource` (6) plus tests. No moq.

**audio_file_symphonia.rs** (368 LOC). Pure-Rust WAV/MP3/FLAC import via
symphonia + rubato resample to 48 kHz stereo f32 (drops the ffmpeg runtime
dep). `AudioFileSource` (50) holds an `AudioFormat`, a `ringbuf::HeapCons<f32>`,
and an `eof` flag; `new` (73) spawns a background decode thread pushing into
a lock-free ring buffer; implements the crate `AudioSource` trait (98). No
moq.

---

## 7. audio_backend.rs + audio_backend/aec.rs

**audio_backend.rs** (2445 LOC). A cpal-based device-I/O layer (replaced a
former firewheel graph). All internal processing fixed at 48 kHz stereo
(`INTERNAL_RATE`/`INTERNAL_CHANNELS`, ~lines 60-62). Three thread
categories: caller threads (push/pop via `OutputStream`/`InputStream`
handles), real-time lock-free cpal callback threads, and a single
`AudioDriver` OS thread (~1175) owning stream lifecycle / device switching /
error recovery, driven by a bounded `mpsc` `DriverMessage` channel (~673).
cpal config negotiation prefers 48k→44.1k→highest, always f32
(`negotiate_stream_config` ~802); streams built via `build_output_stream`
(~1471) / `build_input_stream` (~1540) with error callbacks feeding a
restart/backoff path (~1368-1406). Each stream gets a
`fixed_resample::resampling_channel` (output 50 ms, input 30 ms latency).
Output callback (~942) mixes all consumers with per-stream fade + volume,
writes the mix into the AEC render-reference ring buffer; input callback
(~1111) channel-maps to stereo, runs AEC in place, then fans out to all
input producers.

**Main public types.** `AudioBackend` (~134): a cloneable handle wrapping
`mpsc::Sender<DriverMessage>` + a shared `Arc<AtomicBool>` AEC flag;
exposes `list_inputs`/`list_outputs`, `input`/`output` (+ default/blocking
variants), `switch_input`/`switch_output`, AEC toggles; implements
`AudioStreamFactory` (~285). `AudioBackendOpts` (~83: host, input/output
`DeviceId`, `fallback_to_default`). `AudioDevice` (~122: id/name/is_default).
`OutputStream` (~315) / `InputStream` (~490) implement the crate's
`AudioSink` / `AudioSource` traits.

**audio_backend/aec.rs** (392 LOC). Echo cancellation via the `sonora`
crate. `AecProcessor` (aec.rs:66-74) is an `Arc<Inner>` holding a
`Mutex<AudioProcessing>` + enabled flag; `AecProcessorConfig` (31);
`AecState` (201) is the callback-side accumulator. The render reference is
a lock-free SPSC `ringbuf` (~100 ms): output callback holds the producer,
input callback holds the consumer, so `process_stereo_interleaved`
(aec.rs:274) can drain the reference and run render/capture echo models
serialized on the input thread (avoids cross-callback mutex contention).
AEC types are `pub(crate)`.

**Duplication verdict (audio_backend):** **No moq involvement whatsoever** -
imports are std/anyhow/cpal/fixed_resample/ringbuf/tokio/sonora plus the
crate's own `format::AudioFormat` and `traits::{AudioSink,AudioSource,
AudioStreamFactory}`. Any moq integration is behind those traits, not here.
Pure device-I/O + DSP glue; genuinely iroh-live-specific (and the largest
single file in the crate).

---

## Summary: wrapping vs reimplementation vs genuine glue

Wraps / extends hang·moq-lite·moq-mux (correct, keep):
- **catalog.rs** - extends hang catalog via `CatalogExt`.
- **transport.rs** - `MoqPacketSink`/`MoqPacketSource` delegate container
  framing to `moq_mux`; the intended refactor seam.
- **chat.rs** - moq-lite track/group/frame directly.
- **publish.rs `CatalogProducer`** - wraps `moq_mux::catalog::Producer`.
- **subscribe.rs / publish.rs track handling** - moq-lite
  BroadcastConsumer/Producer + `moq_mux` ordered consumer.

Reimplements moq-js behavior (no Rust hang/moq equivalent to wrap - but
verify against upstream during refactor):
- **sync.rs** - direct port of moq-js `sync.ts` playout clock. Its dead
  `audio_ms`/`video_ms` fields flag a **missing per-codec `jitter` field in
  the Rust hang catalog** (present in hang-js). Real gap to raise upstream.
- **pipeline/video_decode.rs `decode_loop`** and the encode/decode threads -
  Rust OS-thread analogues of the moq-js watch/publish pipelines.

Genuinely iroh-live-specific glue (no overlap with hang/moq):
- **adaptive.rs** - original ABR state machine.
- **audio_backend.rs + aec.rs** - cpal device I/O + sonora AEC (largest,
  fully independent).
- **frame_channel.rs** - latest-wins single-slot channel enabling seamless
  decoder swaps.
- **publish `SharedVideoSource`** - ref-counted parking capture fan-out.
- **stats.rs, net.rs, source_spec.rs, playout.rs, audio_file_symphonia.rs,
  publish/controller.rs** - metrics, signals, parsing, policy, file import,
  capture orchestration.

No file was found to duplicate hang container/catalog logic or moq
track/group logic. The one structural risk for a refactor is the double
indirection `moq-media → moq_mux → hang`, and the several small
compensating hacks in publish.rs (empty-catalog priming, dynamic-handler
race) that paper over moq-lite/hang API rough edges.
