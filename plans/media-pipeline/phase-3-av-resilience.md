# Phase 3: AV Resilience — Overview

## Goal

Bring subscriber-side media quality to libwebrtc grade: adaptive rendition switching, playout timing with jitter buffering, A/V sync, forward error correction, and publisher-side rate control.

## Prerequisites

- Phase 1 complete (Opus + H.264 codecs)
- Phase 2 complete (AV1 codec support)

## Sub-phases

| Phase | Description | Status | Plan |
|-------|-------------|--------|------|
| 3a | Adaptive rendition switching — catalog-aware, signal-driven | Pending | [phase-3a-rendition-switching.md](phase-3a-rendition-switching.md) |
| 3b | Jitter buffer & A/V sync — playout timing, adaptive latency, lip-sync | Pending | [phase-3b-jitter-sync.md](phase-3b-jitter-sync.md) |
| 3c | Forward error correction — Opus FEC/PLC/DTX, comfort noise | Future | [phase-3c-fec.md](phase-3c-fec.md) |
| 3d | Adaptive encoding — encoder rate control, bandwidth estimation, pacing | Future | [phase-3d-adaptive-encoding.md](phase-3d-adaptive-encoding.md) |

## Architecture

```
Publisher                          Subscriber
┌──────────────┐                  ┌──────────────────────────────────────┐
│ VideoEncoder │                  │ SubscribeBroadcast                   │
│  set_bitrate │◄──── 3d ────────│   signals: Watcher<NetworkSignals>   │
│  force_kf    │                  │   │                                  │
│              │                  │   ├──> WatchTrack                    │
│ AudioEncoder │                  │   │    rendition switching (3a)      │
│  FEC/DTX     │◄──── 3c ────────│   │    playout buffer (3b)           │
│              │                  │   │                                  │
└──────────────┘                  │   └──> AudioTrack                    │
                                  │        rendition switching (3a)      │
                                  │        playout buffer (3b)           │
                                  │        FEC/PLC recovery (3c)         │
                                  │                                      │
                                  │ PlayoutClock (3b)                    │
                                  │   shared A/V sync + latency control  │
                                  └──────────────────────────────────────┘
```

## Dependency Graph

```
3a: Rendition Switching ──┐
                          ├──> 3c: FEC (uses NetworkSignals from 3a)
3b: Jitter & Sync       ──┘    │
                                └──> 3d: Adaptive Encoding (uses signals + encoder control)
```

Phases 3a and 3b are independent and can be developed in parallel. Phase 3c builds on 3a's signal infrastructure. Phase 3d builds on 3a's signals and adds publisher-side control.

## Shared Infrastructure

### NetworkSignals

Defined in Phase 3a, used by all sub-phases. Transport-agnostic struct injected from `iroh-live` into `SubscribeBroadcast`:

```rust
pub struct NetworkSignals {
    pub rtt: Duration,
    pub loss_rate: f64,
    pub available_bps: u64,
    pub congested: bool,
}
```

`moq-media` does not depend on `iroh`. Signals are injected as `Option<Watcher<NetworkSignals>>` on `SubscribeBroadcast::new()`. If `None`, auto-switching picks middle quality and the jitter buffer operates without network-aware adaptation.
