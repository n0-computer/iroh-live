# Public API: remaining work

The core API redesign (phases 1–4, 6–7, 9) is complete and documented in
`completed/api/`. This plan tracks the remaining API surface work.

## Status

- [ ] Relay publishing — `LocalBroadcast::relay()` for zero-transcode forwarding
- [ ] Relay convenience methods on `Live` (phase 7.4)
- [ ] Room participant model — `LocalParticipant`, `RemoteParticipant`, `RoomEvent`
- [ ] `set_enabled()` / `set_muted()` — stubs exist, need encoder pipeline pause/resume
- [ ] `BroadcastStatus` watcher on `LocalBroadcast`
- [ ] `VideoTrack::frames()` async stream API
- [ ] `Quality` enum refinement — current Highest/High/Mid/Low is too coarse
- [ ] `DecodeConfig` expansion — viewport hints, preferred pixel format negotiation
- [ ] Source replacement API — swap camera/screen mid-session without teardown
- [ ] Auto-resubscribe on catalog changes (currently manual)

## Details

### Relay publishing (phase 5)

`LocalBroadcast::relay(relay_url)` should forward encoded packets to a
`moq-relay` instance without transcoding. The relay acts as an SFU, fanning
out to multiple subscribers. This bridges iroh P2P sessions with
WebTransport/H3 browser clients.

The `iroh-live-relay` binary and `LiveTicket` relay URL support already exist.
What remains is the `LocalBroadcast` method that opens a second transport
connection to the relay and mirrors published tracks.

### Room participant model (phase 8)

The current room API (`rooms.rs`) manages connection state but does not expose
a participant abstraction. The target design:

- `LocalParticipant` wraps the local `LocalBroadcast` with room-scoped methods
- `RemoteParticipant` wraps a `RemoteBroadcast` with identity and metadata
- `RoomEvent` enum: `ParticipantJoined`, `ParticipantLeft`, `TrackPublished`,
  `TrackUnpublished`
- Event stream via `n0_watcher::Watchable`

### Mute and enable

`set_muted(true)` should stop sending media packets without tearing down the
encoder pipeline (quick resume). `set_enabled(false)` should stop the encoder
entirely (saves CPU). Both are stubs today — the encoder thread needs a
park/resume mechanism similar to `SharedVideoSource` but at the pipeline level.

### Source replacement

Swapping a camera source mid-call currently requires stopping the old encoder
pipeline and starting a new one, which causes a visible gap. A
`VideoPublisher::replace_source()` method should hot-swap the underlying
`VideoSource` while keeping the encoder running, inserting a keyframe at the
switch point.

### Quality and decode config

The `Quality` enum (Highest/High/Mid/Low) maps to vague concepts. A better
approach: let the subscriber specify a maximum resolution and framerate, and
let the adaptive layer pick the closest rendition. `DecodeConfig` should carry
viewport dimensions so the decoder can skip post-decode scaling when the
hardware supports it natively.

## Implementation history

- `9dde4bd`..`dc2aaed` — phases 1–4 (rename, slot API, subscription options, domain errors)
- `7dc83b5`..`61da1a6` — phases 6–7 (Live builder, Call, IncomingSession)
- `90f0b64` — phase 9 (remove LiveNode, consolidate into Live)
- `d78d23c` — relay binary and browser E2E
- `6f7090d` — relay URL in tickets
