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
    pub min_rtt: Duration,        // smallest rtt seen on this path
    pub loss_rate: f64,           // 0.0..=1.0
    pub goodput_bps: Option<u64>, // received bytes over the last second
    pub congestion_events: u64,   // monotonic counter
}
```

moq-media does not depend on iroh, so it never produces these. `iroh-live`'s
`util::spawn_signal_producer` polls the selected path's stats every 200 ms and
publishes into a `watch::Receiver<NetworkSignals>`, which `Live::subscribe` wires
onto the `Subscription`. A caller using moq-media without iroh either supplies
its own signals or leaves the track on whichever rendition it opened.

Only two of these say anything about the downlink. QUIC reports a congestion
window, a loss count and a congestion counter for the direction an endpoint
sends in, and a subscriber sends little but acknowledgements: its window
measures how little it sends, and its loss rate is loss among acknowledgements,
useful as a proxy for path loss only as far as both directions are impaired
alike. `goodput_bps` comes from `udp_rx.bytes` over a one-second window and is
the honest receiver-side reading, and the round trip covers both directions
because the queue holding up the media holds up the replies behind it.

`goodput_bps` is goodput, not capacity. It is bounded by what the publisher
chose to send, so it is a lower bound on what the path could carry: enough to
show a rendition failing to arrive in full, never enough to show room above the
rate already flowing. It reads `None` while too little is arriving to measure,
so a publisher going quiet is an absence of evidence rather than a link that
collapsed.

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

A **downgrade** fires one rung at a time when `loss_rate` reaches
`loss_downgrade` (0.10), or when the rendition is starved on a queueing path:
`goodput_bps` below `bw_downgrade_ratio` (0.85) of the rendition's advertised
bitrate, while `rtt` stands at `rtt_queueing_ratio` (2x) of `min_rtt` and at
least `rtt_queueing_floor` (25 ms) above it. Either has to persist for
`downgrade_hold` (500 ms).

The two halves of that second condition need each other. A catalog bitrate is a
ceiling handed to the encoder rather than a promise, and an easy scene comes in
well under it: measured through the patchbay lab, openh264 sends about 40% of
what the rendition declares. Taken on its own, a goodput shortfall would
therefore read as a collapsed link on every efficient publisher and pin the
ladder to its bottom rung. A queue in front of the bottleneck is what
distinguishes the two, and it is the only thing a subscriber sees when a
downlink runs out of room without dropping anything.

An **upgrade** needs `loss_rate` at or below `loss_good` (0.02), sustained for
`upgrade_hold` (4 s), and is blocked for `post_downgrade_cooldown` (4 s) after
any downgrade and `probe_cooldown` (8 s) after any probe. There is deliberately
no bandwidth precondition: goodput cannot show room above the rate already
flowing, and the advertised bitrate is not a figure to measure against.
Discovering the room is the probe's job.

The upgrade is not gated on the round trip either. It is sampled far more
sparsely than the other signals, because QUIC takes a round-trip sample only
from a packet that asks to be acknowledged and a subscriber mostly sends
acknowledgements, which do not. A reading taken while the link was still bad can
outlive the impairment by tens of seconds, which is harmless in the downgrade
direction and would strand the ladder in the other.

Everything else **holds**. A track already on the highest rendition holds and
resets its good-conditions timer.

The asymmetry between a 500 ms downgrade hold and a 4 s upgrade hold is what
keeps the ladder from oscillating: quality drops quickly when the link
deteriorates and rises only on sustained evidence that it recovered.

## The probe

An upgrade is a bet that the link carries more than it is being asked for, and
since no measurement a receiver can take settles that question, the bet is
placed and then watched. `evaluate` returns `Decision::StartProbe`, the
adaptation task requests the higher rendition, and once the switch lands it
anchors a window of `probe_duration` (3 s) and a congestion baseline at that
moment rather than at the request, so congestion on the rung it had not left yet
is not charged to the probe. `should_abort_probe` ends it early on `loss_rate`
reaching `loss_probe_abort` (0.05) or on any new congestion event, which steps
back down and starts `probe_cooldown` (8 s). A step up that never lands, because
the replacement failed to open, is forgotten rather than judged.

`adaptive::RenditionMode` is unused. Pinning a rendition is
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
| `bw_downgrade_ratio` | 0.85 | Share of a rendition's bitrate that must still arrive |
| `rtt_queueing_ratio` | 2.0 | Multiple of `min_rtt` that counts as a queue |
| `rtt_queueing_floor` | 25 ms | Excess over `min_rtt` below which nothing counts as a queue |
| `check_interval` | 200 ms | How often signals are evaluated |
| `probe_duration` | 3 s | How long an upgrade runs before it is kept |
| `probe_cooldown` | 8 s | Quiet period after a probe |
| `loss_probe_abort` | 0.05 | Loss rate that abandons a probe |

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
