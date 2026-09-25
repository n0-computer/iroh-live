# Adaptive rendition switching

A publisher that offers several renditions lets each subscriber follow its own
downlink. Every `Player` in `RenditionMode::Auto` runs a selector task
(`iroh-live-media/src/player/select.rs`). It reads the broadcast's network
signals every 200 ms, computes which rendition should play, and hands that to
the video supervisor when it changes. The rule itself is in `player/bound.rs`.
The decoder swap is the supervisor's job, described in
[subscribing](subscribe.md#switching-renditions).

## Signals

`iroh_live_media::NetworkSignals` is the transport-agnostic input: a trait with
one method, `sample()`, that returns a `NetworkSample`. A closure that returns
a sample implements it.

```rust
pub struct NetworkSample {
    pub rtt: Option<Duration>,
    pub min_rtt: Option<Duration>,     // smallest recent rtt on this path
    pub loss: Option<f32>,             // 0.0..=1.0
    pub delivery: Option<Bitrate>,     // the publisher's estimate of the path
    pub path_generation: u64,          // bumped when the path changes
}
```

Every field is optional because transports measure different things, and
`None` means unmeasured. A transport attaches its signals with
`RemoteBroadcast::with_network`, and every player started from that broadcast
afterwards reads them. Without signals, the player holds the best rendition its
constraints allow.

iroh-live-media does not depend on iroh, so it never produces these itself.
Every `iroh-moq` link runs a connection monitor (`iroh-moq/src/link.rs`) that
reads its MoQ session's statistics every 200 ms and keeps the latest
`LinkSample`. A direct session starts the history over when the connection
selects another path, and a relay link on every reconnect. Both bump
`path_generation`. `Live::subscribe` and `Live::remote_broadcast` attach a
closure (`iroh-live/src/network.rs`) that reads `Subscription::link()` for
whichever link serves the subscription at that moment. It converts the sample
into a `NetworkSample` and counts a change of serving link as a new path too.

`delivery` is the one figure that describes capacity. The publisher sends it in
moq-net's PROBE control message: its own estimate of what the path to this
subscriber carries, refreshed every 100 ms. iroh-moq reads it as
`estimated_recv_rate` in moq-net's session statistics. A publisher on a MoQ
version without PROBE sends none, and the field reads `None`.

On iroh the estimate is the congestion window over the round trip. Under
CUBIC, iroh's default, an application-limited sender's window grows until
something is lost, so the estimate overstates the link. `MoqPreset`, which
`EndpointOptions` binds with, uses BBR3 instead, whose window follows the
delivery rate it measures.

Loss is the other figure the selector reads. A subscriber sends little but
acknowledgements, so its loss rate is loss among acknowledgements. It stands in
for path loss only as far as both directions are impaired alike. The rate is
measured over two seconds and reads zero when the window holds fewer than 20
packets, so one lost acknowledgement does not read as a large loss.

The selector ignores the round trip and its minimum. They are there for
diagnostics and for tests that check an impairment reached the transport.
`min_rtt` is the minimum over the last 15 seconds, so a baseline that moved
does not read as a queue that never drains.

## Ranking

`Catalog::ranked_video()` lists the renditions largest first by pixel count,
and between two of the same size by the higher advertised bitrate. The
selector walks that list from the top, and the first rendition that passes
every check wins.

The caller's constraints rule renditions out before any network reading
counts: a `max_height` from `RenditionMode::Auto` (a grid tile has no use for
1080p), a `stalled` flag the publisher set in the catalog, and any rendition
whose decoder failed in the last 10 seconds. When every rendition is ruled out,
the smallest still plays.

## The decision

Each tick computes which renditions are allowed, and the best allowed one is
the target. Two bounds apply.

**The delivery estimate caps the bitrate.** The selector takes the maximum of
the estimate over the last second, since the estimate moves with every
acknowledgement and one low reading does not mean a smaller link. A rendition
fits while that maximum covers 1.25 times its advertised bitrate. Encoders send
about 82% of their target (openh264 and VA-API on the patchbay picture), and an
iroh publisher's estimate reads a capped link at 0.8 to 1.6 times the cap. So a
rung fits while the maximum covers about 1.5 times what it actually sends. A
rendition without an advertised bitrate, or a publisher that sends no estimate,
always fits.

**Sustained loss lowers a ceiling.** Loss of 10% or more for 500 ms moves the
ceiling one rung below the rendition playing, and loss that lasts walks down
one rung per 500 ms. Loss of 20% or more is an emergency and drops the ceiling
to the lowest eligible rendition at once. Once loss falls below 10%, every 4 s
of it raises the ceiling one rung.

The same fit ratio applies to staying on a rendition and to stepping up to one.
Timers provide the asymmetry:

- A lower target has to hold for 500 ms before the switch. An emergency
  switches at once.
- A higher target has to hold for 4 s. Each step down from a rung multiplies
  the next hold before stepping up to it by four, up to 120 s. A step up that
  plays for 20 s clears the count.
- No step up is taken within 4 s of a step down.

The growing hold keeps a marginal link from oscillating. On a lower rung the
estimate is read while the link carries only that rung, so it says the upper
one fits even when the link just showed it does not. With the backoff, the rung
the link cannot carry is tried after 16 s, then after 64 s, then every two
minutes. Such a step up is in effect the probe, so there is no separate one.
`adaptation_follows_a_real_link` in the patchbay suite checks that the ladder
climbs back on a clear link. `a_risen_baseline_round_trip_does_not_downgrade`
checks that a longer path, such as a relay fallback, is not taken as a smaller
one.

An automatic step down from the rendition on screen does not overlap the two
tracks. The link cannot carry the rendition on screen, and while both share it
the lower rendition's groups age out before they arrive. The supervisor drops
the incumbent, and its last picture stays up until the lower rendition's first.

The holds are timed against the rendition the selector last asked for, not the
one on screen. A switch takes a decoder open and a keyframe to land, and timing
against the old rendition would restart the hold and ask for it back. The hold
also runs for "any lower" and "any higher" target, not one rendition, so a
target that wavers between two lower rungs still switches.

A rendition that left the catalog or is no longer eligible is left at once. A
new `path_generation` resets everything learned so far: the estimate history,
the loss ceiling and every timer.

## Configuration

The thresholds and timers are `PlayerConfig::adaptation`, an `Adaptation`
value. Tests shorten the timers there.

| Field | Default | Meaning |
|---|---|---|
| `fit_ratio` | 1.25 | Multiple of a rung's advertised bitrate the estimate has to cover |
| `estimate_window` | 1 s | Span of the sliding maximum over the estimate |
| `loss_step_down` | 0.10 | Loss that lowers the ceiling one rung, once held |
| `loss_emergency` | 0.20 | Loss that drops the ceiling to the lowest rung at once |
| `downgrade_hold` | 500 ms | How long a lower target, or step-down loss, has to hold |
| `upgrade_hold` | 4 s | How long a higher target has to hold, and how long loss has to stay clear before the ceiling rises |
| `post_downgrade_cooldown` | 4 s | Quiet period after a step down |
| `trial` | 20 s | How long a step up has to play before it clears its rung's step downs |
| `upgrade_hold_max` | 120 s | The longest hold before a step up |
| `tick` | 200 ms | How often the network is read |

`adaptive_rendition_switching` in `iroh-live/tests/e2e.rs` drives a player
with its own closure as the network signals, and feeds it 25% loss, an
emergency that switches without a hold.

## Logs

The link monitor logs every sample at TRACE as `link sample`, with the path
generation, whether the path is relayed, the round trip and its minimum, the
loss rate, goodput and the publisher's estimate. The selector logs the target
it asks for at TRACE when it changes, and a rendition it backs off from after a
decoder failure at INFO. The bound logs a change of path and a step down at
DEBUG. Together they tell a loop that holds because the link is fine from one
that holds because it has nothing to judge against.

## Choosing by hand

`Player::set_rendition` takes a `RenditionMode`. `Auto { max_height }` is the
selector described here. `Pinned(name)` holds one rendition. If it cannot be
played, because it is not in the catalog or its decoder failed, the player
plays as `Auto` would, says why in `PlayerStatus::switch_error`, and returns to
the pinned rendition when it becomes available. `Off` unsubscribes video while
audio keeps playing, for a tile scrolled off screen.

`Player::wait_for_rendition(name)` waits until a rendition is on screen. The
selector keeps running while the catalog has a single rendition, since a
publisher can add renditions mid-broadcast.
