# Instrumentation and tests

Debugging a real-time pipeline means seeing frame timing, network conditions, and
codec behaviour while the system runs at 30 frames a second. Two pieces cover
that: statistics snapshots in `iroh-live-media` and an overlay in
`iroh-live-egui` that draws them.

## Metrics

`LocalBroadcast::stats()` and `Player::stats()` return plain values,
`PublishStats` and `PlaybackStats`, read as often as a UI draws. There are no
string keys and no registration, and no history: a snapshot carries current
values only.

`PublishStats` carries the source's frame rate, which every rendition of a
ladder shares, one `EncodeStats` per video rendition (the encoder backend that
opened, the encoded size, frames per second, bitrate over the last second, a
smoothed encode time, and running frame and byte counts), and an
`AudioEncodeStats` with the codec, the frames written, and the frames dropped
because the broadcast fell behind its source.

`PlaybackStats` carries a `VideoPlaybackStats` while video plays (the rendition
on screen, the decoder backend, the picture size, frames shown per second, a
smoothed read-and-decode time, frames shown, and access units skipped), an
`AudioPlaybackStats` while audio plays (the rendition, how much audio is queued
ahead of the speaker, and the most recent peak for a meter), the playout
latency, and the last `NetworkSample` if the broadcast has network signals.

`LocalBroadcast::status()` and `Player::status()` complement them as watchers:
the state of each slot, which renditions are encoding, the rendition on screen
and the one warming up, and why the last switch failed.

## What is filled in today

Every figure has exactly one writer. Each rendition's encoder writes its own
entry, the source's frame rate is written by the one task that reads the
source, the player's video task writes the video figures, its audio task the
audio ones, and its selector the network sample. The shared counters every
encoder used to write into, where a ladder's labels named whichever rung wrote
last and its bitrate was a smoothed value somewhere among the rungs, are gone.

Rates are counted over a window rather than derived from the gap between two
events: one late frame in a 30 fps stream reads as 50 by the gap and as 30 by
the count.

For judging pacing and sync, `Player::timeline()` returns the last few hundred
frames of each medium with when each left its decoder and when it was
presented: a picture when it was handed to the player's frames, audio when it
reaches the speaker, which is when it was written plus what was queued ahead of
it. There is no per-path lag figure.

## The debug overlay

`iroh_live_egui::overlay::DebugOverlay` draws a translucent bar along the bottom
of a video tile with one clickable section per `StatCategory`: `Net`, `Capture`,
`Render`, `Audio`, and `Time`. Clicking a section opens a detail panel above the bar,
stacking upward, with each figure shown as a value, a colour where one applies,
and a sparkline next to those that change over time. The overlay keeps the
twelve seconds of history behind its sparklines itself, recording a point every
100 ms, since the snapshots carry none.

`show_publish(ui, rect, &stats, &status)` draws a broadcast: `Capture` with the
source frame rate, each rendition's encoder, the audio encoder, and every slot's
state, and `Net` with the encoded video leaving the broadcast summed over its
renditions. `show_playback(ui, rect, &stats, &status, &timeline)` draws a
player: `Net` with the round trip, loss, and the sender's delivery estimate,
`Render` with the rendition mode, rendition, decoder, frame rate, decode time,
and the last switch error, `Audio` with the buffer and the playout latency, and
`Time` with the player's timeline over the last ten seconds: how long each
picture was held between decoder and screen, picture and audio cadence, the
A/V offset, the audio buffer and the round trip. Scrolling over the timeline
pauses it and moves back in time, and a double click returns to the live edge.
A caller reads `Player::timeline()` only while `timeline_open()` says the panel
is showing.

`irl publish --preview` enables the `Capture` and `Net` categories; `irl watch`
enables `Net`, `Render`, `Audio`, and `Time`.

## Tests

`iroh-live/tests/e2e.rs` runs four tests over a real QUIC connection between two
iroh endpoints. Every source is generated, so no camera, microphone, or speaker is
needed, but the codecs are real: openh264 and Opus encode and decode, and the
bytes cross an actual transport. `publish_subscribe_video` asserts five frames
with non-zero size and non-decreasing timestamps. `publish_subscribe_audio`
plays into `AudioOutput::null()`, so it proves the transport and the codec
without needing an output device, and waits for the player's stats to count
decoded audio frames. `adaptive_rendition_switching` replaces the network signals
`Live::subscribe` attached with a closure over a made-up `NetworkSample`,
through `RemoteBroadcast::with_network`, and asserts the downgrade lands. `changing_the_decoder_backend_rebuilds_it` switches a playing
player to the software decoder and asserts the rebuilt decoder is the one
producing frames.

`iroh-live/tests/latency.rs` measures capture-to-decode latency with publisher
and subscriber in one process, once with `Latency::IMMEDIATE` and once with the
default, and prints the figures.

`iroh-live-rooms/tests/room.rs` covers discovery, subscription, privacy, and peer
departure. Nothing there touches media: the broadcasts carry a plain data track
with hand-written frames, since `iroh-live-rooms` has no media dependency.

`iroh-live-relay/tests/relay_bridge.rs` covers bridging between the WebTransport
and iroh sides of the relay. `tests/e2e-browser/` is a Playwright suite that
builds the relay and the CLI, serves the embedded web client, and watches a
stream in Chromium.

`iroh-live/tests/patchbay.rs` is the only place anything impairs a link. It puts
the publisher and the subscriber in separate network namespaces with a router
between them and applies netem latency, jitter and loss, so the impairment
reaches QUIC rather than being described to the pipeline after the fact. Two
tests hold the delivery cadence to account across a latency ramp and a loss
spike; `adaptation_follows_a_real_link` runs the whole adaptive chain, from
dropped packets through QUIC's loss detection and the path statistics the
session's link monitor samples to a rendition downgrade, and back up once the
loss clears; `adaptation_follows_a_rate_limit`,
`adaptation_holds_steady_under_a_marginal_cap` and
`a_risen_baseline_round_trip_does_not_downgrade` do the same for a bandwidth
cap, a cap that sits on a rung's threshold, and a path that got longer;
`a_switch_does_not_blank_the_picture` holds the decode supervisor to its overlap,
that a replacement decoder warms up beside the incumbent and takes over rather
than opening after the incumbent is gone. The adaptation tests shorten the
player's timers, but not its thresholds, through `PlayerConfig::adaptation`.
It is Linux-only and needs
unprivileged user namespaces, set up from an ELF initialiser before the harness
has a second thread. nextest
gives the binary a single-threaded group of its own, because the timing
assertions do not survive sharing a machine with the rest of the suite.

```sh
cargo make test           # cargo nextest run --locked --workspace
cargo make test-patchbay  # the network simulation suite, including ignored tests
cargo make test-e2e       # builds the relay and CLI, then runs Playwright
cargo make test-full      # check-all, then both of the above
```

## What is gone

The `frame_dump` example, which saved frames as PNGs and checked them against an
SMPTE pattern by PSNR, was removed with the in-house decoder it drove. The
`pi-zero-demo codec-test` subcommand went with the V4L2 M2M codec it tested.

The patchbay suite went the same way when the pipeline it drove was replaced, but
it is back, rewritten against the new one; the A/V sync measurements it also
carried are not, because the timestamping audio backend they read from has no
counterpart yet.
