# Adaptive rendition switching

A publisher that offers several renditions lets a subscriber follow its own
downlink. Every `Player` in `RenditionMode::Auto` runs a selector task that
reads the broadcast's network signals every 200 ms, computes which rendition
should play, and hands that to the video supervisor whenever it changes. The
decoder swap itself is the supervisor's job, described in
[subscribing](subscribe.md).

`moq_mux::select` fixes a rendition at construction and `moq_video::encode::rate`
backs the sender's bitrate off, so neither covers a subscriber choosing for
itself. That gap is why the selector exists. It lives in
`iroh-live-media/src/player/select.rs`, and the rule it applies in
`player/bound.rs`.

## Signals

`iroh_live_media::NetworkSignals` is the transport-agnostic input: a trait with
one method, `sample()`, which returns a `NetworkSample`. A closure returning a
sample implements it.

```rust
pub struct NetworkSample {
    pub rtt: Option<Duration>,
    pub min_rtt: Option<Duration>,     // smallest recent rtt on this path
    pub loss: Option<f32>,             // 0.0..=1.0
    pub delivery: Option<Bitrate>,     // the publisher's estimate of the path
    pub path_generation: u64,          // bumped when the path changes
}
```

Every field is optional because every transport measures a different subset,
and a field left `None` reads as unmeasured rather than as zero. A transport
attaches its signals with `RemoteBroadcast::with_network`, and every player
started from that broadcast afterwards reads them.

iroh-live-media does not depend on iroh, so it never produces these.
`iroh-live`'s signal producer (`util::spawn_signal_producer`) polls the
selected path's stats every 200 ms, reads the session's bandwidth consumer, and
publishes a `util::LinkSignals` into a `watch::Receiver`. `Live::subscribe`,
`Call`, and `Subscription::new` attach it to the broadcast as
`LinkSignals::sample()`, so a caller does not wire anything. A caller using
iroh-live-media without iroh either attaches its own signals or none, in which
case the player holds the best rendition its constraints allow.

`delivery` is the one figure that describes capacity. The publisher sends it:
moq-net's PROBE control message carries the sending side's own estimate of what
the path to this subscriber carries, refreshed every 100 ms from its congestion
controller, and the subscriber reads it as
`moq_net::Session::recv_bandwidth()`. It needs no baseline. An older publisher,
on a MoQ version before PROBE, sends none, and the field reads `None`.

What the figure contains depends on the publisher's transport. On iroh it is
the congestion window over the round trip. Under CUBIC, iroh's default, that
is worthless on a publisher: the window of an application-limited sender grows
until something is lost, so it reads the window and not the link. iroh-live
therefore runs BBR3 on every endpoint (`util::transport_config`), whose window
is sized from the delivery rate it measures. In the patchbay lab a 100 kbit/s
cap then reads as 108 to 164 kbit/s, and a clear loopback path as tens of
Mbit/s. The residual over-read is BBR's gain above the bandwidth-delay
product, and the fit ratio below sits under it. A publisher whose transport
reports the controller's own pacing rate, as `web-transport-quinn` does since
moq-dev/web-transport#385, sends a tighter figure through the same field.

Loss is the other figure the selector reads, and it has a caveat. QUIC reports
a congestion window, a loss count and a congestion counter for the direction an
endpoint sends in, and a subscriber sends little but acknowledgements: its loss
rate is loss among acknowledgements, useful as a proxy for path loss only as far
as both directions are impaired alike.

The loss rate is measured across a two-second window and reads zero when the
window holds fewer than twenty packets. A subscriber sends two to five packets
per 200 ms tick, so a per-tick ratio was a fraction with a denominator of
three: one lost acknowledgement read as a third of everything lost, and on a
Pi 4 over Wi-Fi that dropped the player to its lowest rung every few seconds
with nothing wrong with the picture. Twenty packets is what keeps a single loss
under the step-down threshold; below that count the rate is unmeasured rather
than clean, and reads as zero.

The round trip and its minimum ride along in the sample, and `LinkSignals`
also carries a receiver-side goodput and a congestion event counter, but the
selector reads none of them. They are there for diagnostics and for tests that
check an impairment reached the transport. `min_rtt` is a windowed minimum over
two 15-second buckets rather than the smallest reading ever taken, so a
connection that fell back to a relay does not read as a queue that never drains.

## Ranking

`Catalog::video()` lists the renditions largest first, by pixel count, and
between two of the same size by the higher advertised bitrate. The selector
walks that list from the top, so the first rendition that passes every check
wins.

Before any network reading counts, the caller's constraints rule renditions
out: a `max_height` from `RenditionMode::Auto` (a grid tile has no use for
1080p), a catalog `stalled` flag the publisher set, and any rendition whose
decoder recently failed. A failed rendition is left alone for 5 s the first
time, doubling to at most 60 s on repeated failures. When every rendition is
ruled out, the smallest still plays, so there is always an answer.

## The decision

Each tick computes which renditions are allowed, and the best allowed one is
the target. Two bounds apply.

**The delivery estimate caps the bitrate.** The estimate is taken as a sliding
maximum over the last second, because it is refreshed ten times a second and
moves with every acknowledgement, so a single low reading is not a smaller
link. A rendition fits while that maximum covers half of its advertised bitrate.
Half, because two errors stack in the same direction: the advertised figure is
a ceiling the encoder spends about 40% of (openh264, measured), and the iroh
publisher's estimate over-reads the path by BBR's gain. A rendition that
advertises no bitrate, or a publisher that sends no estimate, fits
unconditionally.

**Sustained loss lowers a ceiling.** Loss at or above 10% for 500 ms moves the
ceiling one rung below the rendition playing, and a loss that lasts walks the
ladder down one rung per 500 ms rather than all at once. Loss at or above 20%
is an emergency and drops the ceiling to the lowest eligible rendition at once.
Once loss falls below 10%, every 4 s of it raises the ceiling one rung, until
it is gone.

The same fit ratio applies for staying on a rendition and for stepping up to
one. The asymmetry lives in the timers instead:

- A lower target has to hold for 500 ms before the switch, except in an
  emergency, which switches at once.
- A higher target has to hold for 4 s.
- No step up is taken within 4 s of a step down.

The asymmetry between the 500 ms downgrade hold and the 4 s upgrade hold is what
keeps the ladder from oscillating: quality drops quickly when the link
deteriorates and rises only on sustained evidence that it recovered.

A rendition that left the catalog, or is no longer eligible at all, is left at
once, since there is nothing to wait for. A change of network path, which the
sample reports as a new `path_generation`, forgets everything learned so far:
the estimate history, the loss ceiling, and every timer. History from the old
path says nothing about the new one.

## Why there is no probe

The previous rule stepped up only once the estimate covered one and a half
times the next rung's advertised bitrate, and fell back to a timed probe of the
higher rung without an estimate. Parked on a low rung, a publisher sends only
that rung's bytes, so its estimate is application-limited at a few times that
rate and never reached the gate: `adaptation_follows_a_real_link` in the
patchbay suite sat at 380 to 490 kbit/s against a 1.2 Mbit/s gate for a full
minute with nothing wrong with the link. One ratio for both directions, with
the asymmetry moved into the timers, is what lets the ladder climb back on a
clear link, and it removes the need to probe.

The same suite's `a_risen_baseline_round_trip_does_not_downgrade` is why the
round trip plays no part: a path that got longer, a relay fallback or a Wi-Fi to
cellular handoff, is not a path that got smaller.

## Configuration

The thresholds and timers are an internal `Tuning` value, so they can be
retuned in a patch without an API change.

| Field | Default | Meaning |
|---|---|---|
| `fit_ratio` | 0.5 | Share of a rung's advertised bitrate the estimate has to cover |
| `estimate_window` | 1 s | Span of the sliding maximum over the estimate |
| `loss_step_down` | 0.10 | Loss that lowers the ceiling one rung, once held |
| `loss_emergency` | 0.20 | Loss that drops the ceiling to the lowest rung at once |
| `downgrade_hold` | 500 ms | How long a lower target, or step-down loss, has to hold |
| `upgrade_hold` | 4 s | How long a higher target has to hold, and how long loss has to stay clear before the ceiling rises |
| `post_downgrade_cooldown` | 4 s | Quiet period after a step down |

The end-to-end test in `iroh-live/tests/e2e.rs` drives a player with its own
closure as the network signals and feeds it a 25% loss reading, which is an
emergency and switches without waiting out a hold.

The signal producer logs every reading at TRACE as `network signals`, with the
round trip, its minimum, the loss rate, goodput, and the publisher's estimate.
The selector logs the rendition it asks for at TRACE whenever it changes, a
change of path at DEBUG, and a rendition it backs off from after a decoder
failure at INFO. Read together, they tell a loop that holds because the link is
fine from one that holds because it has nothing to judge against.

## Requesting a switch by hand

`Player::set_rendition` takes a `RenditionMode`. `Auto { max_height }` is the
selector described here. `Pinned(name)` holds one rendition; if it cannot be
played, because it is not in the catalog or its decoder failed, the player
plays as `Auto` would, says why in `PlayerStatus::switch_error`, and returns to
the pinned rendition when it becomes available. `Off` unsubscribes video while
audio keeps playing, which is what a tile scrolled off screen wants.

`Player::wait_for_rendition(name)` waits until a rendition is on screen, and
fails with a `SwitchError` once a switch to it was superseded, withdrawn or
failed, or the catalog shows no such rendition. The selector keeps ticking while
the catalog carries a single rendition, because a publisher can add renditions
mid-broadcast.
