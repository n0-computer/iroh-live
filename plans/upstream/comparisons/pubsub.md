# Comparison: publish, subscribe, catalog, adaptive, and sync

Part of the moq-alignment refactor series (see [0-overview.md](0-overview.md)).
This document compares iroh-live's `moq-media` publish/subscribe/catalog/
adaptive/sync layer against the corresponding moq stack: `hang`, `moq-mux`,
`moq-video`/`moq-audio` (their track-facing Producer/Consumer layer),
`moq-transcode`, and `moq-stats`. The codec and capture backends themselves
(rusty-codecs vs moq-video internals) are compared in a sibling document; here
the subject is everything between the codec and the wire.

Evidence sources: [maps/moq-media.md](maps/moq-media.md),
[maps/moq-transcode-stats.md](maps/moq-transcode-stats.md),
[maps/moq-video.md](maps/moq-video.md),
[maps/moq-audio-nvenc.md](maps/moq-audio-nvenc.md), and
[maps/moq-net-origin.md](maps/moq-net-origin.md), plus direct source reads cited
inline.

## 1. Scope

moq merged `dev` into `main` on 2026-07-21, so the old main-versus-dev split is
gone: the whole native media stack, the rewritten net layer, moq-transcode,
moq-stats, and the moq-mux catalog rework are all on a single `main`. Citations
below are against `main` HEAD `3a3e0ea8`, read via
`git show 3a3e0ea8:<path>` in `/home/bit/Code/rust/moq`. Where a capability
landed after the old dev pin `261c2048` the pull request is cited.

iroh-live still builds against the older released crates.io line (moq-net
0.1.11 aliased as `moq-lite`, moq-mux 0.5.5, hang 0.19.1), while moq main now
carries moq-mux 0.7.6, hang 0.19.5, and moq-net 0.1.18. Every moq capability
here therefore arrives for iroh-live only with the next breaking release and a
version bump of its pins; the phrasing "pending release" throughout means
exactly that, not that the code is unmerged. LOC figures for moq-media modules
are `wc -l` at time of writing.

## 2. Publish path

### What we have

`LocalBroadcast` (moq-media/src/publish.rs:214-223) is the publish root:

```rust
#[derive(derive_more::Debug, Clone)]
pub struct LocalBroadcast {
    producer: BroadcastProducer,          // moq_lite
    state: Arc<Mutex<State>>,
    _task: Arc<AbortOnDropHandle<()>>,
    stats: crate::stats::PublishStats,
}
```

Tracks start lazily on subscriber demand: `run_dynamic`
(publish.rs:333-363) loops on `dynamic.requested_track()`, dispatches to
`State::start_track` (publish.rs:655-701), and spawns a watcher that calls
`track.unused().await` then `stop_track` when the last subscriber leaves.
`State::start_track` routes a requested track name to a video rendition
encoder, a pre-encoded passthrough, or an audio rendition encoder.
`VideoRenditions` (publish.rs:944-950) and `AudioRenditions`
(publish.rs:721-728) each pair a shared source with a
`HashMap<String, RenditionEntry>` mapping rendition names
(`format!("video/{}-{}", E::ID, preset)`, publish.rs:1016) to a hang
`VideoConfig`/`AudioConfig` plus an encoder factory closure. This is
simulcast: multiple parallel encodes of one source, each advertised as a
catalog rendition, each started only when subscribed.
`SharedVideoSource` (publish.rs:1087-1095) fans one capture thread out to all
active encoders and parks the camera when the subscriber count hits zero.
`PublishCaptureController` (publish/controller.rs:114-125) is app-facing
orchestration over camera/screen/audio toggles.

Catalog production is a thin wrapper, not a reimplementation
(publish.rs:572-585):

```rust
pub struct CatalogProducer(#[debug(skip)] moq_mux::catalog::Producer<crate::catalog::IrohLiveExt>);

impl CatalogProducer {
    pub fn new(broadcast: &mut BroadcastProducer) -> Result<Self> {
        let mut producer =
            moq_mux::catalog::Producer::with_catalog(broadcast, Catalog::default()).anyerr()?;
        // `lock()` only publishes when the catalog is mutated, so touch it once
        // to publish the initial (empty) catalog. ...
        producer.lock().video = Video::default();
        Ok(Self(producer))
    }
```

### What moq has

`moq_mux::catalog::Producer<E: CatalogExt>` (rs/moq-mux/src/catalog/producer.rs:42-65)
publishes the hang `catalog.json` track, its compressed `.z` sibling, and the
MSF `catalog` track together, holding an `Arc<Mutex<Catalog<E>>>` whose `lock()`
guard republishes on drop (producer.rs:140), plus a shared `crate::Clock`, a
reservation gate, and per-rendition timeline producers (now memoized in an
`Arc<Mutex<BTreeMap<..>>>`). This is exactly what our `CatalogProducer` wraps.
`media_producer<C>(track, container)` (producer.rs:209-220) wires the 1:1
default timeline recorder for you.

Above moq-mux sits the track-facing encode layer. `moq_video::encode::Producer`
wraps `moq_mux::codec::h264::Import` (and `h265::Import`) in avc3/hev1 mode:
`publish(packets, Timestamp)` feeds Annex-B access units, the importer parses
parameter sets and registers the catalog rendition itself, and `demand() ->
moq_net::track::Demand` exposes the used/unused signal. `publish_capture` is the
turnkey demand-gated path: track and catalog advertised up front, camera opened
only while `used()`, released on `unused()`. `moq_audio::encode::Producer<E:
CatalogExt>` mirrors it for Opus: one packet per group, rendition registered at
construction, `reset_epoch()` to re-anchor PTS after idle gaps, catalog
rendition removed on drop.

Publish-side catalog machinery in moq-mux:

- `Reserved<E>` / `Rendition<E, C>` (rs/moq-mux/src/catalog/tracks.rs:224-292,
  :300-334): importers reserve renditions by name; the first catalog snapshot is
  withheld until every reservation resolves, so a subscriber's first snapshot is
  the complete track list. A `Rendition` guard retires its catalog entry on drop
  (tracks.rs:413-425). `VideoHint` (tracks.rs:119-140) carries caller-provided
  fields, with stream-detected values winning. The rendition layer is now
  unsealed: the old sealed `Kind`/`Audio`/`Video` markers are gone, replaced by
  the public generic trait `RenditionConfig<E>` (tracks.rs:88-108), with
  `VideoTrack<E>`/`AudioTrack<E>` as aliases over `Rendition<E, C>` (#2420).
- Live catalog `Estimate` and metrics (tracks.rs:15-34, container/jitter.rs):
  `Estimate { jitter: Option<Duration>, bitrate: Option<u64> }` is the set of
  auto-detectable catalog fields; `Rendition::record_frame(ts, bytes)`,
  `record_reorder(reorder)`, and `record_group_end(next)` (tracks.rs:395-410)
  feed the per-rendition `Metrics` detector, auto-filling jitter (from frames and
  reorder) and bitrate (over a one-second group window) only for the fields the
  config left absent. `Rendition::set` captures a config's supplied `Estimate`
  as authoritative and never overwrites it. The publisher thus measures actual
  jitter and bitrate and writes them into the catalog fields a subscriber-side
  ABR selects on.

### Duplication verdict

Against hang/moq-mux there is no duplication: catalog production is wrapped,
track producers are moq-net's, container framing is delegated through
`MoqPacketSink` (moq-media/src/transport.rs:89-122) to
`moq_mux::container::Producer`. Against moq-video/moq-audio the picture is
functional overlap of pattern, not of code: our `run_dynamic` plus
`track.unused()` watcher and `SharedVideoSource` parking implement the same
demand-gated capture semantics as their `publish_capture` and `demand()
.unused()`. Ours operates one level up (broadcast-level dynamic handler
dispatching into a rendition registry) because moq has no Rust simulcast
publish: `moq_video::encode::Producer` publishes exactly one track, and the
only multi-rendition machinery in moq Rust is moq-transcode, which is a JIT
transcoding sidecar (section 5), not a simulcast encoder. So:

- Genuinely iroh-live-specific and kept regardless of alignment: the
  multi-rendition simulcast registry (`VideoRenditions`/`AudioRenditions`,
  roughly 400 LOC of publish.rs), `SharedVideoSource` ref-counted parking
  fan-out, `AudioSourceLease`, and `PublishCaptureController`.
- Pattern duplication that alignment could collapse: the per-track wiring in
  `State::start_track` and the encode pipelines it starts
  (pipeline/video_encode.rs, 249 LOC; pipeline/audio_encode.rs, 155 LOC)
  parallel `publish_capture` and `Producer::publish`/`write`. If the codec
  layer moves to moq-video/moq-audio (sibling document's question), each
  rendition entry becomes a `moq_video::encode::Producer` or
  `moq_audio::encode::Producer` and the sink/import/PTS bookkeeping goes
  away. The simulcast registry then shrinks to "N producers over one shared
  source", which is precisely the shape moq lacks and we could upstream.
- Compensating hacks papering over API sharp edges, both fixable upstream:
  the empty-catalog priming (publish.rs:578-585, quoted above) exists because
  `moq_mux::catalog::Producer` does not publish an initial snapshot until the
  first mutation, so an early subscriber blocks on the catalog track. moq-mux's
  `Reserved` machinery is the principled treatment of initial-publish sequencing
  (withhold until complete, then publish atomically); moving to it replaces our
  hack with the inverse and correct semantics. The dynamic-handler registration
  race workaround (publish.rs:246-252, register `producer.dynamic()`
  synchronously before spawning the task, otherwise `subscribe_track` returns
  NotFound until the task runs) is a moq-net ordering sharp edge worth reporting
  regardless.
- A real gap on our side that moq-mux fills: we advertise the static preset
  bitrate and never populate `jitter` in the catalogs we publish. moq-mux's
  per-rendition `Estimate` detection keeps both fields honest. Browser
  subscribers using the JS ABR select on these fields, so adopting the
  importer-fed metrics (or replicating the feed if we keep our own encoders)
  makes our broadcasts better citizens.

## 3. Subscribe path

### What we have

`RemoteBroadcast` (moq-media/src/subscribe.rs:228-242) wraps a moq-net
`BroadcastConsumer`, a catalog watchable, a `PlaybackPolicy`, per-broadcast
`SubscribeStats`, and a shared `crate::sync::Sync` playout clock. Every
subscription entry point builds the same ordered consumer, for example
`video_rendition` (subscribe.rs:558-560):

```rust
let consumer =
    OrderedConsumer::new(track_consumer, moq_mux::catalog::hang::Container::Legacy)
        .with_latency(max_latency);
```

where `OrderedConsumer` is the crate alias
`moq_mux::container::Consumer<moq_mux::catalog::hang::Container>`
(moq-media/src/lib.rs:31) and `max_latency` comes from
`PlaybackPolicy::max_latency` (moq-media/src/playout.rs:42-54, default
150 ms). Group ordering and latency-bounded skipping happen entirely inside
moq-mux; our `MoqPacketSource` (transport.rs:34-83) just adapts `read()` to
the crate's `MediaPacket`. Decoding runs on dedicated OS threads
(pipeline/video_decode.rs `decode_loop`, :213-409) that gate each frame on
`Sync::wait` or a PTS-cadence `FramePacer`, and deliver through the
single-slot latest-wins `frame_channel` (moq-media/src/frame_channel.rs, 299
LOC), whose `new_sender` (frame_channel.rs:129) is what lets the adaptation
layer swap decoders under a consumer that never changes how it reads.
`VideoTrack` (subscribe.rs:895-903) and `AudioTrack` (subscribe.rs:832-836)
are the handles; `CatalogSnapshot` (subscribe.rs:250-256) carries a sequence
number and quality-based rendition selection helpers (subscribe.rs:807-826).

A note on stale terminology: earlier notes refer to a `WatchTrack` and a hang
`TrackConsumer::set_max_latency`. That was the hang-0.10 model. The current
code has neither; the span-skip machinery lives in `moq_mux::container::
Consumer`, and we configure it only at construction via `with_latency`.

### What moq has

`moq_mux::container::Consumer<F: Container>` has the ordering contract
(rs/moq-mux/src/container/consumer.rs:15-37): frames within a group in arrival
order; across groups advance by sequence, skipping stalled or missing groups
when `newest_available_ts - oldest_pending_ts` exceeds the latency limit;
zero latency skips aggressively; CMAF durations allow early skip; timeline
rewinds are dropped. Latency is set at construction, `with_latency`
(consumer.rs:148), and retuned mid-stream:

```rust
/// Set the maximum latency tolerance.
pub fn set_latency(&mut self, latency: std::time::Duration) {
	self.latency = latency;
}
```

(consumer.rs:479). `discontinuity()` (consumer.rs:161) lets downstream flush
decoder and render buffers on timeline rewinds; our `decode_loop` currently
discovers discontinuities only through decode errors followed by `reset()` and
wait-for-keyframe (video_decode.rs:331-336).

The #2426 per-frame-fragment change clarified the `Container::poll_read`/`read`
contract (container/mod.rs:96-120): only `Ok(None)` ends a group;
`Ok(Some(batch))` may carry an empty `batch`, meaning a wire frame was consumed
but decoded to no media frames (for example a CMAF fragment with zero samples),
which is not end-of-group, so the caller must poll again. The `GroupBuffer`
loop implements the loop-on-empty (consumer.rs:578-582). This is a correctness
trap for our `MoqPacketSource`: it must key completion off `None`, never off an
empty batch. The moq-video/moq-audio `read` loops already do.

Above the container consumer, moq's track-facing decode layer:
`moq_video::decode::Consumer` (rs/moq-video/src/decode/consumer.rs) is `Decoder`
plus `moq_mux::container::Consumer<legacy::Wire>` plus a pending queue, with
`Config::latency_max` forwarded to `with_latency`, and
`moq_audio::decode::Consumer` mirrors it.

### Duplication verdict

The ordering, skipping, and latency machinery is not duplicated; we delegate
it. What our subscribe layer adds over moq's consumers, and what nothing in moq
Rust provides: quality-based rendition selection over the catalog, the
latest-wins frame channel, playout-clock gating, decoder hot-swap, and the
adaptation orchestration (section 5). What their consumers have that ours
lacks: `set_latency` and `discontinuity()`.

On `set_latency` specifically: adaptive rendition switching does not need it,
because every switch builds a fresh consumer (subscribe.rs:1519-1520). What
needs it is latency policy: today changing `PlaybackPolicy::max_latency`
requires resubscribing every track (playout.rs doc at :36-41), and the
planned `PlayoutMode::Auto { min, max }` from phase-3b wants to tune the skip
threshold continuously against measured jitter without ever tearing down the
subscription. That makes mid-stream `set_latency` an enabler for the
jitter/sync work, available on main, pending the pin bump. Flagged.

## 4. Catalog and extensions

### Current state on our side

moq-media/src/catalog.rs (75 LOC) is the whole catalog module:

```rust
pub type Catalog = moq_mux::catalog::hang::Catalog<IrohLiveExt>;
pub type CatalogConsumer = moq_mux::catalog::hang::Consumer<IrohLiveExt>;

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct IrohLiveExt {
    pub chat: Option<Chat>,
    pub user: Option<User>,
}

impl CatalogExt for IrohLiveExt {}
```

(catalog.rs:9-27; `Chat` holds `moq_lite::Track` descriptors, `User` holds
id/name/avatar/color, catalog.rs:32-45.)

The prior audit (plans/old/review-moq-usage.md, finding 1, 2026-06-18) found
we hand-rolled the catalog stack moq-mux already provides: a bespoke
`Catalog` with inlined video/audio copies, a snapshot consumer with a
group-advance race and no delta support, and a bespoke producer. That finding
is resolved: the current code is exactly the recommended shape, a
`CatalogExt` extension over `moq_mux::catalog::hang::Catalog` with the
moq-mux producer and consumer, wire-compatible with base consumers and safe
against upstream enabling merge-patch deltas. Nothing to cut here; 75 LOC is
the floor.

### hang catalog model

The Rust hang catalog is `{ video: Video, audio: Audio }` and nothing else
(rs/hang/src/catalog/root.rs:17-36, `#[non_exhaustive]`). There are no chat,
user, preview, or location sections in Rust hang, and none in JS either: a grep
for chat over `js/hang/src`, `js/watch/src`, and `js/publish/src` is empty; the
only JS references are a README line listing chat as a kind of track a catalog
can describe and a raw-track example. The extension mechanism is documented as
serde-flatten root sections, which is what `CatalogExt` formalizes. `Video`,
`Audio`, `VideoConfig`, and `AudioConfig` are all `#[non_exhaustive]`.

Field-level items that matter to us:

- `jitter` exists on both configs as `pub jitter: Option<std::time::Duration>`
  serialized as integer milliseconds (rs/hang/src/catalog/video/mod.rs,
  audio/mod.rs). This corrects a stale claim in our own code and in
  maps/moq-media.md: sync.rs:18-21 and :99-106 say "our Rust catalog does not
  carry this field yet". The field is there; what was missing is a producer and
  a reader. The producer half is now filled: moq-mux's per-rendition metrics
  auto-detect the value and write it (section 2). We still neither populate it
  from our own encoders nor read it when subscribing (section 6).
- `broadcast: Option<moq_net::PathRelativeOwned>` cross-reference on both
  configs lets a transcoder catalog point passthrough renditions at the source
  broadcast, and is what a rendition-switching subscriber must resolve (moq-mux
  `source.rs`).
- The renditions are keyed in a `BTreeMap<String, VideoConfig>` /
  `BTreeMap<String, AudioConfig>` so JSON Merge Patch works.
- hang renames `to_string` to `to_json`/`to_json_pretty` and adds `displayRatio*`
  serde aliases on `VideoConfig` (#2420, decode-only, emitting `displayAspect*`).
  Both are migration chores for us, the first having bitten us before
  (Deref-to_string gotcha).

### Where do chat and user belong?

Three options. (a) Upstream into hang as first-class sections: upstream
deleted app sections from this line and keeps the root model strictly
media; JS carries no chat either; proposing re-addition runs against their
direction and couples our schema to their release cadence. (b) Keep as a
`CatalogExt` extension: sanctioned, wire-compatible (base consumers ignore
unknown root keys), already implemented, 75 LOC. Now that `RenditionConfig<E>`
is a public generic trait, an app extension could even publish its own catalog
tracks through the same reservation-gating and auto-detect lifecycle the media
configs use (section 2). (c) Move identity out of the catalog entirely into the
room layer (gossip/KV), leaving the catalog media-only. The rooms-overhaul
direction already places identity in the room; the catalog `user` section
remains useful for catalog-only consumers such as the browser watch page.
Verdict: (b) today, with (c) as the room-layer refactor decides; do not pursue
(a).

## 5. Adaptive

### Our subscriber-side ABR, in full

moq-media/src/adaptive.rs (592 LOC including tests) is pure decision logic:

```rust
pub fn evaluate(
    current_idx: usize,
    ranked: &[RankedRendition],
    signals: &NetworkSignals,
    timers: &mut AdaptationTimers,
    config: &AdaptiveConfig,
    now: Instant,
) -> Decision
```

(adaptive.rs:150-157), with `Decision = Hold | Downgrade(usize) | Emergency |
StartProbe(usize)` (adaptive.rs:116-125) and `rank_renditions` sorting by
pixel count descending (adaptive.rs:93-110). The algorithm per 200 ms tick:

1. Emergency: `loss_rate >= 0.20` and not already lowest drops straight to
   the lowest rendition immediately (adaptive.rs:163-168).
2. Downgrade, bandwidth-primary: `bandwidth_stressed = available_bps <
   bitrate_bps * 0.85` OR `loss_rate >= 0.10`; sustained for
   `downgrade_hold = 500ms`, step down one rendition (adaptive.rs:171-185).
3. Upgrade probe: blocked at highest, during `post_downgrade_cooldown = 4s`,
   or during `probe_cooldown = 8s`; otherwise requires
   `available_bps >= next.bitrate_bps * 1.2` AND `loss_rate <= 0.02`
   sustained for `upgrade_hold = 4s`, then `StartProbe` (adaptive.rs:187-220).
4. `should_abort_probe`: loss `>= 0.05` or `congestion_events` above the
   probe-start baseline (adaptive.rs:226-232).

The runtime driver `adaptation_task_v2` (subscribe.rs:1293-1493) applies the
decisions with seamless switching: `switch_rendition_v2`
(subscribe.rs:1497-1533) subscribes the target rendition with a fresh
`OrderedConsumer` and builds a `VideoDecoderPipeline::with_sender` writing
into the shared `FrameSender`, so the new decoder runs in parallel and the
old handle is dropped only once the new one is live. A probe holds both
decoders for `probe_duration = 3s`, committing or aborting on the loss and
congestion signals. Failed switches enter a failure cooldown enforced in the
task (subscribe.rs:1402-1404). Signals arrive as `NetworkSignals { rtt,
loss_rate, available_bps, congestion_events }` (moq-media/src/net.rs:9-16),
populated from iroh `PathStats` (section 7).

### What moq has: supply side only, in Rust

moq Rust has no subscriber-side rendition selection.
`git grep -in "select_rendition|switch_rendition|abr|adaptive" -- rs/` yields
no selection logic; libmoq subscribes by catalog index with a "TODO: a future
API will pick the right rendition"; moq-cli takes static rendition flags;
moq-mux's `catalog::Select` is a static narrowing filter. The only ABR consumer
is JS: `js/watch/src/video/source.ts` reads `connection?.recvBandwidth`, applies
a flat 0.8 safety margin, and ranks renditions. That heuristic has no hold
timers, no loss input, no probes, and no failure cooldown; ours is strictly
richer.

What moq Rust does have is the supply side. `moq-transcode` is JIT publish-side
ABR:

```rust
pub async fn run(
	source: moq_net::broadcast::Consumer,
	mut output: moq_net::broadcast::Producer,
	config: Config,
) -> Result<(), Error>
```

(rs/moq-transcode/src/lib.rs:45-48) with `Rung { height, bitrate }` and
`Config { rungs, source, encoder, decoder }` (config.rs:31-58, default ladder
1080p/5M down to 240p/350k at config.rs:65-71). The catalog is published
immediately and deterministically before any encoder exists; nothing is encoded
until a subscriber asks; one shared decode fans out to all active rungs; and,
critically for a switcher, "Output groups mirror source sequence numbers 1:1,
so group N of every rung is the same content as source group N" (lib.rs:1-22,
rung.rs:12-14), with a bounded fetch path (`MAX_CONCURRENT_FETCHES = 4`,
rung.rs:33) so a switching player can backfill past groups on the new rung.
There is also encoder-side rate control
(`moq_video::encode::rate::Policy`/`Control`, #2303) adapting the encode
bitrate to `moq_net::bandwidth::Consumer`, which is publish-side congestion
response, orthogonal to rendition selection.

### Complementarity and the upstreaming shape

moq-transcode mints the ladder; our adaptive.rs picks the rung. They are the
two halves of one system, and ours is the half moq punts to JS. Upstreaming
it would look like:

- The pure policy (`RankedRendition`, `rank_renditions`, `AdaptiveConfig`,
  `AdaptationTimers`, `evaluate`, `should_abort_probe`, ~340 LOC plus tests)
  is transport-free and codec-free; its only type dependencies are
  `hang::catalog::VideoConfig` and a signals struct. Natural home: moq-mux's
  catalog module next to `Select` (it is catalog-level reasoning), or a small
  new crate if moq-mux wants to stay policy-free.
- The driving loop (parallel staging consumer, decoder swap, probe lifecycle)
  needs a decode layer to swap, so in moq terms it belongs in
  `moq_video::decode` as a switcher over `decode::Consumer`, which exists on
  main.
- Consumer API requirements, checked against main: (a) opening a second
  parallel `container::Consumer` on another rendition: available, track
  subscriptions are independent. (b) Decode timestamps for probe evaluation
  and swap alignment: available, `decode::Frame.timestamp` rides through
  reordering. (c) A bandwidth estimate: available;
  `moq_net::bandwidth::{Producer, Consumer}` is exposed on the receive side, not
  just encode-side #2303: `Session::recv_bandwidth()` carries
  `Option<bandwidth::Consumer>`, `moq_native::Reconnect` exposes
  `recv_bandwidth()` across reconnects, and moq-gst already consumes both
  directions. So the subscriber estimate our `available_bps` approximates exists
  upstream. (d) Loss rate and congestion events: not exposed by moq-net; an
  upstreamed ABR would run bandwidth-primary with loss as an optional injected
  signal, which our `evaluate` already supports since it takes the signals
  struct by reference. (e) Mid-stream `set_latency`: not required for switching
  (fresh consumer per switch) but required for the companion latency policy,
  section 3.
- One improvement upstreaming would unlock that our current switcher lacks:
  with moq-transcode's 1:1 group mirroring plus the fetch path, a switch can
  backfill the current group on the target rung instead of waiting for the
  next keyframe boundary, using `timeline` tracks to find it. Our switcher
  today just subscribes and waits for the first keyframe.

Verdict: keep adaptive.rs unchanged short-term; propose it upstream as the
missing Rust demand side once we bump to the released main line, policy first.

## 6. Sync and playout

### What we have

moq-media/src/sync.rs (420 LOC) is a deliberate port of moq-js
`js/watch/src/sync.ts` at `53fe78d8` (module doc sync.rs:1-40). `Sync` is an
`Arc<SyncInner>` holding a base `Instant`, a `Mutex<SyncState>`, and a
`Condvar` (sync.rs:56-85). The model, all in `i64` milliseconds: `reference`
is the earliest `now_ms - pts_ms` ever observed and only tightens
(`received`, sync.rs:151-164, called on the video receive path only);
`latency_ms = max(audio_ms, video_ms) + jitter_ms` with a 100 ms default
jitter; `wait(pts)` (sync.rs:187-224) computes `sleep = (reference - (now -
pts)) + latency` and blocks the decode OS thread on
`Condvar::wait_timeout`, re-evaluating when reference or latency change and
returning `false` on close. Audio never calls `received` or `wait`; it paces
via its sink ring buffer, making audio the effective master.
moq-media/src/playout.rs (92 LOC) is the policy layer: `SyncMode::{Synced,
Unmanaged}` and `PlaybackPolicy { sync, max_latency }` (playout.rs:42-54),
the latter feeding the container consumer's skip threshold.

The `audio_ms`/`video_ms` fields are currently dead (sync.rs:99-106), and as
established in section 4 the stated reason is stale: the catalog `jitter`
field exists. The actual gaps were that nothing populated it and we did not
read it. The producing half is now closed by moq-mux's per-rendition metrics,
which auto-detect jitter and write it into the catalog `jitter` field
(section 2); the reading half is still ours to wire.

### The moq-mux `Estimate` and the sync story

The round-1 version of this document flagged, as a gap, that Rust hang lacked a
per-codec jitter field our sync.rs needs. That gap is now narrower. moq-mux
exposes `Estimate { jitter: Option<Duration>, bitrate: Option<u64> }`
(tracks.rs:15-34) and computes both per rendition from the frame stream
(`record_frame`/`record_reorder`/`record_group_end`, tracks.rs:395-410),
feeding them straight into the catalog `VideoConfig`/`AudioConfig` `jitter` and
`bitrate` fields via `Rendition::set`/`update`. So a subscriber that reads
`catalog.video.renditions[name].jitter` now gets a publisher-measured jitter
bound rather than nothing. This is exactly the value our playout clock wants to
seed `audio_ms`/`video_ms` (and `jitter_ms`) with, when the publisher is a
moq-mux-based one that populates it. The estimate is a jitter bound on the
publish side, not a full receiver jitter buffer, so it seeds our clock rather
than replacing sync.rs; but the input side of the phase-3b design is no longer a
missing upstream primitive.

### What moq has for playout

No playout clock exists in moq Rust. The old `moq-clock` demo crate is a JS-only
survivor (`js/clock`, `js/moq-clock`) and was a wall-clock broadcast demo, not a
playout clock, so there is nothing to compare even historically. The only sync
implementation upstream is the JS one we ported (`js/watch/src/sync.ts`). On the
Rust side the closest mechanisms are the container consumer's latency-bounded
group skipping (a coarser, drop-oriented mechanism) and
`moq_audio::decode::Config::latency_max`'s doc note that a companion
`latency_min` for jitter-buffer padding "will land in a follow-up", which is an
acknowledgement of the same gap. As for the catalog `jitter` field: moq-mux
writes it, JS reads it (sync.ts), and no Rust consumer reads it.

### Verdict

sync.rs plus playout.rs is our second Rust-side gap-filler, and an upstream
candidate. Target location: moq-mux, next to `container::Consumer`, because
the clock is codec-free, must be shared across the audio and video paths, and
complements the consumer's skip machinery (skip decides what to drop, the
clock decides when to render what survived); moq-video and moq-audio decode
consumers would then optionally gate on it, and it would seed `audio_ms`/
`video_ms` from the catalog `jitter` fields moq-mux now populates.
Independent of upstreaming, two local fixes fall out of this comparison:
update the stale sync.rs doc claims, and wire catalog `jitter` into the clock
when present.

## 7. Stats

Three different things share the word "stats"; only one is ours.

- Ours, subscriber-side congestion sampling: iroh-live/src/util.rs:55-105
  samples the selected iroh path every 200 ms, computes a delta-based loss
  rate from `lost_packets`/`udp_tx.datagrams`, and derives `available_bps =
  cwnd * 8 / rtt` (util.rs:86-90), publishing `NetworkSignals` for the
  adaptive loop. moq-media/src/stats.rs (494 LOC) is a separate typed
  metrics/overlay layer (`NetStats` with `rtt_ms`, `loss_pct`,
  `bw_down_mbps`, stats.rs:242-251, plus encode/render/timing categories)
  for the debug UI.
- moq-stats: relay traffic accounting published as MoQ broadcasts, cumulative
  `Traffic { bytes, frames, groups, subscriptions, fetches, datagrams, ... }`
  counters per `Tier` (rs/moq-net/src/stats.rs:228-262) drained from a
  `moq_net::stats::Registry` and encoded as merge-patch snapshot tracks. `Tier`
  is now a bare `PathOwned` label after #2411 removed the internal defaults
  (stats.rs:350); the producer config type was renamed `Config` ->
  `ProducerConfig` (rs/moq-stats/src/produce.rs:22-49); the `fetches` (#2427)
  and `datagrams` (#2430) fields were added. There is no rtt, loss, cwnd, or
  bandwidth estimate anywhere in it. It is a billing/dashboard exporter and does
  not overlap with our purpose.
- moq-net session stats (#2348): the `Registry`/`Traffic` counters themselves,
  session-level delivery accounting. Also not a congestion signal. The
  congestion-relevant surface moq-net does expose is `bandwidth::Consumer`
  including the subscriber-side `recv_bandwidth` (section 5), which is the
  transport's own estimate rather than our cwnd/rtt approximation.

Conclusion: our stats layer stays. On a pin bump, the `available_bps`
derivation in util.rs can be replaced by consuming `recv_bandwidth` where the
session provides it, with the iroh transport feeding a `bandwidth::Producer`
from `PathStats` in our web-transport glue; that retires roughly 20 LOC of
estimation math and, more importantly, aligns our signal with what the JS ABR
and moq-gst already consume. What we would ask upstream for: loss-rate and
congestion-event signals alongside the bandwidth estimate (or a documented
extension point on the session for transport-specific signals), since bandwidth
alone cannot drive our emergency and probe-abort rules.

## 8. Chat

Ours: moq-media/src/chat.rs (182 LOC). One message per group, one UTF-8
frame per group (chat.rs:1-9), `ChatPublisher::send` writing via
`TrackProducer::write_frame` (chat.rs:61-70), `ChatSubscriber::recv` reading
groups sequentially (chat.rs:92-115), track advertised through the catalog
`chat` extension section (section 4). The rooms-overhaul plan moves the
payload to JSON for sender identity and message ids; the wire mechanism
(single-frame groups on a dedicated track) stays.

moq: no chat implementation in Rust, and none in JS source (grep over js/hang,
js/watch, and js/publish src is empty; only a README mention and a raw-track
example, section 4). Verdict: the extension-section plus dedicated-track
approach keeps chat entirely ours at trivial cost; nothing to align, nothing to
upstream, keep.

## 9. Capability matrix

| Capability | iroh-live (moq-media) | moq main (3a3e0ea8) |
|---|---|---|
| Broadcast publish (tracks + catalog) | yes; `LocalBroadcast` over moq-net + moq-mux | yes; `BroadcastProducer` + `moq_mux::catalog::Producer` + codec importers |
| Multi-rendition simulcast publish | yes; `VideoRenditions`/`AudioRenditions`, demand-started per rendition | no; one track per `encode::Producer`; JIT ladder via moq-transcode sidecar instead |
| Catalog produce/consume live | yes; wraps `moq_mux::catalog::{Producer, hang::Consumer}` | yes; hang + `.z` + MSF tracks, `Reserved`/`Rendition` initial-publish gating |
| Catalog extensions (app sections) | yes; `IrohLiveExt` chat/user via `CatalogExt` | mechanism yes (`CatalogExt`, flatten, public `RenditionConfig<E>`); no app sections shipped |
| Container legacy/CMAF | Legacy only in our pipelines | yes; legacy, fMP4/CMAF (with passthrough timelines), LOC, flv, mkv, ts, hls |
| Group ordering + latency skip | delegated to `moq_mux::container::Consumer` | yes (consumer.rs:15-37), kio-based poll |
| Mid-stream latency change | no; resubscribe required | yes; `set_latency` (consumer.rs:479) |
| Discontinuity signal | no; inferred via decode error + reset | yes; `discontinuity()` (consumer.rs:161) |
| Demand-gated encode/capture | yes; `run_dynamic` + `SharedVideoSource` parking | yes; `publish_capture` + `track::Demand` `used()`/`unused()` |
| Supply-side ABR ladder (JIT transcode) | no | yes; moq-transcode, 1:1 group mirroring, fetch backfill |
| Subscriber-side ABR | yes; adaptive.rs + adaptation_task_v2, loss + bandwidth, probes | no in Rust (JS only, recvBandwidth * 0.8) |
| Playout clock / A/V sync | yes; sync.rs port of sync.ts, audio-master | no Rust equivalent; JS sync.ts; catalog jitter written by metrics |
| Congestion stats for ABR | yes; PathStats -> loss, available_bps, congestion events | partial; `recv_bandwidth` estimate, no loss/congestion signal |
| Publish-side catalog bitrate/jitter measurement | no; static preset values | yes; moq-mux per-rendition `Estimate` (jitter.rs) |
| Chat / user metadata | yes; chat.rs + catalog ext | no |
| Playback / audio sink (device output) | yes; audio_backend (cpal + AEC), sink-paced audio | no (capture only, incl. system audio) |

## 10. Verdicts

Per area:

- Publish path: keep the simulcast registry, `SharedVideoSource`, source
  leasing, and controller (no moq equivalent). The per-track encode wiring
  and encode pipelines (~500 LOC across publish.rs `start_track` plumbing and
  pipeline/{video,audio}_encode.rs) are adopt-theirs-contingent: they
  collapse onto `moq_video`/`moq_audio` `encode::Producer` if the codec-layer
  comparison lands on adoption, and the surviving simulcast layer ("N
  producers, one source, demand-gated") becomes an upstream candidate for
  moq-video. Cut now: nothing. Cut on adoption: the empty-catalog priming
  hack (publish.rs:578-585) replaced by `Reserved` semantics; report the
  dynamic-handler registration race (publish.rs:246-252) upstream either way.
  Adopt: importer-fed catalog metrics so our advertised bitrate and jitter are
  measured, not asserted.
- Subscribe path: keep. Ordering and skipping are already delegated; our
  additions (quality selection, frame_channel, decoder hot-swap, sync gating)
  have no upstream counterpart. Fix the stale `adaptation_task` doc comment
  (subscribe.rs:1281-1287). Respect the #2426 empty-batch contract in
  `MoqPacketSource` (complete on `None`). Enablers to adopt on the pin bump:
  `set_latency` for runtime `PlaybackPolicy`/`PlayoutMode` retuning,
  `discontinuity()` for clean decoder flushes.
- Catalog: keep as is (75 LOC, correct `CatalogExt` usage; audit finding 1 is
  resolved). Chat and user stay an extension; do not upstream them. Migration
  chores on the bump: `to_json` rename, `non_exhaustive` construction,
  `Duration` jitter typing, `broadcast` cross-reference awareness in rendition
  handling, and the option of publishing app tracks through `RenditionConfig<E>`.
- Adaptive: keep, then upstream. This is the single biggest Rust-side gap we
  fill (no subscriber-side selection exists in moq Rust). Upstream the pure
  policy (~340 LOC of adaptive.rs) toward moq-mux catalog (next to `Select`)
  or a small dedicated crate, and the switching driver as a
  `moq_video::decode` switcher, which now has a decode side to attach to.
  Target moq-transcode's 1:1 group contract and fetch path for
  boundary-free switching.
- Sync/playout: keep, then upstream; the second biggest gap (no Rust playout
  clock upstream, acknowledged by moq-audio's deferred `latency_min`).
  Target: moq-mux next to `container::Consumer`. Local fixes now: correct
  the stale "catalog lacks jitter" doc, and read catalog `jitter` (now
  publisher-populated by moq-mux's `Estimate`) into `audio_ms`/`video_ms`.
- Stats: keep both layers (overlay metrics and signal sampling); they do not
  overlap moq-stats or moq-net session stats. On the bump, replace the
  `available_bps` cwnd math (~20 LOC, util.rs:85-90) by feeding and consuming
  `moq_net::bandwidth` and keep PathStats for loss and congestion. Upstream
  ask: loss and congestion signals on the session alongside `recv_bandwidth`.
- Chat: keep (182 LOC); no counterpart anywhere upstream.

Direct cut total in this layer today is small, under ~100 LOC (the hacks,
stale docs, and estimation math), because the layer already wraps rather than
duplicates moq-mux; the substantive reductions tied to this layer (~500 LOC
of encode wiring, plus the 1,212 LOC pipeline module partially) are
contingent on the codec-layer adoption decision, and the layer's main
contribution to the refactor is the upstream and enabler list.

The two biggest Rust-side gaps we fill: subscriber-side ABR (adaptive.rs plus
adaptation_task_v2) and the playout clock (sync.rs plus playout.rs). The
enablers we would depend on are all on moq main now, pending the release bump:
`container::Consumer::set_latency` (mid-stream latency retuning),
`moq_net::bandwidth::Consumer` with subscriber-side `recv_bandwidth` (ABR
input), `Reserved`/`Rendition` atomic catalog gating (replaces the priming
hack), per-rendition `Estimate` metrics feeding catalog jitter and bitrate,
`discontinuity()` signaling, the `broadcast` cross-reference field, the #2426
empty-batch contract to respect, and moq-transcode's 1:1 group mirroring with
fetch backfill.
