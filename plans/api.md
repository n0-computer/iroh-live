# Public API: remaining work

The core API redesign (phases 1-4, 6-7, 9) is complete and documented in
`completed/api/`. This plan tracks the remaining API surface work.

## Status

- [x] `BroadcastStatus` watcher on `RemoteBroadcast` — `subscribe.rs`
- [x] `RoomEvent` enum — `rooms.rs` (RemoteAnnounced, BroadcastSubscribed)
- [ ] Relay publishing — `LocalBroadcast::relay()` for zero-transcode forwarding
- [ ] Relay convenience methods on `Live` (phase 7.4)
- [ ] Room participant model — `LocalParticipant`, `RemoteParticipant` wrappers
- [ ] `set_enabled()` / `set_muted()` — stubs exist, need encoder pipeline pause/resume
- [ ] `VideoTrack::frames()` async stream API
- [ ] `Quality` enum refinement — current Highest/High/Mid/Low is too coarse;
  let subscriber specify max resolution/framerate instead
- [ ] `DecodeConfig` expansion — viewport hints, preferred pixel format negotiation
- [ ] Source replacement API — swap camera/screen mid-session without teardown
- [ ] Auto-resubscribe on catalog changes (currently manual)

## Key design notes

**Relay publishing:** `iroh-live-relay` binary and `LiveTicket` relay URL
support exist. What remains is `LocalBroadcast::relay()` to mirror published
tracks to a relay without transcoding.

**Mute/enable:** `set_muted(true)` stops sending packets (quick resume).
`set_enabled(false)` stops the encoder entirely (saves CPU). Both are no-op
stubs today — the encoder thread needs a park/resume mechanism.

**Source replacement:** `VideoPublisher::replace_source()` should hot-swap
the underlying `VideoSource` while keeping the encoder running, inserting a
keyframe at the switch point.
