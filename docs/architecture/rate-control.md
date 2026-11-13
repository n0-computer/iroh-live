# Rate control

A publisher with rate control lowers its encoders' bitrate when its links
carry less, and raises it again when they carry more, without restarting an
encoder or forcing a keyframe. A ladder pauses its upper renditions before it
starves the lowest one, and resumes them when the links allow.

It is off by default. `NodeConfig::rate_control` is `None`, every link reports
the congestion window over the round trip as its send-rate estimate, and every
encoder runs at its configured bitrate, as before rate control existed.

```rust
use iroh_live::{RateControl, layout::LiveLayout};

let config = LiveLayout::default().config().with_rate_control(RateControl::default());
```

## How it fits together

Everything above the transport is upstream moq:

- `moq_net::bandwidth::Allocator` divides one estimate among the tracks that
  reserve on it, by priority and max-min fair within a priority.
- `moq_mux::rate::Grant` turns a track's slice into encoder targets: a drop
  applies at once, a rise ramps at 25% a second, moves under 5% are ignored,
  and the floor is a tenth of the configured bitrate.
- A rendition above a lower one of the ladder disables below the band boundary
  `(max + 2 * lower) / 3`, but only while a session probes for it.
- `moq_video::encode::Sink::set_bitrate` retunes the running encoder.

What iroh-live adds:

| Piece | Crate | What it does |
|---|---|---|
| `rate::SendEstimate`, `RateConfig` | `moq-transport-iroh` | Where a transport's estimate comes from. `Transport::with_rate` takes it |
| The estimator | `moq-transport-iroh` | Reads the selected path's statistics and turns queueing and loss into an estimate |
| `RateControl`, `FanOut` | `iroh-live-node` | The node's switch, and how one encoder follows several links |
| `Node::bandwidth` | `iroh-live-node` | The allocator every publication of the node reserves on |
| `LocalBroadcast::set_bandwidth` | `moq-ext-publisher` | Reserves each rendition and follows its grant |

`LiveLayout::publish` and `Calls::new` hand the node's allocator to a
`LocalBroadcast` on their own. A broadcast built through `Node::publish_with`
gets it with `broadcast.set_bandwidth(node.bandwidth()?)`.

## The estimate

`SendEstimate::Window` is the estimate every transport reports by default. Under
BBR3 the window carries BBR3's gain over the delivery rate, so it reads up to
1.6 times what a capped link carries, and it never falls while the sender is
limited by its media.

With rate control the transport runs an estimator over the selected path's
`PathStats`, read at most every 50 ms:

- Two readings in a row whose smoothed round trip sits more than
  `clamp(min_rtt / 2, 15 ms, 60 ms)` above its ten-second minimum are a queue.
  The estimate falls to 0.85 times the delivered rate, bytes sent less bytes
  lost. It falls again two seconds on only if the queue has not shrunk and it
  grows, or the sender has not backed off, or the estimate is above what the
  path delivers.
- A queue drains once the sender backs off, and a longer path does not. Take
  a round trip that stays flat for a second while the sender sends 10% less
  than at the fall, with loss under 2%. It is a longer path: it becomes the
  baseline, and the fall is undone.
- Loss above 10% lowers it by half the loss rate. The rate spans two seconds,
  or the time since the last fall if that is shorter, so one burst lowers it
  once. It never falls under 50 kbit/s.
- While the path stays clean and the hold after a fall is over, it climbs up
  to 1.5 times the session's probe target. Between the rate the path last
  congested at and 1.25 times that rate, it climbs 10% a second. Below and
  above that band it climbs 25% a second. Above the band the climb speeds up by 25 points for
  each second it goes on, up to 70% a second. That about doubles the estimate
  each second. The hold starts at one second. A fall that ends a climb within
  ten seconds of the last fall doubles it, up to four.
- A change of the selected path starts the baseline over, and the estimate
  holds for a settle window, two seconds by default.

Until the path first shows congestion, `SendEstimate::App` reports the window
estimate, and `SendEstimate::Controller`, the default, reports the controller's
own estimate, `PathStats::bandwidth_estimate`, which for BBR3 is its bandwidth
model. From then on `App` reports its own estimate, and `Controller` reports the
smaller of the two. noq 1.4's BBR3 never lowers its model while the sender is
limited by its media, so the estimator is what brings `Controller` down.

The estimate reaches the encoders and the peer alike: the session divides it
among the reservations, and sends it to the peer as its PROBE report, which the
peer's players read as `delivery`.

## Probing

There is no padding. A transport with an estimator returns a probe target from
`poll::Session::probe`, so the session hands it the sum of the reservations,
and the reservations count as probed. The estimator's climb is the probe: the
encoders follow the estimate up, and the next queue brings it back. That is
what lets a paused rendition resume, and a player see room for a higher one.

The cost is that each resume of a rendition the path cannot carry builds a
short queue before the estimate falls again, at most once per hold.

## Fan-out

A node serves each publication on every link whose peer may see it. One
encoder per rendition has to pick one rate. `FanOut::Shared`, the only policy,
treats the node's uplink as one pipe that every copy crosses: the estimate is
the mean of the estimates of the links that carry media. With one link it is
that link's.

The node shares one allocator registry with every session, dialed and
accepted, so each takes the sum of the node's reservations as its probe target.
Sharing it with accepted sessions needs moq-net's `Server::with_allocator`,
which upstream does not have yet: the workspace points moq at a local branch
that adds it.

## Encoders

- Each rendition opens at its configured bitrate and only ever lowers it from
  there, which openh264 needs: it refuses a bitrate above the one it opened
  with.
- A backend that cannot retune keeps its bitrate, and its rendition still
  pauses and resumes.
- A paused rendition drains its encoder, marks its catalog entry disabled, and
  closes the encoder. It keeps following its grant without a viewer, since
  players skip a disabled rendition. On resume it marks the entry enabled, and
  the next viewer opens a fresh encoder, whose first frame is a keyframe.
- Audio reserves its bitrate on the same allocator, and keeps it.
- A pre-encoded source, such as `rpicam-vid`, declares no bitrate, so it
  reserves nothing and keeps its rate.
- `EncodeStats::grant` and `EncodeStats::target` show each rendition's slice
  and the bitrate its encoder runs at, and `RenditionState::Paused` a paused
  rendition.

## Limitations

- No padding probes. Recovery after a drop takes the climb's time: about 12 to
  14 s from a 250 kbit/s cap back to a 1.5 Mbit/s ceiling in the patchbay
  suite.
- `FanOut::Shared` only. A peer behind a slow downlink pulls the mean down only
  in part, and its own player has to step down a rendition for it.
- Relay links over `moq_tokio` keep their own estimate and do not count toward
  the node's. iroh-live-relay does not run the estimator toward its viewers.
- The round trip is the smoothed one, so the estimator cannot tell a route that
  got longer on the same QUIC path from a queue at first. It falls once, and
  undoes the fall about two seconds later. A rendition whose share falls under
  its boundary in that time pauses, and a player leaves it. In the patchbay
  suite the top rung sends enough that the fall stays above its boundary. An
  iroh path change, such as a fall back to a relay, is a new path and holds
  through the settle window instead.
- A queue that the backed-off sender holds exactly flat looks like a longer
  path too. The estimate climbs back into it, and the next fall takes the
  queue out of the baseline again.
- The subscriber's fit test still asks for 1.25 times a rendition's bitrate.
  The climb's ceiling, 1.5 times the probe target, leaves room for it, but a
  player does not yet read the publisher's band boundary.

The patchbay scenarios `the_encoder_follows_a_cap_without_stalling`,
`the_encoder_recovers_when_the_cap_lifts` and
`a_paused_rendition_resumes_when_the_cap_lifts` cover the three behaviours.
`IROH_LIVE_PATCHBAY_RATE=app` or `=controller` runs every patchbay scenario with
the publisher's rate control on.
