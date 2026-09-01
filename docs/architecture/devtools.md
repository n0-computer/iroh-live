# Instrumentation and tests

Debugging a real-time pipeline means seeing frame timing, network conditions, and
codec behaviour while the system runs at 30 frames a second. Two pieces cover
that: a metrics vocabulary in `moq-media` and an overlay in `moq-media-egui` that
draws it.

## Metrics

`moq_media::stats` defines two primitives and groups them into typed structs, so
there are no string keys and no registration.

A `Metric` holds an exponentially smoothed current value and a ring buffer of
history for sparklines. `MetricMeta` carries the label, the unit, the smoothing
factor, and optional `Thresholds` that colour the value green, yellow, or red.
`Thresholds::inverted` flips the comparison for a metric where higher is better,
such as frame rate. A `Label` is a string that changes rarely, such as the
decoder backend that opened.

The groups are `NetStats` (round-trip time, loss, bandwidth in both directions,
path type and address), `EncodeStats` (frame rate, encode time, bitrate, and
labels for codec, encoder, and resolution), `RenderStats` (frame rate, decode
time, and labels for decoder, renderer, and rendition), and `TimingStats` (audio
buffer depth, per-path lag, and the A/V delta). `PublishStats` and
`SubscribeStats` bundle the ones each side needs, and `Timeline` records
per-frame arrival, decode, and render instants for the timeline panel.

## What is filled in today

The publish path records `encode.encoder`, `encode.resolution`, `encode.encode_ms`,
and `encode.bitrate_kbps`. `iroh-live`'s `util::spawn_stats_recorder` fills
`NetStats` from the iroh connection's selected path every 200 ms. The egui overlay
sets `render.rendition` from the track.

Everything else is defined and unwritten. `TimingStats`, `Timeline`, and
`LagTracker` have no producer in this repository, so the timing panel and the
timeline read zero. `render.decode_ms` is never recorded, and `render.fps` is
recorded as the constant `1.0` rather than a measured rate, so it reports 1.0
rather than a frame rate. Wiring those back up is outstanding work, not a
configuration step.

## The debug overlay

`moq_media_egui::overlay::DebugOverlay` draws a translucent bar along the bottom
of a video tile with one clickable section per `StatCategory`: `Net`, `Capture`,
`Render`, and `Time`. Clicking a section opens a detail panel above the bar,
stacking upward, with each metric shown as a value, a unit, a threshold colour,
and a sparkline once it has at least two samples.

`irl publish --preview` enables the `Capture` and `Net` categories; `irl watch`
enables `Net`, `Render`, and `Time`.

The `Time` category also draws a timeline panel over a ten-second window: a
latency graph, one lane of video frame boxes coloured by inter-frame gap with a
white edge on keyframes, an audio lane, an A/V offset lane around a zero line, and
sparklines for audio buffer depth and round-trip time. The mouse wheel scrolls
back in time and switches the indicator from `LIVE` to `PAUSED`; a double click
returns to live. It reads `Timeline`, so it stays empty until something records
into it.

## Tests

`iroh-live/tests/e2e.rs` runs three tests over a real QUIC connection between two
iroh endpoints. Every source is generated, so no camera, microphone, or speaker is
needed, but the codecs are real: openh264 and Opus encode and decode, and the
bytes cross an actual transport. `publish_subscribe_video` asserts five frames
with non-zero size and non-decreasing timestamps. `publish_subscribe_audio`
decodes through `moq_audio::decode::Consumer` rather than the playback engine, so
it proves the transport and the codec without needing an output device.
`adaptive_rendition_switching` drives the adaptation loop with made-up
`NetworkSignals` and asserts the downgrade lands.

`iroh-rooms/tests/room.rs` covers discovery, subscription, chat, and peer
departure. Nothing there touches media: the broadcasts carry a plain data track
with hand-written frames, since `iroh-rooms` has no media dependency.

`iroh-live-relay/tests/relay_bridge.rs` covers bridging between the WebTransport
and iroh sides of the relay. `tests/e2e-browser/` is a Playwright suite that
builds the relay and the CLI, serves the embedded web client, and watches a
stream in Chromium.

```sh
cargo make test        # cargo nextest run --locked --workspace
cargo make test-e2e    # builds the relay and CLI, then runs Playwright
cargo make test-full   # check-all, then both of the above
```

## What is gone

The `frame_dump` example, which saved frames as PNGs and checked them against an
SMPTE pattern by PSNR, was removed with the in-house decoder it drove. The
patchbay network-simulation test was removed along with the pipeline it
exercised, and its dev-dependency, its cargo-make task, and its nextest test
group went with it. The `pi-zero-demo codec-test` subcommand went with the V4L2
M2M codec it tested.
