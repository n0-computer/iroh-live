# Map: moq main — moq-transcode, moq-stats, hang + moq-mux (HEAD 3a3e0ea8, 2026-07-21)

SOURCE: moq main, HEAD 3a3e0ea8 (2026-07-21); dev merged into main.

Repo: `/home/bit/Code/rust/moq`. Read blobs with `git show 3a3e0ea8:<path>`; paths are
repo-relative.

Provenance and what changed since the pre-merge analysis (dev SHA 261c2048):

- **moq-transcode**: source logic **unchanged**. `git diff 261c2048..main -- rs/moq-transcode`
  touches only `Cargo.toml` (dropped three deps), `README.md`, and `examples/transcode.rs`
  — and those two are just the moq-net broadcast API migration
  (`origin.publish_broadcast(path, &output)` became
  `origin.create_broadcast(path, moq_net::broadcast::Route::new().with_announce(true))`).
  Every `src/` citation below is exact against main. Still version 0.0.1.
- **moq-stats**: bumped to **0.1.0** with breaking changes. #2427 "collect traffic counters
  in the model layer" and the HEAD commit #2430 "count datagrams in the model layer" added
  fields to `Traffic`; #2411 "remove internal tier defaults". Config type renamed
  `Config` -> `ProducerConfig`. Sections below are re-verified against main.
- **moq-mux**: bumped to **0.7.6**. The catalog rendition layer was reworked by #2420
  ("unseal catalog renditions + explicit shareable timelines"), #2425 ("shared video-import
  catalog helper"), #2426 ("per-frame fragments without waiting for successor"), and #2428
  ("emit timelines from CMAF passthrough"). Sections re-verified with current types + line
  numbers.
- **hang**: essentially unchanged since 261c2048. Only diff is the `displayRatio*` serde
  alias (#2420); noted below. Still version 0.19.5.

There is no live main-vs-dev split anymore; the whole native media stack is on main.

---

## moq-transcode

Crate: `rs/moq-transcode`, version 0.0.1. Files: `src/lib.rs`, `src/config.rs`,
`src/catalog.rs`, `src/feed.rs`, `src/rung.rs`, `src/error.rs`, plus `examples/transcode.rs`.
Origin: #2140 (just-in-time transcoding for hang broadcasts, NVENC-capable) + #2158 (decode
once per source, GPU resize fanout, `moq transcode` verb).

### Model

`rs/moq-transcode/src/lib.rs:1-22` (module doc) is the definitive statement:

```rust
//! Just-in-time live transcoding for hang broadcasts.
//!
//! [`run`] consumes a source broadcast and fills a derivative broadcast: a
//! catalog advertising lower renditions (rungs) of the source video plus
//! references back to the source renditions, and one output video track per
//! rung. The catalog is published immediately and deterministically (codec
//! strings are computed from the ladder, not the bitstream), but nothing is
//! encoded until a subscriber actually asks:
//!
//! - Subscribing to a rung attaches it to a shared live decode of the source
//!   (one subscription and one decoder per source, no matter how many rungs
//!   are active); each rung resizes and encodes its own copy, group for group,
//!   stopping when the last subscriber leaves.
//! - Fetching a specific group fetches that same group from the source and
//!   transcodes just that group. Output groups mirror source sequence numbers
//!   1:1, so group N of every rung is the same content as source group N.
```

Entry point (`rs/moq-transcode/src/lib.rs:45-48`):

```rust
pub async fn run(
	source: moq_net::broadcast::Consumer,
	mut output: moq_net::broadcast::Producer,
	config: Config,
) -> Result<(), Error>
```

`run` flow (lib.rs:49-150):
1. Creates a `moq_mux::catalog::Producer` on the output broadcast and grabs `output.dynamic()`
   (lib.rs:52-54) — consumers asking for a rung track before it exists queue in the dynamic handler.
2. Subscribes the source `catalog.json` and loops `moq_mux::catalog::hang::Consumer` until a
   snapshot has a transcodable video rendition (`catalog::choose_source`, lib.rs:57-71).
3. `catalog::resolve_rungs` sizes the configured ladder against the source (lib.rs:72).
4. Builds one shared `feed::Feed` for the source track (lib.rs:76-81).
5. Publishes the derivative catalog immediately, before any encoder exists (lib.rs:84-92).
6. Serve loop (lib.rs:98-137): `dynamic.requested_track()` spawns `rung::serve` for known rung
   names (rejects others with `moq_net::Error::NotFound`); source catalog updates re-run
   `catalog::populate` (rung set is fixed at startup, passthrough entries track the source).

### Rung and Config (quoted verbatim)

`rs/moq-transcode/src/config.rs:9-25`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rung {
	/// Output height in pixels. Rounded down to even (I420 chroma is 2x2).
	pub height: u32,

	/// Target bitrate in bits per second: the CBR target and the bitrate
	/// advertised in the derivative catalog.
	pub bitrate: u64,
}

impl Rung {
	/// A rung at `height` pixels and `bitrate` bits per second.
	pub fn new(height: u32, bitrate: u64) -> Self {
		Self { height, bitrate }
	}
}
```

`rs/moq-transcode/src/config.rs:31-58`:

```rust
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	pub rungs: Vec<Rung>,
	pub source: Option<PathRelativeOwned>,
	pub encoder: moq_video::encode::Kind,
	pub decoder: moq_video::decode::Kind,
}
```

Default ladder (`config.rs:65-71`): `1080p/5M, 720p/2.5M, 480p/1.2M, 360p/600k, 240p/350k`, top
rung first, filtered at runtime so only strictly-lower-than-source renditions are offered
(`config.rs:34-38`: dropped when height exceeds source, when bitrate is not below a known source
bitrate, or when height matches the source without a known source bitrate to undercut — "A 480p
source is never transcoded up to 720p").

`Config::source` is the cross-broadcast reference wiring (`config.rs:41-47`): a relative path
(e.g. `".."` when the output is at `<source>/transcode.hang`) that makes the derivative catalog
reference all source renditions through the hang `broadcast` catalog field, "so players fetch
them from the source directly; the transcoder never proxies or subscribes them."

There is a private resolved form (`rs/moq-transcode/src/catalog.rs:10-18`): `pub(crate) struct
Resolved { name: String /* "video/360p" */, size: moq_video::Size, bitrate: u64, framerate: u32 }`.
Track names are `format!("video/{height}p")` (catalog.rs:82).

### Catalog advertising

- `choose_source` (catalog.rs:22-33): picks the highest-resolution decodable rendition
  (H.264/H.265/8-bit-4:2:0 AV1) that is local to the source broadcast (`config.broadcast.is_none()`).
- `rung_entry` (catalog.rs:96-116): the rung's `VideoConfig` codec string is computed from the
  ladder, not the bitstream — avc3 (in-band parameter sets), High profile 0x64, level from a
  Table A-1 lookup (`h264_level`, catalog.rs:174-200) — "so the catalog can be published before
  any encoder exists and stays deterministic."
- `populate` (catalog.rs:122-169): resets `out.video`/`out.audio`, copies display/rotation/flip
  from the source, inserts rung entries, then (when `Config::source` is set) re-inserts every
  local source video and audio rendition with `config.broadcast = Some(rel)` as passthrough
  references.

### feed.rs: decode once, fan out

`rs/moq-transcode/src/feed.rs:1-13` (module doc): "one source subscription and one decoder per
broadcast, fanned out to every rung with live demand. ... A [`Feed`] decodes each group once and
broadcasts the frames; each rung resizes and encodes its own copy (cheap: GPU frames are
refcounted). The decode loop runs only while at least one [`Listener`] exists."

Mechanics:
- `pub(crate) enum Item { Group(u64), Frame(Arc<moq_video::decode::Frame>), End, Finished, Lagged }`
  (feed.rs:30-44); frames are `Arc`-cloned, "a GPU frame stays on the GPU for every receiving rung."
- `Feed::listen()` (feed.rs:86-106) increments a listener count and lazily spawns the decode task
  over a `tokio::sync::broadcast` channel with `CAPACITY: usize = 16` (feed.rs:26); a rung that
  falls behind gets `Item::Lagged` (from `RecvError::Lagged`, feed.rs:122) and abandons the group
  rather than stalling other rungs. `Drop for Listener` (feed.rs:128-140) aborts the decode task
  when the last listener detaches, releasing the source subscription and decoder.
- The decode loop (feed.rs:166-198) decodes at the stream's native size; per-rung sizing happens
  on the rung side via `Frame::resize`. GPU path per lib.rs:18-22: NVDEC decodes/scales in
  hardware, NVENC encodes the CUDA frame in place, no CPU copies; other decoders scale on CPU.

### rung.rs: on-demand — encode only while subscribed

`rs/moq-transcode/src/rung.rs:1-14` (module doc): "Nothing is encoded until someone asks, via the
two demand paths moq-net exposes on the output track: a live subscription (`used`) starts a live
loop ... until the track goes `unused` again; a fetch of a specific group (`requested_group`)
fetches that same group from the source and transcodes just that group with a fresh encoder."

The internal per-rung server context (`rung.rs:36-51`):

```rust
#[derive(Clone)]
pub(crate) struct Rung {
	pub info: Resolved,
	pub source: moq_net::track::Consumer,
	pub feed: Feed,
	pub broadcast: moq_net::broadcast::Consumer,
	pub config: VideoConfig,
	pub encoder: moq_video::encode::Kind,
	pub decoder: moq_video::decode::Kind,
}
```

Details:
- `live` (rung.rs:98-214): parks on `producer.demand().used()`; on demand attaches a feed
  `Listener` + fresh encoder ("rate control persists across groups, while every group still opens
  with a forced IDR"); on `demand.unused()` drops both, releasing the shared decode when last.
  Output groups mirror source sequence numbers (`moq_net::group::Info { sequence }`, rung.rs:146),
  with a documented two-writer race against a concurrent fetch handled via `Error::Duplicate`
  (rung.rs:149-158).
- `fetches` (rung.rs:223-253): `MAX_CONCURRENT_FETCHES: usize = 4` semaphore (rung.rs:33) bounds
  concurrent one-shot decode+encode pipelines (hardware encoders expose few sessions; a
  rendition-switching player bursting past-group fetches must not starve live viewers).
- `Pipeline` (rung.rs:367-371): `struct Pipeline { decoder, encoder, size }` with
  `decode::Config::resize = Some(rung.info.size)` so NVDEC scales in hardware; frames at other
  sizes get `Frame::resize` (rung.rs:360-366).
- GOP: keyframes forced at every group boundary; encoder `gop = framerate * 8` is only a backstop
  (rung.rs:68-70).

### Where it runs

Standalone library (`moq_transcode::run(source_consumer, output_producer, config)`) plus a CLI
verb: `rs/moq-cli/src/transcode.rs:1-8` — "The `transcode` verb: consume a source broadcast and
publish a just-in-time transcoded ladder next to it. The derivative appears at
`<broadcast>/transcode.hang` (or `--output`)". The CLI subscribes to the source through the relay
and publishes the derivative back through the same session (`transcode.rs:53-55`). It is a sidecar
process (deployable next to the publisher OR next to/inside a relay node with a GPU), not
publisher-encoder ABR and not relay-integrated logic. `Error` type:
`rs/moq-transcode/src/error.rs:6-39` (`NoSource`, `SourceDimensions`, transparent
Net/Mux/Hang/Video, `TimeOverflow`, `Scale`).

### CRITICAL: publish-side vs subscriber-side — complementary, not overlapping

**moq-transcode is strictly publish-side ABR**: it *produces* the multi-rendition ladder and
advertises it in the catalog. It contains zero selection logic (beyond `choose_source`, which
picks which *input* to transcode from).

**moq has NO subscriber-side rendition selection in Rust.** Evidence at 3a3e0ea8:
- `git grep -in "select_rendition|switch_rendition|abr|adaptive" -- rs/` yields no selection
  logic; the only ABR mentions are NVENC headers, changelogs, and a libmoq doc TODO.
- `rs/libmoq/src/audio.rs`: subscription is by catalog *index* — "The catalog `index` identifies
  which audio rendition to subscribe ... TODO: a future API will pick the right rendition". Same
  for video (`rs/libmoq/src/video.rs`).
- `rs/moq-cli/src/subscribe.rs`: rendition selection is static CLI flags (exact name or
  codec-family filter), no bandwidth logic.
- `rs/moq-mux/src/catalog/select.rs` (`Select`) is a static narrowing filter for exporters, not
  an adaptive switcher.

**The JS side DOES have it** — the asymmetry is real. `js/watch/src/video/source.ts`:
- `source.ts` "Manual selection by name — skip all ABR logic."
- "Auto-select: use recv bandwidth if no explicit bitrate target" — reads
  `connection?.recvBandwidth` (the QUIC receive-bandwidth estimate signal) and applies
  `const safeBitrate = Math.round(estimate * 0.8); // Apply a safety margin (80%) to avoid
  oscillation`, then runs a filter/ranking pipeline (`byPixels`/`byDimensions`/`byBitrate`).
- `js/watch/src/ui/components/quality.ts`: "The Quality tab: pick a video rendition or let ABR
  choose automatically" / "auto · currently X" / "adapts to bandwidth".

**Comparison with iroh-live**: iroh-live's `moq-media/src/adaptive.rs` (subscriber-side,
bandwidth-primary selection with loss thresholds, asymmetric downgrade/upgrade timers, seamless
decoder swap) has no Rust counterpart in moq — it fills exactly the gap moq punts to JS. The two
systems are complementary: moq-transcode is the supply side (mint the rungs, encode lazily),
iroh-live adaptive.rs is the demand side (pick the rung). moq-transcode's design anticipates a
switching consumer: output group N of every rung is the same content as source group N precisely
"so a fetch for output group N maps to source group N and a player switching renditions lands on
the same content" (rung.rs:12-14), and the fetch path exists so a switcher can backfill past
groups on the new rung. iroh-live could target this 1:1-sequence contract for seamless switching.
The JS 0.8-bandwidth-margin heuristic is much cruder than iroh-live's planned loss-threshold +
hold-timer state machine.

---

## moq-stats

Crate: `rs/moq-stats`, version **0.1.0**. Files: `src/lib.rs`, `src/produce.rs`, `src/consume.rs`.
The counter collection itself lives in `rs/moq-net/src/stats.rs` and is re-exported
(`rs/moq-stats/src/lib.rs:59-62`: `pub use moq_net::stats::{Handle, Presence, Registry, Role,
Tier, Traffic};`). Deps (Cargo.toml): moq-json, moq-net, serde, serde_json, thiserror, tracing,
web-async — no media deps.

### Model: on-the-wire stats, published as MoQ broadcasts

The key architectural fact: moq-stats publishes *server/relay traffic accounting* as MoQ tracks on
the wire, for remote aggregators. `rs/moq-stats/src/lib.rs:1-10`:

```rust
//! Publish and consume MoQ traffic stats.
//!
//! `moq-net` collects per-session traffic counters in a
//! [`stats::Registry`](moq_net::stats::Registry); this crate turns that
//! registry into MoQ broadcasts and back:
//!
//! - [`Producer`] drains a registry on an interval and publishes the counters
//!   as JSON tracks on an origin.
//! - [`Consumer`] subscribes to one published stats broadcast and yields typed
//!   frames, for aggregators, dashboards, and billing meters.
```

Wire format (lib.rs:12-49):
- One broadcast per node at `<prefix>/node/<node>` (default prefix `.stats`; node may be
  multi-segment, e.g. `sjc/1`). A grouping `depth` splits into `<prefix>/<group>/node/<node>`
  per leading broadcast-path segments; parse announce paths back with `parse_node_path`
  (returning `NodePath { group, node }`).
- Traffic is bucketed by `Tier` — "an arbitrary label chosen by business logic: billing class,
  region, ..." (lib.rs:21-23). Default tier unprefixed; named tiers prefix track names.
  Note #2411 removed the internal tier defaults; a `Tier` is now just a path-like label
  (`moq_net::stats::Tier(PathOwned)`, stats.rs:350).
- Per tier: `publisher.json` (egress) / `subscriber.json` (ingress) traffic tracks + a
  `sessions.json` track, each with a compressed `.z` sibling. Names via `traffic_track(tier,
  role, compressed)` / `sessions_track(tier, compressed)` (lib.rs helpers).
- Compression: `<name>.json.z` is "encoded with [`moq_json::snapshot`] (group-scoped DEFLATE
  plus RFC 7396 merge-patch deltas). Since successive stats frames are nearly identical, this is
  a fraction of the plain track's bytes" (lib.rs:31-38). The plain track is the same
  `moq_json::snapshot::Producer` with compression off, wire-identical to one full JSON frame per
  group.
- Counters are cumulative and monotonic; "a downstream aggregator computes rates from successive
  snapshots" and a decrease means restart/GC (lib.rs:44-49). Entries emitted while live or on
  change, dropped once fully closed.

Frame types (lib.rs:65-69):

```rust
/// One frame off a traffic track: cumulative counters keyed by broadcast path.
pub type TrafficFrame = BTreeMap<String, Traffic>;

/// One frame off a sessions track: connect/disconnect gauges keyed by auth root.
pub type SessionsFrame = BTreeMap<String, Presence>;
```

### Metric set (quoted from moq-net) — UPDATED: `fetches` + `datagrams` are new

`rs/moq-net/src/stats.rs:228-262` (current), with the two fields added since 261c2048 by #2427
(model-layer counters) and #2430 (datagrams):

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Traffic {
	/// Cumulative broadcast announce events on this slice.
	pub announced: u64,
	/// Cumulative broadcast unannounce events on this slice.
	pub announced_closed: u64,
	/// Cumulative announce-control bytes: the broadcast name length summed
	/// over each announce and unannounce. Distinct from `bytes` (payload).
	pub announced_bytes: u64,
	/// Per-(broadcast, session) subscription sentinel opens: the first active
	/// subscription a session holds on a broadcast.
	pub broadcasts: u64,
	/// Sentinel closes: the session's last subscription to the broadcast ended.
	pub broadcasts_closed: u64,
	/// Cumulative track-level subscriptions opened.
	pub subscriptions: u64,
	/// Cumulative track-level subscriptions closed.
	pub subscriptions_closed: u64,
	/// Cumulative one-shot group fetches requested. Counted once per coalesced fetch
	/// when the fetch is issued, so one that resolves to `NotFound` still counts.
	/// Separate from `subscriptions` and the viewer refcount. Fetched payload still
	/// flows into `bytes`/`frames`/`groups`.
	pub fetches: u64,
	/// Cumulative payload bytes.
	pub bytes: u64,
	/// Cumulative frames delivered.
	pub frames: u64,
	/// Cumulative groups delivered.
	pub groups: u64,
	/// Cumulative single-frame groups delivered over an unreliable QUIC datagram.
	/// A subset of `groups`: each one also counts there and its payload in
	/// `frames` / `bytes`.
	pub datagrams: u64,
}
```

Counters are bumped from RAII guards on `Registry` handles; the datagram path bumps via
`Registry::datagram(n)` (stats.rs:991), "A datagram stands in for the group it represents, so it
also increments `groups`/`frames`/`bytes` alongside `datagrams`." `Presence` (stats.rs:319):
`pub sessions: u64, pub sessions_closed: u64` with `active()`. `Role` (stats.rs:401),
`Registry` (stats.rs:539). #2427's "model layer" framing is that the counting now lives on the
model types (Registry/Traffic) rather than being scattered through the session code — a breaking
0.1.0 reshuffle.

### Producer/Consumer API — UPDATED: `Config` is now `ProducerConfig`

`rs/moq-stats/src/produce.rs:22-49`:

```rust
#[derive(Clone)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Origin the stats broadcasts are created on. When `None`, no task spawns.
	pub origin: Option<origin::Producer>,
	/// Top-level path stats are published under (default `.stats`). Also the
	/// registry's exclude prefix.
	pub prefix: PathOwned,
	/// Node suffix disambiguating relays sharing a cluster origin. May be
	/// multi-segment. Default none.
	pub node: Option<PathOwned>,
	/// Drain interval. Default 1s.
	pub interval: Duration,
	/// Grouping depth (leading broadcast-path segments used as a key). Default 0.
	pub depth: usize,
}
```

`ProducerConfig::new()` (produce.rs:51) gives no-origin defaults; `.with_origin(...)` enables
publishing. `Producer::new(config)` with no origin is a no-op with a disabled registry; otherwise
it spawns a drain task on `interval` that lives until the last clone drops (Weak keepalive).

Consumer side (`consume.rs`): `ConsumerConfig` (consume.rs:11-29, `compression: bool`, builder
`with_compression`); `Consumer::new(broadcast, ConsumerConfig)` (consume.rs:46) with
`.traffic(&tier, role).await` -> `TrafficConsumer` (consume.rs:52) and `.sessions(&tier).await` ->
`SessionsConsumer`, each `next() -> Result<Option<Frame>>`; a slow reader's missed intermediate
frames are collapsed, safe because counters are cumulative. lib.rs exports:
`Producer, ProducerConfig` (produce) and `Consumer, ConsumerConfig, SessionsConsumer,
TrafficConsumer` (consume).

### Target comparison with iroh-live's moq-media/src/stats.rs

Different layer entirely. iroh-live's `moq-media/src/stats.rs` is a *local, subscriber-side
transport probe*: it samples iroh `PathStats` (rtt, cwnd, lost_packets — see `rtt_ms` at
`/home/bit/Code/rust/iroh-live/moq-media/src/stats.rs:243`) and derives `available_bps` to feed
the adaptive rendition loop. moq-stats is a *server-side accounting exporter*: cumulative
per-broadcast delivery counters (bytes/frames/groups/subscriptions/fetches/datagrams/sessions),
published on the wire for dashboards and billing, with no rtt/cwnd/loss/bandwidth-estimate
anywhere. The closest moq analogue to iroh-live's stats.rs is the JS-side
`connection.recvBandwidth` signal used by `js/watch/src/video/source.ts` — which has no Rust
counterpart. Also note the publisher-side catalog `Metrics` (live jitter + bitrate measurement)
in moq-mux, covered below, which feeds *catalog* fields rather than a stats channel. Conclusion
for the refactor: moq-stats does not overlap with moq-media's stats.rs purpose; if anything it is
a model for the "publish stats as a track" idea (compressed JSON-merge-patch snapshot tracks)
rather than for congestion sampling.

---

## hang

Crate: `rs/hang`, version 0.19.5. Catalog + frame wire format only (`src/catalog/*`,
`src/container/frame.rs`, `src/timeline.rs`). **Only change since 261c2048**: the `displayRatio*`
serde aliases on `VideoConfig` (#2420), for decoding catalogs from publishers that predate the
`displayRatio*` -> `displayAspect*` rename:

```rust
// rs/hang/src/catalog/video/mod.rs
/// The `displayRatio*` aliases decode catalogs from publishers predating the
/// rename to `displayAspect*`; the current name is what we emit.
#[serde(alias = "displayRatioWidth")]
#[serde(alias = "displayRatioHeight")]
```

Round-trip tested by `decodes_legacy_display_ratio_keys` (video/mod.rs tests). The emitted key is
still `displayAspect*`; the alias only affects decode.

### Catalog model (current shape)

`rs/hang/src/catalog/root.rs:17-36`:

```rust
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Catalog {
	#[serde(default)]
	pub video: Video,
	#[serde(default)]
	pub audio: Audio,
}
```

`Catalog` is exactly `{ video: Video, audio: Audio }`; app extension via `#[serde(flatten)]` in an
app struct. No chat/user/preview/location sections. `Video`/`Audio` and
`VideoConfig`/`AudioConfig` are all `#[non_exhaustive]`.

`rs/hang/src/catalog/video/mod.rs` (fields only):

```rust
#[non_exhaustive]
pub struct VideoConfig {
	pub broadcast: Option<moq_net::PathRelativeOwned>,   // cross-broadcast passthrough ref
	pub codec: VideoCodec,
	pub description: Option<Bytes>,
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,
	pub display_aspect_width: Option<u32>,   // #[serde(alias = "displayRatioWidth")]
	pub display_aspect_height: Option<u32>,  // #[serde(alias = "displayRatioHeight")]
	pub bitrate: Option<u64>,
	pub framerate: Option<f64>,
	pub optimize_for_latency: Option<bool>,
	pub container: Container,
	pub jitter: Option<std::time::Duration>,   // ms on the wire; player's jitter buffer >= this
	pub timeline: Option<crate::catalog::Timeline>,
}
```

`rs/hang/src/catalog/audio/mod.rs` (fields only): same shape —
`broadcast, codec, sample_rate, channel_count (#[serde(rename="numberOfChannels")]), bitrate,
description, container, jitter, timeline`. Renditions live in
`Video.renditions: BTreeMap<String, VideoConfig>` / `Audio.renditions: BTreeMap<String,
AudioConfig>`, BTreeMap "so it will work with JSON Merge Patch". The `broadcast` field is what
moq-transcode's passthrough entries and moq-mux's `Source` resolution rely on; the `jitter` field
(milliseconds on the wire) is what iroh-live's phase-3b jitter/sync work would read to seed the
jitter buffer.

`catalog/priority.rs`: `pub const PRIORITY: Priorities = Priorities { catalog: 100, audio: 80,
video: 60 }`; `Catalog::default_subscription()` / `default_track_info()`. `to_json`/`to_json_pretty`
(the rename that bit iroh-live's Deref-to_string gotcha).

### container::Frame — NO keyframe field

`rs/hang/src/container/frame.rs`:

```rust
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame. ... This is NOT a wall clock time.
	pub timestamp: Timestamp,
	/// The encoded media data for this frame. ...
	#[debug("{} bytes", payload.len())]
	pub payload: Bytes,
}
```

The keyframe bit does NOT live on hang's wire `Frame` — the "first frame in a group is a keyframe"
convention carries it, and the richer decoded `Frame` with a `keyframe: bool` field is
`moq_mux::container::Frame` (below). `Timestamp` is the `moq_net::{Timescale, Timestamp}` pair with
`pub const TIMESCALE: Timescale = Timescale::MICRO` (frame.rs:13); `track_info()` (frame.rs:23-25)
is the one canonical `moq_net::track::Info` every hang media track creation uses, pinning the
net-level timescale to micros.

### Live Catalog Producer/Consumer

**Rust hang has no Catalog Producer/Consumer** — the live catalog producer/consumer lives in
**moq-mux**: `moq_mux::catalog::Producer` (both tracks + merge-patch snapshots via `moq_json`) and
`moq_mux::catalog::hang::Consumer` / `moq_mux::catalog::Consumer`. For the refactor comparison:
iroh-live's `moq-media/src/catalog.rs` (CatalogWrapper with sequence numbers) maps onto
`moq_mux::catalog::*`, not onto `rs/hang`.

---

## moq-mux (version 0.7.6)

The genuinely structural deltas since 261c2048 are the catalog rendition rework (#2420), the
shared video-import helper (#2425), the per-frame-fragment container contract (#2426), and CMAF
passthrough timelines (#2428). Everything is exact against 3a3e0ea8.

### catalog module exports (catalog/mod.rs)

```rust
pub use consumer::Consumer;
pub use format::*;
pub use producer::{Guard, Producer};
pub use select::Select;
pub use stream::Stream;
pub use tracks::{AudioTrack, Estimate, Rendition, RenditionConfig, Reserved, VideoHint, VideoTrack};
```

Note the shape change from the pre-merge analysis: the old sealed `Kind`/`Audio`/`Video` markers
are gone; renditions are now a **public generic trait** `RenditionConfig<E>` plus a new `Estimate`
type. `VideoTrack`/`AudioTrack` are type aliases over the generic `Rendition`.

### catalog::Producer (catalog/producer.rs:42-65)

```rust
pub struct Producer<E: CatalogExt = ()> {
	hang: moq_json::snapshot::Producer<Catalog<E>>,
	hangz: moq_json::snapshot::Producer<Catalog<E>>,
	msf_track: moq_net::track::Producer,
	current: Arc<Mutex<Catalog<E>>>,
	reservations: Arc<Mutex<Reservations>>,   // gates initial publish until all reservations resolve
	clock: crate::Clock,                        // shared wall clock; every importer gets a Copy
	broadcast: moq_net::broadcast::Producer,    // retained so timeline tracks can be created lazily
	timelines: Arc<Mutex<BTreeMap<String, crate::timeline::Producer>>>,  // memoized per media-track name
}
```

Publishes hang `catalog.json`, its `.z` sibling, and the MSF `catalog` track together; catalog
mutation is `producer.lock()` -> `Guard`, which publishes on drop (producer.rs:140). Key methods:
`reserve() -> Reserved<E>` (producer.rs:164), `media_producer<C>(track, container) ->
container::Producer<C>` (producer.rs:209-220, wires the 1:1-default timeline recorder), `timeline(name)
-> timeline::Producer` (producer.rs:225-235, memoized), `snapshot()` (producer.rs:152),
`timestamp(hint)` (producer.rs:132), `consume()` (producer.rs:237), `finish()` (producer.rs:242).

### #2420 — unsealed renditions: `RenditionConfig` trait + `Estimate` (catalog/tracks.rs)

The rendition layer is now generic over any config type, so an app extension can publish its own
tracks with the full lifecycle (reservation gating, removal on drop, jitter/bitrate detection).

`Estimate` — the auto-detectable catalog fields (tracks.rs:15-34):

```rust
#[derive(Clone, Default, Debug, PartialEq)]
#[non_exhaustive]
pub struct Estimate {
	/// The maximum jitter before the next frame is emitted.
	pub jitter: Option<Duration>,
	/// The maximum bitrate in bits per second.
	pub bitrate: Option<u64>,
}
// with_jitter / with_bitrate builders
```

`RenditionConfig<E>` — the public trait (tracks.rs:88-108):

```rust
pub trait RenditionConfig<E: CatalogExt>: Sized + 'static {
	fn insert(self, catalog: &mut Catalog<E>, name: &str);
	fn get_mut<'a>(catalog: &'a mut Catalog<E>, name: &str) -> Option<&'a mut Self>;
	fn remove(catalog: &mut Catalog<E>, name: &str);
	fn estimate(&self) -> Estimate { Estimate::default() }           // opt into detection
	fn set_estimate(&mut self, _estimate: Estimate) {}
}
```

`hang::catalog::VideoConfig` and `AudioConfig` implement it for every `E` (tracks.rs:181-222),
mapping `estimate`/`set_estimate` onto their own `jitter`/`bitrate` fields. `VideoHint`
(tracks.rs:119-140) is unchanged in spirit: caller-provided fields the importer fills only where
the stream leaves a gap (`fill` at tracks.rs:145; `apply` at tracks.rs:158; `to_config` at
tracks.rs:174).

`Reserved<E>` (tracks.rs:224-292): a clonable reservation context; while any clone is alive the
track set may grow so the catalog is withheld. `init::<C>(name) -> Rendition<E, C>` reserves a
rendition (tracks.rs:246); `video(name) -> VideoTrack<E>` / `audio(name) -> AudioTrack<E>` are
shorthands (tracks.rs:255-264); `producer() -> Producer<E>` hands back a non-gating catalog handle
(tracks.rs:283).

`Rendition<E, C: RenditionConfig<E>>` (tracks.rs:300-334): the reserved rendition guard, retired
from the catalog on drop. Type aliases `VideoTrack<E> = Rendition<E, VideoConfig>`,
`AudioTrack<E> = Rendition<E, AudioConfig>`. Methods:
- `set(config: C)` (tracks.rs:338-356): captures the config's supplied `Estimate` (authoritative,
  never overwritten by detection), fills the rest from the metrics detector, inserts the config,
  and releases the reservation (flushing a complete snapshot if it was the last).
- `update(f: impl FnOnce(&mut C))` (tracks.rs:384-393): refine in place (e.g. a synthesized
  description).
- `record_frame(ts, bytes)` / `record_reorder(reorder)` / `record_group_end(next)`
  (tracks.rs:395-410): feed the per-rendition `Metrics` (`container/jitter.rs`), auto-filling
  jitter (from frames/reorder) and bitrate (from group boundaries, 1s window) only for fields the
  config left absent. This is the publish-side measurement loop that keeps the catalog's
  `bitrate`/`jitter` honest — the values the (JS) subscriber-side ABR then selects on.
- `Drop` (tracks.rs:413-425): removes the config from the catalog if present.

### #2425 — shared video-import catalog helper (codec/video.rs, NEW)

`rs/moq-mux/src/codec/video.rs` (new file) factors out the catalog-publishing state every video
codec importer (h264/h265/av1/vp8/vp9) shares: overlay the caller's `VideoHint`, advertise the
rendition's timeline section, and dedupe a re-publish that matches the last one. `pub(crate) struct
Catalog { hint: VideoHint, timeline: hang::catalog::Timeline, last: Option<VideoConfig> }`:
- `new(reserved, name, hint)`: snapshots `reserved.producer().timeline(name).section()`.
- `initial_config()`: the hint-alone config (a hint carrying a codec) for publishing before the
  stream is parsed.
- `publish(rendition, config)`: applies the hint, sets `config.timeline`, dedupes against `last`,
  then `rendition.set(config)`. "A changed config just re-mirrors the rendition; there are no fixed
  tracks to reject a reconfiguration." (The generic `Rendition::set` no longer advertises the
  timeline itself — this helper does it for the video importers.)

This is invisible to callers of `moq_video::encode::Producer` (which drives `codec::h264::Import`
/ `codec::h265::Import`); only the importer internals moved.

### #2426 — per-frame fragments, empty-batch contract (container/mod.rs, consumer.rs)

The `Container::poll_read` / `read` trait contract was clarified (container/mod.rs:96-120): "Only
`Ok(None)` signals the end of the group. `Ok(Some(batch))` may carry an empty `batch`: a wire frame
was consumed but decoded to no media frames (e.g. a CMAF fragment with zero samples). That is not
end-of-group; poll again for the next batch. Callers accumulating frames must not treat an empty
batch as completion." The `GroupBuffer` loop in `container/consumer.rs:578-582` implements the
loop-on-empty. This enables emitting per-frame CMAF fragments without waiting for a successor
sample. **Anyone writing a new consumer against `moq_mux::container::Consumer` must key completion
off `None`, not an empty batch.** (moq-video/moq-audio `read` loops already do.)

### #2428 — emit timelines from CMAF passthrough (container/fmp4/import.rs)

The fMP4 passthrough importer now indexes each track's group opens into its `<name>.timeline.z`
timeline, so a playlist/seek/VOD reader can map time to group even for passthrough (non-transcoded)
CMAF. `Fmp4Track` gained `recorder: Option<crate::timeline::Recorder>` (fmp4/import.rs:75); at init
each track advertises `config.timeline = Some(self.catalog.timeline(track.name()).section())`
(fmp4/import.rs:239-256, per-track 1:1 since audio/video group boundaries differ) and holds
`timeline.recorder()`. On a keyframe fragment it calls `recorder.record(g.sequence, timestamp)`
(fmp4/import.rs:768-780), dropping the recorder on failure since "the timeline is an optional
sidecar ... a recording failure must NOT abort the passthrough." Passthrough writes groups by hand
(no `container::Producer`), so it feeds the recorder directly rather than through `with_recorder`.

### container::Frame / Producer / cut(end)

`rs/moq-mux/src/container/mod.rs` — the decoded frame WITH the keyframe bit:

```rust
#[derive(Clone, Debug)]
pub struct Frame {
	pub timestamp: moq_net::Timestamp,
	pub duration: Option<moq_net::Timestamp>,   // CMAF only; Legacy/LOC leave None
	pub payload: Bytes,
	pub keyframe: bool,   // CMAF reads it from trun sample-flags; Legacy/LOC leave false
}
```

`container::Producer<C: Container>` manages group boundaries — "Every group must start with a
keyframe. Writing a frame with `keyframe = true` closes the previous group ... and starts a new
one"; a keyframe-less write with no open group returns `MissingKeyframe`. `with_recorder(recorder)`
is **now public** (container/producer.rs, was `pub(crate)`): mint a recorder from a
`timeline::Producer` and wire it, one recorder per timeline; `media_producer` wires the 1:1 default
for you. `cut(&mut self, end: Option<Timestamp>)` is the group boundary (a keyframe cuts the
previous group using its timestamp as the boundary).

### timeline.rs — shareable timelines (#2420)

Timelines are now **shareable across aligned renditions** (a transcode ladder whose rungs mirror
the source's group boundaries can point at one timeline). `timeline::Producer<E>` is now `#[derive(Clone)]`
(was not `Clone`); every clone shares one track and a `wall: Arc<Mutex<Option<u64>>>` anchor, so N
renditions advertising the same timeline share it. Get one from `catalog::Producer::timeline(name)`.
`Recorder` is now **public** (was `pub(crate)`) and still move-only (owns its throttle cursor), so
wire exactly one recorder per timeline — a shared timeline is filled by its source alone, and the
other renditions only advertise `timeline.section()`. `Producer::recorder()` is now public too.

### Importer + source API

- `import/mod.rs`: importers split by multiplicity (`Track`/`TrackStream` = one codec onto one
  track; `Container`/`ContainerStream` = a container that may publish several tracks) and by
  framing. `unique_track()` mints a track with `hang::container::track_info()`.
- `import::Track<E>`: built from a `moq_net::track::Request` + `crate::catalog::Reserved<E>` +
  `Init`; `decode(frame, pts)`, `finish`, `abort`, `cut(end)`, `seek(sequence)`, `demand()`,
  `name()`. Importers serve on-demand track requests and register their own catalog rendition.
- `src/source.rs`: `pub struct Source { origin: moq_net::origin::Consumer, path: moq_net::PathOwned }`
  bundles an origin + catalog-broadcast path so exporters can resolve a rendition's cross-broadcast
  `broadcast` reference ("`../source`") through the same origin, deduplicating shared subscriptions.
  The consumer-side counterpart of the hang `broadcast` field and moq-transcode's passthrough
  catalog. (The 261c2048..main diff here is only the test-helper migration to the dynamic-broadcast
  origin API; the runtime API is unchanged.)
- `container/fmp4/muxer.rs`: one-shot CMAF muxing for individually fetched groups (the fetch-path
  complement used by HLS-style export).

### Refactor-relevant takeaways (moq-mux vs iroh-live moq-media)

- moq's Rust stack now has: publish-side ladder minting (moq-transcode), publish-side live
  bitrate/jitter measurement into the catalog (moq-mux `Rendition` metrics / `container/jitter.rs`),
  a latency-tunable ordering consumer with mid-stream `set_latency` and discontinuity signaling,
  per-rendition (and now shareable) timeline tracks for group indexing, and cross-broadcast
  rendition references — but still no Rust subscriber-side selection loop and no transport-level
  congestion sampling comparable to iroh-live's `stats.rs` (`PathStats` -> `available_bps`).
- iroh-live's subscriber pipeline (adaptive.rs + stats.rs + sync.rs + catalog.rs) would sit on top
  of exactly these primitives: catalog `jitter` seeds the jitter buffer, `timeline` + 1:1 rung group
  sequences enable seamless switch/backfill, `set_latency` enables PlayoutMode retuning,
  `discontinuity()` drives decoder flushes.
- The new `RenditionConfig<E>` trait means an iroh-live app extension could publish its own catalog
  tracks with the same reservation-gating + auto-detect lifecycle the media configs use, rather than
  hand-editing the catalog under the media pipeline's feet.
- The #2426 empty-batch contract is a correctness trap for any new `container::Consumer` reader:
  complete on `None`, never on an empty batch.
