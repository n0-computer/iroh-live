# API Sketch

The canonical API sketch is now a compiling Rust example:

**`iroh-live/examples/api_sketch.rs`**

It defines all types as stubs with `todo!()` bodies, organized as in-file modules
mirroring the real crate layout:

- Placeholder external types (Endpoint, Gossip, Router, etc.)
- `iroh_moq` — MoqSession, Moq, IncomingSession, protocol handler
- `moq_media` — LocalBroadcast, RemoteBroadcast, VideoTrack, AudioTrack, codecs, options
- `iroh_live` — Live, Call, Room, participants, TrackName, events

Plus ~18 usage example modules covering all use cases.

Run `cargo check -p iroh-live --example api_sketch` to verify it compiles.
