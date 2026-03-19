# Nightly codebase review — 2026-03-18

Systematic review of public API surface, error handling, concurrency, and
design across all workspace crates. Findings are grouped by theme rather than
crate to make cross-cutting patterns visible.

## Public API assessment

### moq-media

The publish and subscribe APIs are the most-used surfaces and have the most
room for improvement.

**Stubs in the public API.** `VideoPublisher::set_enabled()` and
`AudioPublisher::set_muted()` silently ignore their arguments. These are
public methods on public types with no documentation that they are no-ops.
A caller who depends on mute behavior will see no error and no effect. They
should either be removed from the public API, return `Err(unimplemented)`, or
be implemented.

**Confusing VideoTarget / Quality / CatalogSnapshot interaction.**
`VideoOptions::quality()` sets a `Quality` enum that is internally converted
to a `VideoTarget`. `CatalogSnapshot::select_video_rendition()` also takes
`Quality`. `VideoTarget::rendition()` pins to a specific name, bypassing
quality-based selection. The precedence rules (rendition pin wins over quality)
are not documented. A user reading these APIs cannot predict which controls
which without reading the implementation.

**VideoPublisher naming inconsistency.** `set()` and `replace()` do the same
thing — `replace()` is documented as "equivalent to `set()`". One should be
removed or given distinct semantics (e.g., `replace` implies hot-swap
mid-stream while `set` implies first-time configuration).

**renditions() allocates on every call.** `VideoPublisher::renditions()`
builds a `Vec<String>` from the internal map on each invocation. High-frequency
callers (e.g., UI polling) pay allocation costs for no reason. Return a
cached slice or an iterator instead.

**Leaky PlayoutClock abstraction.** Users must call
`remote_broadcast.clock().set_buffer()` and `.mode()` to tune playout
behavior. The clock is an implementation detail that leaked into the public
surface. `RemoteBroadcast` should expose `set_playout_mode()` and
`set_playout_buffer()` directly.

**AdaptiveVideoTrack::format() returns zeroed dimensions.** The `VideoSource`
trait impl returns `VideoFormat { dimensions: [0, 0], .. }`. Any code that
reads format for layout (e.g., encoder config from source) will break. Should
return the current track's dimensions dynamically.

**No probe state observability in AdaptiveVideoTrack.** When debugging "why
isn't it switching renditions?", there is no API to query whether a probe is
active, what the current bandwidth estimate is, or why a decision was made.
Adding a `AdaptiveVideoTrack::debug_state()` or tracing spans would help.

**Pipeline thread errors are invisible.** Encoder and decoder pipelines spawn
OS threads. If a thread panics or errors, the pipeline silently stops
producing frames. No panic hook, no error channel, no health check. Callers
see a frozen video stream with no diagnostic information.

**Hardcoded 30 fps in VideoEncoderPipeline.** `frame_duration` is computed as
`1/30` regardless of the encoder config's framerate. A 60 fps source paced at
30 fps drops half its frames silently.

### iroh-live

**`subscribe()` returns a tuple.** `Live::subscribe()` returns
`(MoqSession, RemoteBroadcast)`. The lifetime relationship between these two
objects is unclear — dropping the session closes the broadcast, but the type
system does not enforce this. A wrapper type like `ActiveSubscription` would
make ownership explicit.

**`Call::closed()` always returns `RemoteClose`.** The disconnect reason from
the QUIC connection is ignored. Whether the local side closed, the remote side
closed, or the transport errored, the caller always sees `RemoteClose`.

**spawn_thread panics on failure.** `util::spawn_thread()` is a public
function that calls `.unwrap()` on `thread::Builder::spawn()`. Thread spawn
can fail (resource exhaustion). This should return `Result<JoinHandle>`.

**Ticket serialization panics.** `LiveTicket::to_bytes()`,
`LiveTicket::serialize()`, `CallTicket::serialize()`, and
`rooms.rs:postcard::to_stdvec()` all call `.unwrap()` on postcard
serialization. While postcard serialization of these types should never fail
for valid inputs, panicking in a library's `Display` impl is unacceptable.
Use `.expect()` with a message at minimum, or return `Result`.

**Room gossip dependency is implicit.** `Room::new()` fails at runtime if
gossip was not enabled on `Live`. The type system does not prevent this
mistake, and there is no `Live::can_join_room()` predicate.

**RoomEvent::RemoteConnected is defined but never emitted.** The enum variant
exists in the public API, but no code path produces it. Callers waiting for
this event will wait forever.

**Two serialization APIs on tickets.** `to_bytes()` / `from_bytes()` (raw
postcard) and `serialize()` / `deserialize()` (base64url-wrapped postcard)
serve the same purpose. The naming does not clarify when to use which.
Consolidate to one default, make the other internal.

### iroh-moq

**`subscribe()` takes `&mut self` unnecessarily.** The `OriginConsumer` is
internally shared. The mutable borrow prevents concurrent subscriptions from
the same session handle, which is a common pattern (subscribe to video +
audio simultaneously).

**`subscribe()` blocks indefinitely.** No timeout parameter, no cancellation
mechanism other than dropping the future. If the remote never announces the
requested name, the caller hangs forever.

**`publish()` takes `String` but `subscribe()` takes `&str`.** Inconsistent
parameter types for the same concept (broadcast name).

**`published_broadcasts()` returns empty on actor death.** If the internal
actor task panics, `published_broadcasts()` returns `Vec::new()` via
`.unwrap_or_default()`. The caller cannot distinguish "no broadcasts" from
"actor crashed".

**Silent `.ok()` on critical oneshot sends.** At least five locations in the
actor loop discard send errors with `.ok()`. If a reply channel is dropped
before the actor responds, the error is invisible.

## Error handling patterns

The codebase has a recurring pattern of `.expect("poisoned")` on mutex locks
(~20 locations across `publish.rs`, `playout.rs`, `audio_backend.rs`). Mutex
poisoning happens when a thread panics while holding the lock. The current
approach propagates the panic to every subsequent caller. For internal locks
that are always held briefly, this is defensible but should be commented. For
locks shared with callback threads (audio backend, V4L2), poisoning is a real
risk and should be handled gracefully.

`anyhow::Result` is used as the error type in many public APIs. This makes
it impossible for callers to match on specific error variants. Where distinct
failure modes exist (codec not available, device not found, connection refused),
domain error types should be used instead.

## Concurrency concerns

**SharedVideoSource park/unpark timing.** The source thread uses
`thread::park()` to sleep when no subscribers exist. Wakeup depends on
`unpark()` being called before the next `park()` check. If the wakeup is
missed (race between subscriber count check and park), the thread hangs.
The existing test (`replace_video_while_source_parked`) uses a 100 ms sleep
to work around this, confirming the race exists. A condvar would be more
reliable.

**PlayoutClock mutex on every frame.** `observe_arrival()`, `playout_time()`,
and `jitter()` all lock an `Arc<Mutex<ClockInner>>`. At 60 fps, this is
60 lock/unlock cycles per second per track. The jitter field is diagnostic
only and could use `AtomicU64` instead.

**PlayoutBuffer 1 ms polling.** `recv_timeout()` in the playout buffer uses
`thread::sleep(1ms)` in a polling loop. On low-power devices (Pi Zero), this
adds measurable latency and CPU overhead. A condvar or `recv_deadline()` would
be better.

**Adaptive rendition switch failure loops forever.** If `switch_rendition()`
fails repeatedly (e.g., all decoders broken), the adaptation task logs warnings
but never backs off or gives up. It will keep trying at the probe interval
indefinitely.

## Documentation gaps

Most public types and methods across `iroh-live`, `iroh-moq`, and `moq-media`
lack doc comments. Key gaps:

- `Live::subscribe()`: no docs on what happens if called twice for the same remote+name
- `MoqSession`: no module-level docs explaining the actor pattern or lifecycle
- `MoqSession`: no docs on connect deduplication (the actor deduplicates, but this is invisible)
- `PlayoutMode::Live { buffer, max_latency }`: no docs explaining the relationship between the two parameters or validation rules
- `NetworkSignals`: no docs on how each field is computed or what valid ranges are
- `TrackName` enum exists but room code hardcodes string broadcast names — unclear which to use
- `DisconnectReason` has only three variants, losing information from QUIC close codes

## rusty-codecs findings

**Per-frame allocations in hot paths.** Several codecs allocate fresh `Vec`s
on every frame. Fixed items are struck through:
- ~~Opus encoder: `.to_vec()` on encode output buffer~~ — inherent copy for owned packet, encode buffer already reused
- ~~H.264 decoder: `RgbaImage::from_raw()` allocates per frame~~ — removed RgbaImage wrapper, stores raw Vec<u8>
- ~~Scaler: `ImageStoreMut::alloc()` on every `scale_rgba()` call~~ — caches destination buffer across calls
- ~~V4L2 decoder: `copy_plane()` allocates even when stride == width~~ — returns Cow<[u8]> for zero-copy fast path
- ~~SPS/PPS extraction `.to_vec()` per keyframe~~ — returns borrowed slices
- NAL conversion: `annex_b_to_length_prefixed()` and
  `length_prefixed_to_annex_b()` still allocate per packet (remaining)

**VAAPI device hardcoded.** Both the native VAAPI encoder/decoder and the
FFmpeg VAAPI path hardcode `/dev/dri/renderD128`. Multi-GPU systems or renamed
device nodes silently fail. Should try multiple paths or accept an env var.

**Opus pre-skip always zero.** The Opus encoder signals 0 pre-skip in the
OpusHead, but the standard Opus encoder delay is 312 samples (6.5 ms at
48 kHz). This can cause A/V sync drift when interoperating with other Opus
implementations.

~~**Vulkan command pool not destroyed.**~~ Verified: `DmaBufImporter` has a
proper `Drop` impl that destroys the command pool, fence, and cached images.
This was a false positive in the initial review.

**AV1 decoder stride assumption.** `picture.stride()` is cast to u32 and
used without validating it is >= width. If rav1d returns a smaller stride
(should not happen, but not asserted), the plane copy underflows.

## rusty-capture findings

**PipeWire pod parsing without bounds check.** `parse_format_pod()` computes
pod size from a raw pointer header and creates a slice without validating that
the computed size fits the actual allocation. A malformed pod could cause
out-of-bounds reads.

**X11 SHM cleanup ignores errors.** `stop_capture()` calls `shm::detach()`
and `shmdt()` with `.ok()` and no return value check. If the X11 connection
died, the SHM segment is leaked without any log message.

**CameraConfig::select_format() falls back silently.** If `preferred_format`
is set but no camera supports it, the function silently ignores the preference
and selects by resolution/fps only. Not documented.

**Frame drop logging is sparse.** PipeWire frame drops are logged at
power-of-two counts, meaning long-running sessions can drop hundreds of frames
between log messages. The threshold should be configurable or log at a fixed
interval (e.g., every 10 seconds during active drops).
