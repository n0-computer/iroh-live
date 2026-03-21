# Debug overlays — remaining work

Phases 1–3 (MetricsCollector, DebugOverlay, sparklines, frame timing)
are complete. See
[docs/architecture/devtools.md](../docs/architecture/devtools.md) for
the current infrastructure.

## Phase 4: remaining items

- [ ] A/V sync offset measurement — compare audio playout time vs video
  render time for matching PTS range
- [ ] Adaptive algorithm state visibility — current rendition, limitation
  reason, probe state in the overlay
- [ ] Publish-side encode_ms and capture fps metrics recording in
  `publish.rs` pipeline stats

## Typed metrics refactor

Replace the stringly-typed `MetricsCollector` (BTreeMap lookups) with
typed stat structs. Eliminates runtime string keys, enables compile-time
checked pipeline records.

### Core types
- `Metric` — EMA + ring buffer for time-series values
- `Label` — atomic string for state labels (rendition name, codec)

### Stat category structs
- `NetStats` { rtt, loss, bw, cwnd }
- `CaptureStats` { fps, frame_time, drops }
- `RenderStats` { upload_ms, draw_ms, dropped }
- `TimingStats` { encode_ms, decode_ms, playout_offset }

### Composites
- `SubscribeStats` owns NetStats + RenderStats + TimingStats
- `PublishStats` owns CaptureStats + TimingStats

## Timeline panel redesign

150px panel with five lanes: latency graph, video frames, audio frames,
A/V sync, RTT. Scrolling timeline, drift detection, frame drop
visibility, lip-sync drift display.
