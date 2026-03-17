# API Sketch

The canonical API sketch is now a compiling Rust example:

**`iroh-live/examples/api_sketch.rs`**

It uses real imports from the actual crates (iroh, iroh-moq, moq-media, iroh-live).
Example modules demonstrate the implemented API:

- `ex_one_to_many` — publish + subscribe
- `ex_call_raw` — 1:1 call with raw primitives
- `ex_call_with_helper` — 1:1 call with Call sugar
- `ex_call_with_screenshare` — screen share as second broadcast
- `ex_dashboard` — quality-constrained multi-feed
- `ex_camera_switch` — source replacement mid-stream

Examples requiring unimplemented features (relay, room redesign) are
kept as commented-out design references.

Run `cargo check -p iroh-live --example api_sketch --all-features` to verify it compiles.
