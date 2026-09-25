# Instrumentation and tests

Two pieces show what a running pipeline does: statistics snapshots in
`iroh-live-media` and an overlay in `iroh-live-egui` that draws them.

## Metrics

`LocalBroadcast::stats()` and `Player::stats()` return plain values,
`PublishStats` and `PlaybackStats`, cheap enough to read on every redraw. A
snapshot has current values only. The code is in `iroh-live-media/src/stats.rs`.

`PublishStats` has the source's frame rate, one `EncodeStats` per video
rendition, and an `AudioEncodeStats`. `EncodeStats` has the encoder backend,
the encoded size, frames per second, bitrate, a smoothed encode time, and frame
and byte counts. `AudioEncodeStats` has the codec, the frames written, and the
frames dropped because the broadcast fell behind its source.

`PlaybackStats` has a `VideoPlaybackStats` while video plays: the rendition,
the decoder backend, the picture size, frames per second, a smoothed
read-and-decode time, frames shown and access units skipped. It has an
`AudioPlaybackStats` while audio plays: the rendition, the audio queued ahead
of the speaker, the latest peak and the frame count. It also has the playout
latency and the last `NetworkSample`, if the broadcast has network signals.

Every figure has one writer. Each rendition's encoder writes its own entry, the
task that reads the source writes the source frame rate, and the player's video
and audio tasks write their own figures. Rates are counted over a window
instead of derived from the gap between two events. One late frame in a 30 fps
stream reads as 50 by the gap and as 30 by the count.

`LocalBroadcast::status()` and `Player::status()` add watched state: each
slot's state, which renditions are encoding, the rendition on screen and the
one warming up, and why the last switch failed.

For pacing and sync, `Player::timeline()` returns the last few hundred frames
of each medium, with when each left its decoder and when it was presented. A
picture is presented when it is handed to the player's frames. Audio is
presented when it reaches the speaker, which is when it was written plus the
audio queued ahead of it.

## The debug overlay

`iroh_live_egui::overlay::DebugOverlay` draws a bar along the bottom of a video
tile, with one clickable section per `StatCategory`: `Net`, `Capture`,
`Render`, `Audio` and `Time`. Clicking a section opens a detail panel above the
bar. Values that change over time get a sparkline. The overlay keeps the
history itself: one point every 100 ms, twelve seconds in all.

`show_publish(ui, rect, &stats, &status)` draws a broadcast. `Capture` shows the
source frame rate, each rendition's encoder, the audio encoder and each slot's
state. `Net` shows the encoded video bitrate summed over the renditions.

`show_playback(ui, rect, &stats, &status, &timeline)` draws a player. `Net`
shows the round trip, loss and the publisher's delivery estimate, plus any
lines the caller set with `set_link`. `Render` shows the rendition mode, the
rendition, the decoder, the frame rate, the decode time and the last switch
error. `Audio` shows the buffer and the playout latency. `Time` draws the
player's timeline over the last ten seconds: how long each picture was held
between decoder and screen, picture and audio cadence, the A/V offset, the
audio buffer and the round trip. Scrolling over it pauses it and moves back in
time, and a double click returns to the live edge. Read `Player::timeline()`
only while `timeline_open()` is true.

`irl publish --preview` shows `Capture` and `Net`. `irl watch` shows `Net`,
`Render`, `Audio` and `Time`.

## Tests

`iroh-live/tests/e2e.rs` runs over a real QUIC connection between two iroh
endpoints. Every source is generated, so no camera, microphone or speaker is
needed, but the codecs are real.

- `publish_subscribe_video` checks five frames for a non-zero size and
  non-decreasing timestamps.
- `a_call_reads_the_other_side` dials a `Call` and plays the other side.
- `publish_subscribe_audio` plays into `AudioOutput::null()` and waits for the
  player's stats to count decoded audio.
- `adaptive_rendition_switching` replaces the network signals with a closure
  over a made-up `NetworkSample` and checks that the downgrade lands.
- `changing_the_decoder_backend_rebuilds_it` switches a playing player to the
  software decoder and checks that the new decoder produces the frames.

`iroh-live/tests/latency.rs` measures capture-to-decode latency with publisher
and subscriber in one process, and prints the figures.

`iroh-live-rooms/tests/room.rs` covers discovery, subscription, privacy and
members leaving. The broadcasts carry a plain data track, since
`iroh-live-rooms` has no media dependency.

`iroh-moq/tests/` covers sessions, the node and the moq routing behaviour the
node relies on. `iroh-live-relay/tests/relay_bridge.rs` covers bridging between
the WebTransport and iroh sides of the relay. `tests/e2e-browser/` is a
Playwright suite that builds the relay and the CLI, serves the embedded web
client, and watches a stream in Chromium.

`iroh-live/tests/patchbay.rs` is the only place that impairs a link. It puts
the publisher and the subscriber in separate network namespaces with a router
between them and applies netem latency, jitter, loss and rate limits, so the
impairment reaches QUIC. The tests cover frame delivery through a latency ramp
and a loss spike, adaptation to loss, to a rate limit and to a marginal cap, a
longer round trip that must not downgrade, and switches that must not blank the
picture. They shorten the player's timers, but not its thresholds, through
`PlayerConfig::adaptation`. The suite is Linux-only and needs unprivileged
user namespaces, which a `ctor` initializer sets up before the harness starts
threads. nextest runs the patchbay tests one at a time, since two at once
starve the software encoder.

```sh
cargo make test           # cargo nextest run --locked --workspace
cargo make test-patchbay  # the patchbay suite, including ignored tests
cargo make test-e2e       # builds the relay and CLI, then runs Playwright
cargo make test-full      # check-all, test and test-e2e
```
