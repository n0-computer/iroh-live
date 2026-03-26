# Adaptive Rendition Switching

| Field | Value |
|-------|-------|
| Status | draft |
| Applies to | moq-media |

Network conditions change during a live session — bandwidth drops, loss
spikes, congestion clears. When the publisher offers multiple quality
levels (renditions), the subscriber can switch between them to maintain
smooth playback without manual intervention. This page covers the
selection algorithm, the network signals that drive it, and the
switching mechanism between decoder pipelines.

## Current state

The selection algorithm and switching infrastructure are implemented and
working. Seamless switching (parallel decoder handoff) is designed but not
yet wired up; rendition changes currently cause a brief interruption while
the new decoder starts.

`AdaptiveVideoTrack` wraps a `RemoteBroadcast` and automatically switches
between video renditions based on network conditions. It implements
`VideoSource`, so it can replace a capture source anywhere in the pipeline.

## NetworkSignals

The adaptation algorithm consumes `NetworkSignals`, a transport-agnostic
struct defined in moq-media:

```rust
pub struct NetworkSignals {
    pub rtt: Duration,
    pub loss_rate: f64,          // 0.0..=1.0
    pub available_bps: u64,      // cwnd * 8 / rtt
    pub congestion_events: u64,  // monotonic counter
}
```

moq-media does not depend on iroh. iroh-live produces these signals by
polling `session.conn().stats()` every 200ms and publishing them via a
`watch::Receiver<NetworkSignals>`. If no signals are available (e.g., when
moq-media is used standalone), adaptive switching does not trigger and
the track stays on its initial rendition.

## Selection algorithm

The algorithm is bandwidth-primary, aligning with WebRTC GCC/REMB and
Zoom's simulcast switching approach. Bandwidth estimation drives
decisions; loss rate triggers emergency downgrades; RTT feeds into
bandwidth estimation indirectly (through `available_bps = cwnd * 8 / rtt`)
rather than being used as a direct selection signal.

### Rendition ranking

Renditions are ranked by pixel count (width times height) in descending
order, with bitrate as a tiebreaker. Index 0 is the highest quality.

### Decision rules

The algorithm evaluates signals on each tick (every 200ms by default) and
produces one of four decisions: hold, downgrade, emergency, or start
probe.

**Emergency drop.** If `loss_rate >= 0.20`, immediately switch to the
lowest rendition. No hold timer; the situation is severe enough to act
instantly.

**Sustained downgrade.** If `loss_rate >= 0.10` or the current
rendition's bitrate exceeds 85% of `available_bps`, and these conditions
persist for `downgrade_hold` (500ms), switch to the next lower rendition.
The 85% threshold leaves headroom for protocol overhead and transient
spikes.

**Upgrade probe.** If `available_bps` exceeds the next higher rendition's
bitrate by a factor of `bw_probe_headroom` (1.2x), and conditions have
been good for `upgrade_hold` (4s), begin a probe. The asymmetry between
500ms downgrade and 4s upgrade prevents oscillation: the system drops
quality quickly when conditions deteriorate but requires sustained
evidence of improvement before upgrading.

**Congestion hold.** If the congestion event counter is increasing, reset
the upgrade timer and hold current. Never upgrade during active
congestion.

**Loss recovery hysteresis.** After a loss-triggered downgrade, require
`loss_rate < 0.02` sustained for `upgrade_hold` before considering any
upgrade. The 2% threshold matches WebRTC's definition of "good
conditions."

### AdaptiveConfig

All thresholds and timers are collected in `AdaptiveConfig`:

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `upgrade_hold` | 4s | Sustained good conditions before probing |
| `downgrade_hold` | 500ms | Sustained bad conditions before downgrading |
| `probe_duration` | 3s | How long a probe runs before commit/abort |
| `probe_cooldown` | 8s | Cooldown after failed probe |
| `post_downgrade_cooldown` | 4s | Cooldown after downgrade before probing |
| `loss_downgrade` | 0.10 | Loss rate triggering sustained downgrade |
| `loss_emergency` | 0.20 | Loss rate triggering immediate drop to lowest |
| `loss_good` | 0.02 | Loss rate below which conditions are "good" |
| `loss_probe_abort` | 0.05 | Loss rate above which active probe is aborted |
| `bw_downgrade_ratio` | 0.85 | Bandwidth utilization ceiling |
| `bw_probe_headroom` | 1.2 | Bandwidth excess factor required for probe |
| `check_interval` | 200ms | How often signals are evaluated |

## RenditionMode

Controls whether adaptation is automatic or manually pinned:

```rust
pub enum RenditionMode {
    Auto,
    Fixed(String),
}
```

In `Auto` mode, the algorithm runs on each tick. In `Fixed` mode, the
track subscribes to the named rendition and stays there regardless of
network conditions.

## Seamless switching design

The intended switching mechanism uses a staging channel. When the
algorithm decides to switch renditions:

1. A new `TrackConsumer` is created for the target rendition.
2. A new decoder thread starts, writing decoded frames to a temporary
   staging channel.
3. The adaptation task waits for the first decoded frame from the new
   decoder.
4. That frame is forwarded to the stable output channel.
5. The old decoder thread is cancelled.
6. The new decoder is rewired to write directly to the output channel.

This ensures the consumer sees no gap: the last frame from the old
decoder is followed immediately by the first frame from the new decoder.

This design is specified and the types exist, but the parallel decoder
handoff is not yet connected. Current rendition switches stop the old
decoder, start the new one, and resume output after the first keyframe
arrives on the new track.

## Signal production in iroh-live

iroh-live's `spawn_signal_producer()` polls the iroh connection's path
stats every 200ms and publishes `NetworkSignals`:

- `rtt` from `PathStats::rtt`
- `loss_rate` from delta of `lost_packets / sent_packets` over the
  sampling interval
- `available_bps` computed as `cwnd * 8 / rtt_seconds`
- `congestion_events` from the monotonic counter on `PathStats`

The resulting `watch::Receiver<NetworkSignals>` is passed to `RemoteBroadcast`
at construction time. moq-media reads it but never writes it, preserving
the transport-agnostic boundary.
