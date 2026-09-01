# Adaptive rendition switching

A publisher that offers several renditions lets a subscriber follow its own
downlink. `VideoTrack::enable_adaptation(signals)` starts a task that reads
transport signals every 200 ms, asks `moq_media::adaptive::evaluate` what to do,
and requests a switch when the answer is not "hold". The decoder swap itself is
the video supervisor's job, described in [subscribing](subscribe.md).

`moq_mux::select` fixes a rendition at construction and `moq_video::encode::rate`
backs the sender's bitrate off, so neither covers a subscriber choosing for
itself. That gap is why this module exists.

## Signals

`moq_media::net::NetworkSignals` is the transport-agnostic input:

```rust
pub struct NetworkSignals {
    pub rtt: Duration,
    pub loss_rate: f64,          // 0.0..=1.0
    pub available_bps: u64,      // cwnd * 8 / rtt
    pub congestion_events: u64,  // monotonic counter
}
```

moq-media does not depend on iroh, so it never produces these. `iroh-live`'s
`util::spawn_signal_producer` polls the selected path's stats every 200 ms and
publishes into a `watch::Receiver<NetworkSignals>`, which `Live::subscribe` wires
onto the `Subscription`. A caller using moq-media without iroh either supplies
its own signals or leaves the track on whichever rendition it opened.

## Ranking

`rank_renditions` reads `coded_width` and `coded_height` off each catalog entry
and sorts by pixel count descending, so index 0 is the highest quality. The sort
is stable, so renditions with equal pixel counts keep catalog order.

`CatalogSnapshot::best_video()` is the first entry of that ranking, which is what
`RemoteBroadcast::video()` opens.

## The decision

`evaluate` runs on every tick and returns one of four decisions. The checks run
in this order.

An **emergency** drop fires when `loss_rate` reaches `loss_emergency` (0.20) and
the track is not already on the lowest rendition. There is no hold timer: the
task jumps straight to the lowest rendition.

A **downgrade** fires one rung at a time when either the current rendition's
bitrate exceeds `bw_downgrade_ratio` (0.85) of `available_bps`, or `loss_rate`
reaches `loss_downgrade` (0.10). Both conditions have to persist for
`downgrade_hold` (500 ms). The 85% ceiling leaves headroom for protocol overhead
and short spikes.

An **upgrade** needs `available_bps` to exceed the next higher rendition's
bitrate by `bw_probe_headroom` (1.2x) and `loss_rate` to stay at or below
`loss_good` (0.02), sustained for `upgrade_hold` (4 s). It is also blocked for
`post_downgrade_cooldown` (4 s) after any downgrade.

Everything else **holds**. A track already on the highest rendition holds and
resets its good-conditions timer.

The asymmetry between a 500 ms downgrade hold and a 4 s upgrade hold is what
keeps the ladder from oscillating: quality drops quickly when the link
deteriorates and rises only on sustained evidence that it recovered.

## What the probe machinery does not do

`evaluate` returns `Decision::StartProbe` for an upgrade, and `AdaptiveConfig`
carries `probe_duration`, `probe_cooldown`, and `loss_probe_abort` alongside a
`should_abort_probe` function. None of that is wired up. The adaptation task
treats `StartProbe` exactly like a switch: it requests the higher rendition and
never revisits the decision. Nothing sets `AdaptationTimers::last_probe` or
`probe_congestion_baseline`, so the two probe cooldowns never trigger and
`should_abort_probe` is exercised only by unit tests.

An upgrade is therefore committed rather than trialled. If the higher rendition
turns out to be too much for the link, the ordinary downgrade path brings it back
down 500 ms later. Turning `StartProbe` into a real probe, where the replacement
runs for `probe_duration` and is abandoned on loss, is the work this
configuration was written for and has not had.

`adaptive::RenditionMode` is also unused. Pinning a rendition is
`VideoTrack::set_rendition` plus `disable_adaptation`.

## Configuration

`AdaptiveConfig` collects every threshold and timer.

| Field | Default | Meaning |
|---|---|---|
| `upgrade_hold` | 4 s | Sustained good conditions before upgrading |
| `downgrade_hold` | 500 ms | Sustained bad conditions before downgrading |
| `post_downgrade_cooldown` | 4 s | Quiet period after a downgrade |
| `loss_downgrade` | 0.10 | Loss rate that triggers a sustained downgrade |
| `loss_emergency` | 0.20 | Loss rate that triggers an immediate drop to lowest |
| `loss_good` | 0.02 | Loss rate below which conditions count as good |
| `bw_downgrade_ratio` | 0.85 | Share of estimated bandwidth a rendition may use |
| `bw_probe_headroom` | 1.2 | Bandwidth excess required before upgrading |
| `check_interval` | 200 ms | How often signals are evaluated |
| `probe_duration` | 3 s | Unused |
| `probe_cooldown` | 8 s | Unused |
| `loss_probe_abort` | 0.05 | Unused |

The defaults are tuned for a real link. `enable_adaptation_with` takes an
explicit config, which is how the end-to-end test sees a switch inside its own
timeout instead of waiting out a four-second upgrade hold.

## Requesting a switch by hand

The adaptation task and the application share one request channel, so
`VideoTrack::set_rendition(name)` works whether or not adaptation is running. The
task holds off while a requested switch has not yet landed, so it does not
re-request the same change on every tick. It also holds when the catalog carries
fewer than two renditions, and keeps ticking rather than returning, because a
publisher can add renditions mid-broadcast.
