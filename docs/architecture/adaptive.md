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
    pub min_rtt: Duration,        // smallest rtt over a recent window on this path
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

`min_rtt` is a windowed minimum, not the smallest reading ever taken. The
producer keeps two 15-second buckets and reports the smaller, so a baseline is
forgotten between 15 and 30 seconds after the path stops producing it. That is
deliberate, and it is what keeps a connection that fell back to a relay from
reading as a queue that will never drain, since the new path is longer rather
than congested.

It has a consequence worth knowing when reading a trace. A bottleneck that
persists past the window becomes the new minimum, so `rtt` stops standing
clear of `min_rtt`, the path stops counting as queueing, and the goodput
arriving through the bottleneck starts being recorded as this rung's healthy
rate. Watched in the patchbay lab, a link capped for a minute took `min_rtt`
from 1 ms to 417 ms and the healthy reference down to the capped rate. Nothing
downgrades after that, which is correct in the relay case the window exists for
and wrong in the bottleneck case, and telling those two apart from a subscriber
needs something the transport does not currently report.

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
`loss_downgrade` (0.10), or when the rendition is starved on a queueing path.
Either has to persist for `downgrade_hold` (500 ms).

**Starved** is measured against what this rung was seen delivering on a clear
path, not against what the catalog advertises. The loop keeps a peak goodput per
rendition, decaying with a `goodput_baseline_halflife` (30 s) half life and
updated only on ticks where the path is neither queueing nor losing, since a
reading taken through a queue would teach it that the capped rate is the normal
one. Goodput below `goodput_downgrade_ratio` (0.75) of that peak is a shortfall.

Where nothing has been measured yet, the catalog's advertised bitrate is the
fallback, at the wider `advertised_downgrade_ratio` (0.5). That case is a
subscriber whose link was already bad when it joined: it never records a
clear-path reading, and without a fallback it never can, because the reading
only counts while the path is not queueing and the path does not stop queueing
until something downgrades. The margin is wider because a catalog bitrate is a
ceiling handed to the encoder rather than a promise, and an easy scene comes in
well under it.

**Queueing** is `rtt` at `rtt_queueing_ratio` (2x) of `min_rtt` and at least
`rtt_queueing_floor` (25 ms) above it, corroborated by `queueing_samples` (2)
*distinct* round trip readings. Elapsed time is no evidence for a queue: QUIC
hands out the same reading until it takes another sample, so a hold shorter than
the gap between samples is satisfied by a single scheduler hiccup on a path with
a one-millisecond baseline.

The two halves need each other. Taken on its own a goodput shortfall is as
easily a scene that got easier to encode, and a queue on its own is as easily a
path that simply got longer. High loss stands alone, because it is recounted
from fresh packet totals on every tick.

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
| `goodput_downgrade_ratio` | 0.75 | Share of the measured clear-path peak that must still arrive |
| `goodput_baseline_halflife` | 30 s | Half life of that peak |
| `advertised_downgrade_ratio` | 0.5 | Share of the catalog bitrate, used only while nothing has been measured |
| `queueing_samples` | 2 | Distinct round trip readings that corroborate a queue |
| `rtt_queueing_ratio` | 2.0 | Multiple of `min_rtt` that counts as a queue |
| `rtt_queueing_floor` | 25 ms | Excess over `min_rtt` below which nothing counts as a queue |
| `check_interval` | 200 ms | How often signals are evaluated |
| `probe_duration` | 3 s | How long an upgrade runs before it is kept |
| `probe_settle` | 1500 ms | How long a probe's rendition plays before its goodput is judged |
| `probe_cooldown` | 8 s | Quiet period after a probe |
| `loss_probe_abort` | 0.05 | Loss rate that abandons a probe |

The defaults are tuned for a real link. `enable_adaptation_with` takes an
explicit config, which is how the end-to-end test sees a switch inside its own
timeout instead of waiting out a four-second upgrade hold.

Every tick logs at TRACE under the `adapt` span: the round trip and its minimum,
the goodput, the reference it is compared against and the decision. The
interesting case is the one where nothing happens, because a loop that holds
because the link is fine and a loop that holds because it has no reference to
judge against look identical from outside and need different fixes.

## Requesting a switch by hand

The adaptation task and the application share one request channel, so
`VideoTrack::set_rendition(name)` works whether or not adaptation is running. The
task holds off while a requested switch has not yet landed, so it does not
re-request the same change on every tick. It also holds when the catalog carries
fewer than two renditions, and keeps ticking rather than returning, because a
publisher can add renditions mid-broadcast.
