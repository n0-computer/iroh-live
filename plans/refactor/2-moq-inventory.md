# moq Primitives Inventory: What the Upstream Stack Offers

This document catalogs the primitives the moq workspace (`/home/bit/Code/rust/moq`) exposes,
per crate, as the reference for the cut and upstream plans. It is built from the map files
under `plans/refactor/maps/` (moq-media.md, moq-video.md, moq-audio-nvenc.md,
moq-transcode-stats.md, and moq-net-origin.md), spot-verified against the repo. The
iroh-live side it is measured against is [1-code-map.md](1-code-map.md); category names
(codec-impl, capture-backend, pubsub-glue, adaptive, sync, and so on) refer to its section 2
tables.

## Upstream state

moq merged `dev` into `main` on 2026-07-21, so the previous main-versus-dev split is gone.
The whole native media stack, the rewritten net layer, moq-transcode, moq-stats, and moq-nvenc
now live on a single `main`. This inventory is verified against `main` at HEAD `3a3e0ea8`.
Every capability below is plain moq main; where a capability arrived after the old dev pin
`261c2048` the landing commit is cited.

Version numbers, verified against the tree at `3a3e0ea8`: moq-net 0.1.18, moq-native 0.18.3,
hang 0.19.5, moq-mux 0.7.6, moq-video 0.0.6, moq-audio 0.0.9, moq-transcode 0.0.1, moq-stats
0.1.0, and moq-nvenc 0.0.1 (release-plz owns them). iroh-live still pins the older released
crates.io line: moq-net 0.1.11 (aliased `moq-lite`), hang 0.19.1, moq-native 0.17.1, and
moq-relay 0.12.2 (`Cargo.toml:54-58`). The merged tree is visibly pre-bump (module-scoped
renames, `#[non_exhaustive]` everywhere, wire versions Lite05/Lite06Wip), so adopting any
capability catalogued here means driving or waiting for the next breaking release and then
bumping iroh-live's pins. See the "Capabilities on moq main, pending the next release" section
at the end for the consolidated dependency list.

Dependency layering: moq-net (transport) is at the bottom, hang (catalog and frame model) sits
on it, moq-mux (containers, importers, live catalog) on both, and moq-video / moq-audio (native
capture and codecs) on moq-mux plus moq-net.

---

## moq-net

Purpose: the transport layer. Broadcast, track, group, and frame primitives over any
`web_transport_trait::Session`, plus the origin registry and announce bus that relays and
meshes are built from. The merge brought a new subscription model, transparent migration,
caller-driven sessions, a real stats module, cumulative-cost multi-hop routing, and
path-scoped tokens. iroh-live still pins the released 0.1.11 line, so the whole surface below
arrives only with the next breaking release.

| Type / trait | Role | Map reference |
|---|---|---|
| `Origin`, `OriginList` | Relay identity (62-bit id) and hop chain for loop protection | moq-net-origin.md 1 |
| `origin::Producer` / `origin::Consumer` | In-process path tree of broadcasts plus coalesced announce stream; `scope`, `with_root`, `dynamic`, `create_broadcast` | moq-net-origin.md 1 |
| `origin::Producer::create_broadcast(path, Route)` | The sole content ingress (#2396); `publish_broadcast` is gone, attach plus announce gating fold into `Route` | moq-net-origin.md 1, 2 |
| `AnnounceProducer` / `AnnounceConsumer`, `OriginAnnounce` | Announce cursor split from the read handle; unannounce-then-announce for replacement | moq-net-origin.md 1 |
| `broadcast::Route { hops, cost, announce }`, `Route::announced()` | Route attachment: announced routes first, then lowest cumulative cost (#2424), best serves and others park as hot standbys | moq-net-origin.md 2 |
| `Path`, `PathOwned`, `PathPrefixes` | Zero-copy relative path type, boundary-safe prefix ops; empty PATH now legal (#2414) | moq-net-origin.md 1 |
| `PathRelative`, `Path::resolve` | `..`-aware sibling resolution for cross-broadcast references | moq-net-origin.md 1 |
| `Client` / `Server` builders, `(Session, Driver)` | Symmetric session setup over any WebTransport session; `with_origin`, `with_publisher`, `with_subscriber` wiring | moq-net-origin.md 3 |
| `Driver` (with `(Session, Driver)` connect) | Caller-driven protocol future, no internal tokio spawn, wasm-friendly (kio) | moq-net-origin.md 3 |
| `Role` in SETUP (`Publisher` / `Subscriber`, `Both` unrepresentable) | Publish-only or subscribe-only sessions derived from wired origins | moq-net-origin.md 3 |
| `Session::stats() -> ConnectionStats` | RTT, rates, bytes and packets sent / received / lost per session | moq-net-origin.md 3 |
| `Session::send_bandwidth()` / `recv_bandwidth()` (`BandwidthConsumer`) | Congestion-controller bandwidth estimate signal, both directions (also on pinned 0.1.11, verified `moq-net-v0.1.11:rs/moq-net/src/session.rs:59`) | moq-net-origin.md 3 |
| `Subscription { priority, ordered, latency_max, group_start, group_end }` | Per-subscriber wire preferences; the publisher observes the aggregate, clamped once (#2349) | moq-net-origin.md 4 |
| `resume::Producer` / `track::Subscriber` (`Plain` / `Spliced`), `update` | Transparent subscription migration across connections at group boundaries (#2241) | moq-net-origin.md 2 |
| `fetch_group`, coalescing `Requests<K, V>` | Past-group fetch with request dedup (N requesters, one upstream round trip, #2328); `get_group` is gone | moq-net-origin.md 4 |
| lite-05/06 announce (`AnnounceOk { origin, active }`, `RouteCost`, `EndedId`) | Initial-roster completion marker, cumulative route cost, retract-by-id | moq-net-origin.md 1, 2 |
| `stats::Registry` / `stats::Handle`, `Traffic`, `Presence`, `Tier` | Per-broadcast and per-subscription cumulative counters; `Tier` is now a bare `PathOwned` label (#2411) | moq-net-origin.md 3; moq-transcode-stats.md, moq-stats |
| moq-token `Claims`, `Scope`, `Key::with_scope` | Segment-aware path-scoped signing keys and tokens (#2416): a session's origin is constrained cryptographically to its granted prefixes | moq-net-origin.md 5 |

Capabilities relevant to iroh-live:

- The origin and announce bus can replace the announcement half of iroh-live's room layer
  (`iroh-live/src/rooms.rs`, 695 LOC, category room): scoped `announced()` over per-peer
  sessions instead of gossip plus smol-kv broadcast lists, with immediate unannounce on the
  last route detaching (#2419) and loop-protected, cost-aware transitive forwarding (#2424,
  moq-net-origin.md 7). Membership bootstrap (whom to dial first) still needs the ticket or
  gossip.
- moq-token path-scoping (#2416) closes the announce-spoofing gap that made gossip plus KV's
  signed authorship a hard advantage: each accepted session's origin is scoped to
  `<room>/<peer-id>/`, so a peer can only announce its own broadcasts (moq-net-origin.md 5, 7).
- `Subscription.latency_max` and the aggregate clamp are the transport-level home for
  iroh-live's `PlaybackPolicy.max_latency` (moq-media playout.rs, category sync).
- `Session::stats()` and `send_bandwidth()` cover most of what iroh-live's
  `spawn_signal_producer` derives from iroh `PathStats` (`iroh-live/src/util.rs`, category
  stats, feeding `NetworkSignals` in moq-media net.rs, category adaptive).
- #2241 migration gives calls seamless roaming and reconnect resume with no iroh-live
  equivalent today, though #2419 removed the old `ROUTE_LINGER` grace, so a reconnecting peer
  re-announces rather than splicing into a lingering front unless the replacement route
  attaches before the old one detaches.

Gaps: no subscriber-side ABR or rendition selection anywhere in Rust moq-net or above (the
logic lives in JS `js/watch`, moq-transcode-stats.md, "CRITICAL" section); no membership or
peer-sampling layer (announce presupposes sessions exist); transitive re-announce conflicts
with per-peer token scoping unless relay nodes carry a broader scope (moq-net-origin.md 7.2).

---

## moq-native

Purpose: batteries-included client and server on top of moq-net: QUIC (quinn, quiche),
WebSocket, and iroh transports behind one `connect(url)` / `accept()` surface, plus TLS,
config, and a reconnecting client. Uses `web-transport-iroh` over `iroh` with the
caller-supplied `iroh::Endpoint` model. iroh-live pins the released 0.17.1; main is 0.18.3.

| Type / trait | Role | Map reference |
|---|---|---|
| `EndpointConfig::bind` (iroh module) | One shared P2P endpoint for both roles, all moq ALPNs plus h3 | moq-net-origin.md 6 |
| `iroh::connect(endpoint, url, addrs)` | Dial an `iroh://` URL (host is an `EndpointId`), returns a `web_transport_iroh::Session` | moq-net-origin.md 6 |
| Single-phase iroh accept (`accept -> (session, url, None)`) | ALPN plus WebTransport handshake completed in one step; the old two-phase deferred-accept authorization window is gone, authorization moves to SETUP token plus path | moq-net-origin.md 6 |
| `Reconnect` + `Backoff` | Transport-agnostic reconnecting client; each retry is a fresh connect | moq-net-origin.md 6 |
| `Reconnect::send_bandwidth()` / `recv_bandwidth()` / `stats()` | Bandwidth and connection stats surviving reconnects | moq-net-origin.md 6 |

Capabilities relevant to iroh-live:

- The iroh module makes about 120 lines of `iroh-moq`'s `MoqSession` handshake a duplicate,
  including the hardcoded `b"moq-lite-04"` ALPN (1-code-map.md, iroh-moq section; category
  transport). The remaining `Actor` (dedup, node-wide fan-out, `ProtocolHandler`) has no
  moq-native equivalent and stays.
- `iroh-live-relay` already consumes moq-native and moq-relay directly (1-code-map.md
  section 1), so alignment here is consolidation, not adoption.

Gaps: `Reconnect` does not interact with #2241 subscription resume (migration engages at the
relay and origin layer); no iroh `Router` `ProtocolHandler` integration or per-node session
manager; the single-phase iroh accept drops the deferred-accept authorization window the older
pinned line had.

---

## hang

Purpose: the shared media-metadata model: the JSON catalog (renditions of `VideoConfig` and
`AudioConfig`, codec descriptors, container selection) and the Legacy container wire `Frame`.
It is deliberately model-only; the live publish and subscribe machinery lives in moq-mux.
iroh-live pins the released 0.19.1; main is 0.19.5, essentially unchanged since 261c2048.

| Type / trait | Role | Map reference |
|---|---|---|
| `Catalog { video, audio }` | Rendition maps plus display metadata; JSON, camelCase, `#[non_exhaustive]`, unknown fields tolerated | moq-transcode-stats.md, hang |
| `VideoConfig` / `AudioConfig` | WebCodecs-shaped decoder configs, `#[non_exhaustive]` | moq-transcode-stats.md, hang |
| `VideoCodec` (H264, H265, VP9, AV1, VP8, Unknown), `AudioCodec` (AAC, Opus, Mp2, Ac3, Ec3, Unknown) | Codec descriptor parse and display (`avc1.PPCCLL` and friends) | moq-transcode-stats.md, hang |
| `Container` enum (Legacy, Cmaf, Loc) | Per-track wire framing selector | moq-transcode-stats.md, hang |
| `container::Frame { timestamp, payload }` | Legacy wire frame, micro timescale, no keyframe field (the group convention carries it) | moq-transcode-stats.md, hang |
| `jitter: Option<Duration>` on both configs | Publisher-advertised jitter bound (milliseconds on the wire) the player's buffer should exceed | moq-transcode-stats.md, hang |
| `timeline: Option<Timeline>` | Companion track indexing a rendition's groups | moq-transcode-stats.md, hang |
| `broadcast: Option<PathRelativeOwned>` on both configs | Cross-broadcast rendition reference (transcoder passthrough) | moq-transcode-stats.md, hang |
| `displayRatio*` serde aliases on `VideoConfig` | Decode catalogs predating the `displayAspect*` rename; emitted key stays `displayAspect*` (#2420) | moq-transcode-stats.md, hang |
| `PRIORITY` constants, `default_subscription()`, `track_info()` | Canonical catalog / audio / video priorities and the net timescale pinned to micros | moq-transcode-stats.md, hang |

Capabilities relevant to iroh-live:

- The catalog model already backs iroh-live's config types; rusty-codecs `config.rs`
  (318 LOC, category catalog) mirrors it with `From` impls and could shrink to direct use.
- The `jitter` field is the publisher-side input iroh-live's phase-3b jitter and sync work
  needs; moq-media sync.rs's dead `audio_ms` / `video_ms` fields flagged exactly this field as
  missing, but it exists and is now populated by moq-mux's per-rendition metrics (1-code-map.md
  section 3, moq-mux entry below).
- `broadcast` cross-references plus `timeline` enable the seamless rendition switching contract
  iroh-live's adaptive layer wants (moq-transcode-stats.md, moq-transcode comparison).

Gaps: no chat or user sections (apps flatten their own extensions; iroh-live's `IrohLiveExt`
in moq-media catalog.rs, category catalog and chat, remains necessary); no live Rust catalog
producer or consumer in hang itself (that is `moq_mux::catalog`, moq-transcode-stats.md, hang
section); no VP8 descriptor struct; the `jitter` retype to `Duration` and the `to_json` rename
are migration chores for iroh-live, the second having bitten it once (project memory, migration
gotchas).

---

## moq-mux

Purpose: everything between transport and model: container muxers and demuxers (legacy, LOC,
CMAF/fMP4, MPEG-TS, MKV, FLV, HLS), per-codec importers that parse raw bitstreams into tracks
plus catalog entries, the generic latency-bounded `container::Producer` / `Consumer`, and the
live catalog publish and subscribe machinery (hang JSON plus MSF encodings). Version 0.7.6; the
structural deltas since 261c2048 are the catalog rendition rework (#2420), the shared
video-import helper (#2425), the per-frame-fragment container contract (#2426), and CMAF
passthrough timelines (#2428).

| Type / trait | Role | Map reference |
|---|---|---|
| `container::Frame { timestamp, duration, payload, keyframe }` | The rich decoded-side frame (hang's wire frame lacks keyframe and duration) | moq-transcode-stats.md, moq-mux |
| `container::Producer<C: Container>` | Group management: keyframe starts group, `cut(end)` boundary, `MissingKeyframe` guard; `with_recorder` now public | moq-transcode-stats.md, moq-mux |
| `container::Consumer<C>` with `with_latency` / `set_latency` | Ordered cross-group reading with latency-bounded group skipping, mid-stream latency retune (consumer.rs:479), and `discontinuity()` (consumer.rs:161) | moq-transcode-stats.md, moq-mux |
| `Container::poll_read` / `read` contract | Only `Ok(None)` ends a group; `Ok(Some(empty))` is a consumed wire frame that decoded to no media frames, poll again (#2426) | moq-transcode-stats.md, moq-mux |
| `codec::{h264, h265, av1, vp8, vp9, aac, opus, annexb}` importers and config parsers | avcC / hvcC / av1C / OpusHead parsing, `Import` publishing raw bitstream to a broadcast | moq-transcode-stats.md, moq-mux |
| `codec::video::Catalog` (shared video-import helper) | Factored-out per-importer catalog state: overlay `VideoHint`, advertise the timeline section, dedupe re-publish (#2425); invisible to `encode::Producer` callers | moq-transcode-stats.md, moq-mux |
| `catalog::Producer<E: CatalogExt>` / `Guard` | Live catalog publishing in hang JSON, compressed `.z`, and MSF encodings; republish on guard drop; `reserve`, `media_producer`, `timeline` | moq-transcode-stats.md, moq-mux |
| `catalog::Consumer<E>` / `catalog::hang::Consumer<E>` | Live catalog snapshots off a subscription (the Rust live catalog consumer) | moq-transcode-stats.md, moq-mux |
| `RenditionConfig<E>` trait, `Estimate { jitter, bitrate }` | Unsealed public generic rendition trait (#2420); the old sealed `Kind`/`Audio`/`Video` markers are gone, `VideoTrack`/`AudioTrack` are aliases over `Rendition<E, C>` | moq-transcode-stats.md, moq-mux |
| `Reserved<E>` / `Rendition<E, C>` / `VideoHint` | Initial-publish gating (first snapshot withheld until reservations resolve), rendition retired on drop, caller hints filled only where the stream leaves a gap | moq-transcode-stats.md, moq-mux |
| `Rendition::record_frame` / `record_reorder` / `record_group_end`, `Metrics` (container/jitter.rs) | Publish-side per-rendition live jitter and bitrate measurement auto-filling the catalog fields the config left absent | moq-transcode-stats.md, moq-mux |
| `timeline::Producer` (`Clone`), `Recorder`, `Producer::recorder()` | Shareable timelines across aligned renditions (`wall: Arc<Mutex<..>>` anchor); `Recorder` and `recorder()` now public, one recorder per timeline (#2420) | moq-transcode-stats.md, moq-mux |
| `import::{Track, TrackStream, Container, ContainerStream}`, `unique_track()` | Format-string front door split by multiplicity and framing; importers serve on-demand `track::Request`s | moq-transcode-stats.md, moq-mux |
| `Source { origin, path }` | Consumer-side resolution of cross-broadcast `broadcast` references with subscription dedup | moq-transcode-stats.md, moq-mux |
| `fmp4::Muxer` (one-shot), fMP4 passthrough timelines | Standalone CMAF muxing of individually fetched groups; passthrough importer now indexes group opens into `<name>.timeline.z` (#2428) | moq-transcode-stats.md, moq-mux |
| `Clock` | Shared wall clock stamping capture PTS so audio and video align | moq-transcode-stats.md, moq-mux |
| `Select` (catalog) | Static rendition narrowing for exporters (not adaptive) | moq-transcode-stats.md, moq-transcode section |

Capabilities relevant to iroh-live:

- `container::Producer` / `Consumer` are what moq-media's `MoqPacketSink` / `MoqPacketSource`
  already wrap (`moq-media/src/transport.rs`, 204 LOC, the declared refactor seam, category
  transport); `PlaybackPolicy.max_latency` maps directly onto `with_latency`, and `set_latency`
  is the hook for runtime `PlayoutMode` changes (category sync).
- The #2426 empty-batch contract is a correctness trap for any new consumer reader: complete on
  `None`, never on an empty batch. iroh-live's `MoqPacketSource` must respect it.
- `catalog::Producer` replaces the purpose of moq-media's `CatalogProducer` wrapper
  (catalog.rs, category catalog); iroh-live already delegates container framing and catalog
  production here with no duplication found (1-code-map.md, moq-media section).
- The `RenditionConfig<E>` trait means an iroh-live app extension could publish its own catalog
  tracks with the same reservation-gating and auto-detect lifecycle the media configs use,
  rather than hand-editing the catalog under the media pipeline's feet.
- Per-rendition `record_frame` / `record_group_end` metrics cover the publish-side half of what
  iroh-live's stats overlay estimates locally, and the `Estimate { jitter, bitrate }` they
  auto-detect keeps the catalog `bitrate` and `jitter` honest, which is the input a
  subscriber-side ABR selects on (categories stats and adaptive).
- `discontinuity()` gives the decoder-flush signal iroh-live's subscribe pipeline currently
  infers itself (category pubsub-glue).

Gaps: no subscriber-side rendition selection (`Select` is static); no playout clock or jitter
buffer on top of the consumer (skipping only, never adding latency).

---

## moq-video

Purpose: native video capture, encode, and decode publishing straight into moq tracks and the
hang catalog, with a deliberately backend-free public API. A 41-file native stack
(VideoToolbox, Media Foundation, NVENC/NVDEC, VAAPI, openh264) with zero-copy GPU paths and no
ffmpeg dependency. Version 0.0.6; the core codec files are byte-identical to the pre-merge
analysis.

| Type / trait | Role | Map reference |
|---|---|---|
| `encode::Encoder`, `encode::Config`, `Kind` (Auto, Hardware, Software, Named) | Raw RGBA, I420, or decoded-frame in, Annex-B `Vec<Bytes>` out | moq-video.md 1 |
| `encode::Codec` (H264, H265) | Output codec selection | moq-video.md 1 |
| `Encoder::set_bitrate` | Live retune, no IDR, no session rebuild | moq-video.md 1 |
| `rate::Policy` / `rate::Control` | Pure rate-control policy fed by `moq_net::bandwidth::Consumer`: headroom, hysteresis, asymmetric attack and decay (#2303) | moq-video.md 1, rate control |
| `encode::Producer` | Splitter plus moq-mux importer pair onto a `*.avc3` / `*.hev1` track with catalog registration | moq-video.md 1 |
| `encode::publish_capture`, `demand()` | Turnkey demand-driven capture: catalog advertised up front, camera open only while watched | moq-video.md 1 |
| `decode::Decoder`, `decode::Config { kind, latency_max, resize }` | Catalog-driven decode with avc1 / hvc1 to Annex-B conversion, keyframe gating | moq-video.md 2 |
| `decode::Consumer` | Track subscription plus decode loop, `latency_max` forwarded to `with_latency` | moq-video.md 2 |
| `decode::Frame` (public: timestamp, size; private I420 or GPU), `resize`, `into_i420` | Decoded picture, `Send + Sync`, GPU resize on CUDA | moq-video.md 2, 4 |
| `capture::Source` (Camera, Display, Window, App), enumeration (`cameras()`, `displays()`, `windows()`, `apps()`) | Source selection and enumeration (#2293); enumeration is macOS-only today | moq-video.md 3 |
| Encode backends: videotoolbox, mediafoundation, nvenc, vaapi, openh264 | Candidate-table dispatch, try-in-order fallback; `Backend` trait is `pub(crate)` | moq-video.md 1 |
| Decode backends: videotoolbox, mediafoundation (DXVA), nvdec, openh264 | Same dispatch; NVDEC does H.264, H.265, and AV1 with free hardware scaling | moq-video.md 2 |
| `frame::Frame` (Surface, Texture, Cuda, I420) and converters | Private zero-copy frame union; yuv-crate BT.601 converters, SIMD resize | moq-video.md 4 |
| Zero-copy paths: capture to VT encode (macOS), capture to MF encode (Windows), NVDEC to NVENC (Linux) | End-to-end GPU frames with no host copy | moq-video.md 4 |

Capabilities relevant to iroh-live:

- moq-video is the single largest overlap with iroh-live: it targets the same territory as
  rusty-codecs codec-impl (12,331 LOC), rusty-capture capture-backend (5,507 LOC), and the
  video half of moq-media pubsub-glue (1-code-map.md section 3). Concretely it covers
  iroh-live's h264 (openh264), vtb (VideoToolbox), and NVIDIA-shaped needs, plus Windows
  backends iroh-live only has documentation stubs for.
- `publish_capture` plus `demand()` replaces the lazy on-demand track start and camera parking
  in moq-media publish.rs (category pubsub-glue).
- `rate::Control` (#2303) is a ready implementation of iroh-live's planned phase-3d adaptive
  encoding (category adaptive, encoder side).
- `decode::Consumer` with `latency_max` replaces the decoder-thread half of moq-media
  subscribe.rs `VideoTrack` (category pubsub-glue).

Gaps: no VP9 anywhere; no AV1 encode (AV1 is NVDEC-decode-only, 8-bit 4:2:0); no VAAPI decode
(iroh-live's vaapi decoder, 3,257 LOC, has no counterpart); no V4L2 codec backend (v4l is
capture-only; iroh-live's v4l2 encoder and decoder for ARM SoCs have no counterpart); no
Android backends at all (iroh-live's MediaCodec stack has no counterpart); no decode-to-render
GPU handoff except the CUDA encode path, and no GPU renderer of any kind (iroh-live's 3,463 LOC
gpu-zerocopy render stack is unmatched); `Backend` traits are `pub(crate)`, so external
backends cannot be plugged in; VAAPI encode is CPU-input only, unvalidated on hardware, and
hard-links libva (#1837); capture enumeration is macOS-only; Linux screen capture (PipeWire) is
CPU-only with no dmabuf import.

---

## moq-audio

Purpose: native audio counterpart, shaped like moq-video: Opus encode and decode via
unsafe-libopus (pure Rust), cpal microphone capture, rubato resampling, publishing one Opus
packet per moq-lite group over the Legacy container with hang catalog registration. Version
0.0.9; #2350 recut the engine into `encode` / `decode` / `capture` modules whose type names
mirror moq-video one-for-one, and #2293 added system-audio capture and device enumeration.

| Type / trait | Role | Map reference |
|---|---|---|
| `Codec` (Opus only, `#[non_exhaustive]`) | Codec selection with explicit growth path, no trait yet | moq-audio-nvenc.md, codec coverage |
| `Format` | WebCodecs `AudioData.format` mirror with zero-copy `as_interleaved_f32` | moq-audio-nvenc.md, frame and format |
| `Frame { timestamp, data }` | PCM unit crossing the codec boundary; layout fixed at construction | moq-audio-nvenc.md, frame and format |
| `encode::Encoder` | Sans-I/O Opus encode, `catalog()` builds the hang `AudioConfig` with OpusHead | moq-audio-nvenc.md, encoder |
| `decode::Decoder` | Opus decode from a catalog `AudioConfig` | moq-audio-nvenc.md, decoder |
| `encode::Producer` | PCM to Opus to container track; rendition registered at construction, removed on drop; `reset_epoch` gap semantics | moq-audio-nvenc.md, producer |
| `decode::Consumer` | Track subscription to PCM frames; `latency_max` forwarded to `with_latency` | moq-audio-nvenc.md, consumer |
| `capture::Source` (Microphone, System), `devices()` enumeration | Mic capture; macOS system audio via ScreenCaptureKit and device listing (#2293) | moq-audio-nvenc.md, capture |
| `encode::publish_capture` | Turnkey demand-driven audio publish on the shared `Clock` | moq-audio-nvenc.md, turnkey publish |
| `Resampler` | rubato sinc resample, sample-rate only | moq-audio-nvenc.md, error and resampler |

Capabilities relevant to iroh-live:

- Replaces iroh-live's opus codec-impl (rusty-codecs codec/opus/, 804 LOC): same underlying
  unsafe-libopus crate, plus catalog interop iroh-live builds by hand.
- `encode::Producer` / `decode::Consumer` replace the audio pipeline half of moq-media
  pubsub-glue (pipeline audio loops, silence insertion aside).
- Capture overlaps the input half of moq-media's `AudioDriver` (audio_backend.rs, 2,445 LOC,
  category audio-device): both sit on cpal, but moq-audio only captures.

Gaps: no FEC or PLC entry point (`opus_decode_float` is only ever called with real packet data;
no `decode_lost()`; iroh-live's phase-3c has no upstream base); no playback sink at all (speaker
output, mixing, fades, device switching, and recovery in iroh-live's audio_backend.rs have no
counterpart); no AEC (iroh-live's sonora-based audio_backend/aec.rs has no counterpart); no AAC
despite the catalog supporting it; no runtime bitrate change (unlike video's `set_bitrate`);
channel remapping rejected; `latency_min` jitter padding explicitly deferred (decode/decoder.rs
doc); capture forwards over an unbounded channel (conflicts with iroh-live's bounded-channel
convention).

---

## moq-nvenc

Purpose: vendored, trimmed fork of nvidia-video-codec-sdk providing NVENC encode bindings and
an NVDEC (cuvid) function table, always dlopen-based: it links on GPU-less builders, starts on
driverless hosts, needs no CUDA toolkit at build time, and compiles everywhere (compile-only
stub off Linux and Windows). Version 0.0.1, vendored by #2042 `dbc589f4`; publishable by design
(crates.io metadata, three external deps, release-flow wired). Source is byte-identical since
261c2048.

| Type / trait | Role | Map reference |
|---|---|---|
| `ENCODE_API: EncodeAPI` | dlopen'd NVENC function table (panics if driver absent; probe first) | moq-audio-nvenc.md, moq-nvenc safe API |
| `Encoder`, `EncoderInitParams`, `Session` | Session lifecycle over a `CudaContext` | moq-audio-nvenc.md, moq-nvenc |
| `Session::reconfigure(bitrate)` | The fork's addition: live bitrate change, no IDR, no reset; what `set_bitrate` maps to | moq-audio-nvenc.md, session.rs |
| `EncodePictureParams::force_idr` | Out-of-cadence keyframe via `FORCEIDR` flag (hardware-verified) | moq-audio-nvenc.md, session.rs |
| `Buffer` / `Bitstream` locks, `EncoderInput` / `EncoderOutput` traits | RAII I/O buffers with pitched writes | moq-audio-nvenc.md, buffer.rs |
| `RegisteredResource<'a, T>` | Register an external (CUDA) buffer as encoder input: the zero-copy hook | moq-audio-nvenc.md, buffer.rs |
| `cuvid::Api` | Runtime-resolved NVDEC entry points (returns `Result`, allowing fallback); no safe wrapper yet | moq-audio-nvenc.md, cuvid.rs |

Capabilities relevant to iroh-live: purely additive; iroh-live has no NVIDIA backend, so this
extends codec-impl coverage rather than replacing anything. The dlopen pattern is also the model
#1837 wants for VAAPI.

Gaps: safe layer self-described as largely unfinished; no safe NVDEC wrapper; encode-side lazy
static panics without the driver (callers must run `driver_libs_present()` first); NVIDIA-only.

---

## moq-transcode

Purpose: just-in-time live transcoding for hang broadcasts (#2140, #2158): consume a source
broadcast, publish a derivative catalog advertising a ladder of lower rungs plus passthrough
references to the source renditions, and encode a rung only while somebody subscribes to it.
One shared subscription and decoder per source regardless of active rungs; output group N of
every rung is the same content as source group N, so a switching player lands on identical
content. Version 0.0.1; source logic is unchanged since 261c2048 (only the moq-net broadcast
API migration touched it). Runs as a library or the `moq transcode` CLI verb (sidecar process,
not relay-integrated).

| Type / trait | Role | Map reference |
|---|---|---|
| `run(source, output, config)` | The whole crate's entry point | moq-transcode-stats.md, moq-transcode |
| `Rung { height, bitrate }`, `Config { rungs, source, encoder, decoder }` | Ladder definition (default 1080p/5M down to 240p/350k), never upscaling | moq-transcode-stats.md, moq-transcode |
| `feed::Feed` / `Listener` | Decode-once fan-out over a broadcast channel; `Arc<decode::Frame>` keeps GPU frames on the GPU; lagging rung gets `Lagged`, not a stall | moq-transcode-stats.md, feed.rs |
| `rung::serve` (live plus fetch paths) | Demand-driven encode (`used` / `unused`), bounded one-shot group fetch transcode (semaphore of 4) | moq-transcode-stats.md, rung.rs |
| Deterministic catalog advertising | Codec strings computed from the ladder, published before any encoder exists | moq-transcode-stats.md, catalog advertising |

Capabilities relevant to iroh-live: the supply side of ABR. It can replace or complement the
simulcast rendition registries in moq-media publish.rs (category pubsub-glue): instead of the
publisher encoding N renditions, a GPU-equipped node mints the ladder on demand. The 1:1
group-sequence contract plus the fetch path are exactly what iroh-live's planned seamless
rendition switching (phase-3a) can target for backfill.

Gaps: strictly publish-side; contains zero selection logic. Rust moq has no subscriber-side ABR
at all (selection exists only in JS `js/watch`, with a crude 0.8-bandwidth-margin heuristic);
iroh-live's adaptive.rs fills exactly that gap and is complementary, not redundant
(moq-transcode-stats.md, "CRITICAL" section).

---

## moq-stats

Purpose: publish and consume MoQ traffic accounting as MoQ broadcasts (#2380, #2348): a
`Producer` drains the moq-net `stats::Registry` on an interval and publishes cumulative
per-broadcast counters as JSON tracks (plain plus DEFLATE merge-patch compressed `.z` siblings)
at `<prefix>/node/<node>`; a `Consumer` yields typed frames for aggregators, dashboards, and
billing. Version 0.1.0.

| Type / trait | Role | Map reference |
|---|---|---|
| `Producer`, `ProducerConfig { origin, prefix, node, interval, depth }` | Interval-drained registry publisher; no-op without an origin. `Config` was renamed `ProducerConfig` | moq-transcode-stats.md, moq-stats |
| `Consumer`, `ConsumerConfig`, `TrafficConsumer`, `SessionsConsumer` | Typed frame readers; missed frames collapse safely (counters are cumulative) | moq-transcode-stats.md, moq-stats |
| `Traffic`, `Presence`, `Tier` (re-exported from moq-net) | Cumulative bytes, frames, groups, subscriptions, `fetches` (#2427), `datagrams` (#2430), sessions, bucketed by `Tier` (now a bare `PathOwned` label after #2411 removed the internal defaults) | moq-transcode-stats.md, metric set |

Capabilities relevant to iroh-live: a model (and possible host) for iroh-live's
`spawn_stats_recorder` (`iroh-live/src/util.rs`, category stats), and the "publish stats as a
track" pattern for the debug overlay. It does not overlap with moq-media stats.rs.

Gaps: server-side delivery accounting only; no RTT, cwnd, loss, or bandwidth estimate anywhere,
so it cannot feed the adaptive loop (that remains `Session::stats()` plus `send_bandwidth()` on
moq-net, or iroh `PathStats` as today).

---

## Peripheral crates (one-line notes)

Versions from the main working tree at `3a3e0ea8`.

- **moq-hls** (0.3.0): HLS / LL-HLS gateway for MoQ broadcasts; irrelevant to iroh-live except
  as a moq-mux fmp4 consumer.
- **moq-rtc** (0.1.5): WebRTC (WHIP/WHEP) gateway; potential future browser-interop path, not a
  refactor target.
- **moq-rtmp** (0.1.3): RTMP contribution ingest gateway; not relevant.
- **moq-srt** (0.1.2): bidirectional SRT gateway; not relevant.
- **moq-cli** (0.8.7): the `moq` command (publish, subscribe, relay, transcode); reference
  consumer code, rendition selection is static flags.
- **moq-relay** (0.13.7): the relay server; iroh-live-relay already builds directly on it and
  moq-native (1-code-map.md section 1).
- **moq-token** (0.6.3): token generation and validation for relay auth, now with segment-aware
  path `Scope` on the signing key (#2416); relevant to per-peer announce authorization if rooms
  adopt relay-backed auth (moq-net-origin.md 5).
- **libmoq** (0.3.14): C bindings; subscription is by catalog index with a TODO for real
  rendition selection, confirming the ABR gap.
- **moq-ffi** (0.2.33): UniFFI bindings (mobile); the reason moq-audio's capture feature is off
  by default.
- **moq-gst** (0.2.12): GStreamer plugin; alternative capture and codec integration path,
  consumes both bandwidth directions, not inventoried further.
- **moq-vaapi** (0.0.2): external crates.io crate (repo moq-dev/vaapi, vendored from cros-libva
  plus cros-codecs); the VAAPI backend moq-video's encode path links against.

Also present upstream but out of scope here: kio (waker plumbing under moq-net), moq-json
(snapshot merge-patch encoding used by catalogs and stats), moq-msf, moq-flate, moq-loc,
moq-wasm, moq-bench, and moq-boy.

---

## Summary table: moq capabilities on main

| # | Capability | Present on moq main | Where |
|---|---|---|---|
| 1 | Origin registry, announce bus, hop-chain loop protection | yes | moq-net-origin.md 1 |
| 2 | `create_broadcast` plus `Route.announce` gating, `AnnounceOk` initial-roster count | yes (#2396) | moq-net-origin.md 1, 2 |
| 3 | `Subscription` (priority, ordered, latency_max, group ranges) on the API | yes | moq-net-origin.md 4 |
| 4 | Aggregate subscription combine plus single clamp (#2349) | yes | moq-net-origin.md 4 |
| 5 | Transparent subscription migration (#2241); `ROUTE_LINGER` removed (#2419) | yes, linger gone | moq-net-origin.md 2 |
| 6 | Cumulative-cost multi-hop route selection | yes (#2424) | moq-net-origin.md 2 |
| 7 | Coalesced `fetch_group` past-group fetch | yes (#2328) | moq-net-origin.md 4 |
| 8 | Caller-driven `(Session, Driver)`, tokio-free moq-net | yes | moq-net-origin.md 3 |
| 9 | `Role` in SETUP (publish-only / subscribe-only) | yes | moq-net-origin.md 3 |
| 10 | Bandwidth estimate signal (`send_bandwidth` / `recv_bandwidth`) | yes (also on pinned 0.1.11) | moq-net-origin.md 3 |
| 11 | `Session::stats()` connection snapshots | yes | moq-net-origin.md 3 |
| 12 | Path-scoped signing keys and tokens (`Scope`) | yes (#2416) | moq-net-origin.md 5 |
| 13 | iroh transport in moq-native (`iroh://` dial and single-phase accept) | yes | moq-net-origin.md 6 |
| 14 | hang catalog model (renditions, codecs, container, jitter, timeline, `broadcast`) | yes | moq-transcode-stats.md, hang |
| 15 | Live catalog producer and consumer (moq-mux, hang JSON plus MSF) | yes | moq-transcode-stats.md, moq-mux |
| 16 | Unsealed `RenditionConfig<E>` trait plus `Estimate { jitter, bitrate }` auto-detect | yes (#2420) | moq-transcode-stats.md, moq-mux |
| 17 | Catalog `Reserved` / `Rendition` gating plus live per-rendition `Metrics` | yes | moq-transcode-stats.md, moq-mux |
| 18 | Container consumer latency skip (`with_latency`), mid-stream `set_latency`, `discontinuity()` | yes | moq-transcode-stats.md, moq-mux |
| 19 | Per-frame-fragment empty-batch container contract | yes (#2426) | moq-transcode-stats.md, moq-mux |
| 20 | Shareable timelines, public `Recorder` / `recorder()` / `with_recorder` | yes (#2420) | moq-transcode-stats.md, moq-mux |
| 21 | Native H.264 and H.265 encode (hardware backends plus openh264) | yes | moq-video.md 1 |
| 22 | Encoder rate control (#2303), live `set_bitrate` | yes | moq-video.md 1 |
| 23 | Native video decode (VT, MF/DXVA, NVDEC, openh264; AV1 via NVDEC) | yes | moq-video.md 2 |
| 24 | Screen, window, and app capture; device enumeration (macOS-only enumeration) | yes | moq-video.md 3 |
| 25 | Zero-copy capture-to-encode and NVDEC-to-NVENC GPU paths | yes | moq-video.md 4 |
| 26 | Opus encode, decode, mic capture, demand-driven publish | yes | moq-audio-nvenc.md |
| 27 | System-audio capture, audio device enumeration (#2293) | yes | moq-audio-nvenc.md, capture |
| 28 | JIT transcode ladder (moq-transcode) | yes | moq-transcode-stats.md |
| 29 | Wire-published traffic stats (moq-stats, with `fetches` / `datagrams`) | yes | moq-transcode-stats.md |
| 30 | Subscriber-side ABR / rendition selection in Rust | no (JS only) | moq-transcode-stats.md, CRITICAL |
| 31 | Playout clock, jitter buffer, A/V sync in Rust | no | moq-transcode-stats.md, takeaways |

## Capabilities on moq main, pending the next release iroh-live bumps to

The capabilities the iroh-live refactor would depend on are all merged on moq main now. They
are no longer a dev-branch gamble; they are simply waiting on the next breaking release of the
affected crates and a version bump in iroh-live, which still pins the older crates.io line
(moq-net 0.1.11, moq-native 0.17.1, hang 0.19.1). Adopting any row means driving or waiting for
that release and then updating the pins.

| Capability | Crate(s) | Landing | What it enables for the refactor |
|---|---|---|---|
| Native video decode (`decode::{Decoder, Consumer, Frame}`) | moq-video | #2145, #1859, #1854, #2178 | Cut rusty-codecs decoders; the switching driver attaches to `decode::Consumer` |
| Native hardware encode backends (ffmpeg removed, default-on) | moq-video, moq-nvenc, moq-vaapi | #1860, #2042 | Adopt the native codec stack without pulling ffmpeg back in |
| Encoder rate control from the congestion estimate | moq-video | #2303 | Phase-3d adaptive encoding; the `bandwidth::Consumer` signal is available both directions |
| `Subscription` model with `latency_max` plus aggregate clamp | moq-net | #2176, #2349 | Per-viewer latency limit on the wire for `PlaybackPolicy.max_latency` |
| Transparent subscription migration (#2241); note `ROUTE_LINGER` removed (#2419) | moq-net | #2241, #2419 | Seamless roaming and reconnect resume; departure signalling is now immediate |
| Cumulative-cost multi-hop routing plus token path-scoping | moq-net, moq-token | #2424, #2416 | Cost-aware transitive announce forwarding and cryptographic per-peer authorship for the room layer |
| Mid-stream `set_latency` plus `discontinuity()` on the container consumer | moq-mux | consumer.rs:479, :161 | Runtime `PlayoutMode` retuning without re-subscribe, and clean decoder flushes |
| Unsealed `RenditionConfig<E>` plus `Estimate { jitter, bitrate }` and per-rendition metrics | moq-mux | #2420 | Honest catalog bitrate and jitter for a selector, and app-owned catalog tracks with the full lifecycle |
| `Reserved` / `Rendition` catalog gating | moq-mux | catalog/tracks.rs | Initial-publish atomicity, replacing the empty-catalog priming hack |
| Per-frame-fragment empty-batch contract | moq-mux | #2426 | A correctness constraint any new consumer reader must respect (complete on `None`) |
| Shareable timelines, public `Recorder` | moq-mux | #2420 | Aligned-rendition group indexing for seamless switch and backfill |
| hang `broadcast` cross-reference field | hang | video/mod.rs | Transcode passthrough catalogs and cross-broadcast rendition references |
| JIT transcode ladder | moq-transcode | #2140, #2158 | Simulcast supply side minted on demand instead of publisher-encoded |
| Wire-published traffic stats | moq-stats, moq-net stats module | #2380, #2348, #2427, #2430 | A host and model for `spawn_stats_recorder` and the "stats as a track" overlay |
| Subscriber-side bandwidth estimate on the session | moq-net | `recv_bandwidth` | ABR input aligned with what the JS ABR and moq-gst already consume |
