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
    pub rtt_samples: u64,          // distinct rtt readings so far
    pub min_rtt: Duration,         // smallest rtt over a recent window on this path
    pub loss_rate: f64,            // 0.0..=1.0
    pub goodput_bps: Option<u64>,  // received bytes over the last second
    pub delivery_bps: Option<u64>, // the publisher's estimate of the path
    pub congestion_events: u64,    // monotonic counter
}
```

moq-media does not depend on iroh, so it never produces these. `iroh-live`'s
`util::spawn_signal_producer` polls the selected path's stats every 200 ms,
reads the session's bandwidth consumer, and publishes into a
`watch::Receiver<NetworkSignals>`, which `Live::subscribe` wires onto the
`Subscription`. A caller using moq-media without iroh either supplies its own
signals or leaves the track on whichever rendition it opened.

`delivery_bps` is the one figure that describes capacity. The publisher sends
it: moq-net's PROBE control message carries the sending side's own estimate of
what the path to this subscriber carries, refreshed every 100 ms from its
congestion controller, and the subscriber reads it as
`moq_net::Session::recv_bandwidth()`. It needs no baseline, which is what the
other signals all need and cannot always get. An older publisher, on a MoQ
version before PROBE, sends none, and the field reads `None`.

What the figure contains depends on the publisher's transport. On iroh it is
the congestion window over the round trip. Under CUBIC, iroh's default, that
is worthless on a publisher: the window of an application-limited sender grows
until something is lost, so it reads the window and not the link. iroh-live
therefore runs BBR3 on every endpoint (`util::transport_config`), whose window
is sized from the delivery rate it measures. In the patchbay lab a 100 kbit/s
cap then reads as 108 to 164 kbit/s, and a clear loopback path as tens of
Mbit/s. The residual over-read is BBR's gain above the bandwidth-delay
product, and the thresholds below sit under it. A publisher whose transport
reports the controller's own pacing rate, as `web-transport-quinn` does since
moq-dev/web-transport#385, sends a tighter figure through the same field.

Of the rest, only two say anything about the downlink. QUIC reports a congestion
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

It has a consequence worth knowing when reading a trace of the fallback. A
bottleneck that persists past the window becomes the new minimum, so `rtt`
stops standing clear of `min_rtt`, the path stops counting as queueing, and
the goodput arriving through the bottleneck starts being recorded as this
rung's healthy rate. Watched in the patchbay lab, a link capped for a minute
took `min_rtt` from 1 ms to 417 ms and the healthy reference down to the
capped rate. Nothing downgrades after that, which is correct in the relay case
the window exists for and wrong in the bottleneck case. Telling the two apart
is what `delivery_bps` does, and where it is present neither the round trip
nor the minimum is consulted.

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
`loss_downgrade` (0.10), or when the path does not carry the rendition. Either
has to persist for `downgrade_hold` (500 ms).

**Whether the path carries the rendition is the estimate's call where there is
one.** The rendition fits while `delivery_bps` covers
`delivery_downgrade_ratio` (0.5) of its advertised bitrate, and is left when it
does not. Half, because two errors stack in the same direction: the advertised
figure is a ceiling the encoder spends about 40% of, and the iroh publisher's
estimate over-reads the path by BBR's gain. No second reading is needed to
corroborate it, since the publisher refreshes it ten times a second rather than
handing out one figure until the next acknowledgement. The rest of this section
is the fallback, for a publisher that sends no estimate or a rendition that
advertises no bitrate.

**Starved** is measured against what this rung was seen delivering on a clear
path, not against what the catalog advertises. The loop keeps a peak goodput per
rendition, decaying with a `goodput_baseline_halflife` (30 s) half life and
updated only on ticks where the path is neither queueing nor losing, since a
reading taken through a queue would teach it that the capped rate is the normal
one. Goodput below `goodput_downgrade_ratio` (0.75) of that peak is a shortfall.

**Where nothing has been measured, the fallback downgrades nothing on
bandwidth.** A subscriber whose link was already congested when it joined
records no clear-path reading and never can, because the reading only counts
while the path is not queueing and the path does not stop queueing until
something downgrades. Loss still moves it, so what is stranded is specifically
the lossless bottleneck. The estimate has no such gap, which is the reason it
comes first: it is a level, not a change, and a subscriber that joins a link at
its limit reads the limit.

Measuring against the catalog's advertised bitrate instead was tried and
reverted. A catalog bitrate is a ceiling handed to the encoder rather than a
promise, and openh264 sends about a quarter to a half of what the rendition
declares as a matter of course, so a shortfall against it is true of a healthy
stream. Any path that looks queueing for an unrelated reason, a relay fallback
or a Wi-Fi to cellular handoff, then steps the ladder down for no reason. See
`plans/v2/260903-review.md` under D1.

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
any downgrade and `probe_cooldown` (8 s) after any probe. With an estimate the
step is taken outright, as `Decision::Upgrade`, once `delivery_bps` covers the
next rung's advertised bitrate by `delivery_upgrade_headroom` (1.5), and is not
taken at all while it does not: the good run does not accrue against a path the
publisher says is full. Without one there is deliberately no bandwidth
precondition: goodput cannot show room above the rate already flowing, and the
advertised bitrate is not a figure to measure against. Discovering the room is
then the probe's job.

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

Without an estimate, an upgrade is a bet that the link carries more than it is
being asked for, and since no measurement a receiver can take settles that
question, the bet is placed and then watched. `evaluate` returns `Decision::StartProbe`, the
adaptation task requests the higher rendition, and once the switch lands it
anchors a window of `probe_duration` (3 s) and a congestion baseline at that
moment rather than at the request, so congestion on the rung it had not left yet
is not charged to the probe. `Probe::abort` ends it early on `loss_rate`
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
| `queueing_samples` | 2 | Distinct round trip readings that corroborate a queue |
| `rtt_queueing_ratio` | 2.0 | Multiple of `min_rtt` that counts as a queue |
| `rtt_queueing_floor` | 25 ms | Excess over `min_rtt` below which nothing counts as a queue |
| `check_interval` | 200 ms | How often signals are evaluated |
| `delivery_downgrade_ratio` | 0.5 | Share of the rung's advertised bitrate the estimate must cover |
| `delivery_upgrade_headroom` | 1.5 | Multiple of the next rung's bitrate the estimate must reach |
| `probe_duration` | 3 s | How long an upgrade runs before it is kept |
| `probe_settle` | 1500 ms | How long a probe's rendition plays before its goodput is judged |
| `probe_cooldown` | 8 s | Quiet period after a probe |
| `loss_probe_abort` | 0.05 | Loss rate that abandons a probe |

The defaults are tuned for a real link. `enable_adaptation_with` takes an
explicit config, which is how the end-to-end test sees a switch inside its own
timeout instead of waiting out a four-second upgrade hold.

Every tick logs at TRACE under the `adapt` span: the round trip and its minimum,
the goodput, the publisher's estimate, the reference the fallback compares
against and the decision. The
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
