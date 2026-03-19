# Developer Tools

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
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

The `frame_dump` example captures frames from a publish-subscribe pipeline and computes PSNR against a known SMPTE test pattern. This provides end-to-end frame verification: capture source generates a test pattern, the codec pipeline encodes and decodes it, and the output is compared against the expected image. Useful for validating codec correctness and measuring compression quality.

## On-device codec testing

The `pi-zero-demo` includes a `codec-test` subcommand for testing V4L2 hardware codecs directly on the Raspberry Pi. It exercises the encoder and decoder at multiple resolutions, reports frame rates, and checks for driver-specific issues (level negotiation, SPS/PPS handling, pixel format quirks).
