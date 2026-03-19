# Developer Tools

| Field | Value |
|-------|-------|
| Status | draft |
| Applies to | moq-media-egui, iroh-live, moq-media |

## Debug overlay

The `split` example includes a collapsible debug overlay rendered per video tile. When enabled, it shows a stats bar with RTT, downstream/upstream bandwidth, and frame delay as large monospace numbers. A colored dot indicates connection quality: green when RTT is under 50 ms and loss under 1%, yellow under 150 ms/5%, red otherwise.

The detail panel (toggled by a button) covers the tile with a semi-transparent background and displays per-session information: remote endpoint ID, per-path connection stats (RTT, congestion window, MTU, lost packets/bytes, bandwidth, congestion events), catalog contents with the currently selected rendition, and buffering state.

Per-path stats are read from `MoqSession::conn().paths().get()`, which returns a `PathInfoList`. Each `PathInfo` exposes `rtt()`, `is_selected()`, `remote_addr()`, and `stats()` with the full `PathStats` struct.

## Metrics infrastructure

`Metric` provides EMA (exponential moving average) smoothing for noisy per-frame measurements. `Label` formats metrics for display with unit suffixes. Typed metric groups (`NetStats`, `CaptureStats`, `RenderStats`) collect related measurements. These are defined in moq-media-egui and used by the debug overlay.

## Network simulation

The [patchbay](https://crates.io/crates/patchbay) crate (by n0-computer) provides Linux user-namespace network labs with configurable NAT topologies, routing, and link impairment (latency, loss, bandwidth caps, jitter) via tc/netem. This is the planned approach for testing the media pipeline under realistic network conditions.

The current e2e tests in `iroh-live/tests/e2e.rs` inject synthetic `NetworkSignals` to exercise the adaptive rendition switching algorithm without requiring network namespaces. The `adaptive_rendition_switching` test verifies that the adaptation logic correctly downgrades and upgrades renditions when signal values change.

patchbay integration would add kernel-level packet impairment that affects the actual QUIC congestion controller, NAT traversal testing across different topologies, and verification that sessions fall back to relay when direct connectivity fails.

## frame_dump example

The `frame_dump` example captures frames from a live broadcast and saves them as PNGs for visual inspection. It can also verify frames against the expected SMPTE test pattern by computing PSNR on the color bar region.

```sh
# Publish a test pattern
cargo run --example frame_dump -- publish

# In another terminal, watch and verify
cargo run --example frame_dump -- watch <TICKET> --out /tmp/frames --verify

# Generate reference PNGs without network
cargo run --example frame_dump -- reference --out /tmp/ref
```

The PSNR check compares each frame's color bar region against the known SMPTE bar colors, ignoring the animated bouncing line. Values above 30 dB indicate correct codec operation; below 20 dB suggests color space errors or decode failures.

## On-device codec testing

The `pi-zero-demo` includes a `codec-test` subcommand for testing V4L2 hardware codecs directly on the Raspberry Pi without any network involvement:

```sh
ssh pi@livepizero "./pi-zero-demo codec-test all --frames 60"
ssh pi@livepizero "./pi-zero-demo codec-test roundtrip --frames 30 --width 1280 --height 720"
```

It exercises the encoder, decoder, and full roundtrip at configurable resolutions, reports frame rates, and can save decoded frames for visual inspection.
