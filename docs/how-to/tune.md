# Tune quality and latency

Quality and latency are set on both sides. The publisher decides which
renditions exist and how they are encoded. Each player decides how much to
buffer and which rendition to play.

## Choose a starting point

| Use | Publisher | Player |
|---|---|---|
| A call or a live demo | `--keyframe-interval 1` | `--latency realtime` |
| Watching on a good link | the defaults | the defaults |
| Viewers on Wi-Fi or mobile links | a ladder, such as `--renditions 360p,720p` | `--latency smooth` |
| A slow device, such as a Pi Zero | one small rendition, such as `--renditions 360p` | a hardware decoder |

## On the publisher

`irl publish` takes these flags, and the [CLI reference](../reference/cli.md#flags)
has the details:

- `--renditions <LIST>` sets the ladder. A rung is `<height>p`,
  `<width>x<height>` or `<name>:<width>x<height>`, with an optional
  `@<fps>`. A ladder lets each viewer drop to a smaller rung when its link is
  slow, and an unwatched rung costs nothing.
- `--bitrate <BITS_PER_SECOND>` sets the bitrate of the largest rung, and the
  smaller rungs get a share by pixel count. Without it, the encoder derives a
  bitrate from the size and frame rate.
- `--keyframe-interval <SECONDS>` (default 2) sets how long a new viewer waits
  for its first picture, and how long a rendition switch takes. A shorter
  interval costs bitrate.
- `--codec h265` and `--encoder` pick the codec and backend. H.265 needs a
  hardware encoder.

From Rust, each `VideoRendition` carries moq-video's encoder config:

```rust,no_run
use std::time::Duration;

use iroh_live::media::{Rate, VideoEncoding, VideoRendition, video};

fn ladder() -> VideoEncoding {
    let mut high = VideoRendition::p720();
    high.config.bitrate = Some(Rate::from_kbps(2_500));
    // A keyframe every second: faster joins and switches.
    high.config.join = Duration::from_secs(1);

    let mut low = VideoRendition::new("low");
    low.size = Some(video::Size::new(320, 180));
    low.config.join = Duration::from_secs(1);

    VideoEncoding::ladder([low, high])
}
```

`VideoRendition::p360()` is 640x360 at 500 kbit/s, and `p720()` is 1280x720
at 1.8 Mbit/s. A rendition's frame rate follows the source unless `rate` is
set.

Each rung's bitrate is a ceiling. With
[rate control](../architecture/rate-control.md), which is off by default, the
encoders follow what the links carry: they lower the bitrate when a link
slows, and pause the upper rungs before they starve the lowest one.

## On the player

The player holds each frame for a short time before it shows it, so it can
absorb jitter and keep audio and video in step. `--latency` sets how long,
and how far behind it may fall before it skips ahead to the live edge:

| `irl watch --latency` | Holds | Skips at | For |
|---|---|---|---|
| `realtime` | 60 ms | 100 ms | Conversations |
| `balanced` | 100 ms | 150 ms | The default |
| `smooth` | 400 ms | 600 ms | Wi-Fi or mobile links |

The hold rises above the minimum when the catalog or the measured audio
jitter asks for more, and returns to it on a clean link.

From Rust, `PlayerConfig` takes the same settings through its `with_*`
builders, and setters change a running player without a gap:

```rust,no_run
use std::time::Duration;

use iroh_live::media::{Delay, Player, PlayerConfig, RenditionMode, video::decode};

fn smooth() -> PlayerConfig {
    PlayerConfig::default().with_latency(Delay {
        min: Some(Duration::from_millis(400)),
        max: Some(Duration::from_millis(600)),
    })
}

fn pin(player: &Player) {
    // Plays only `720p`, and falls back to automatic choice while it cannot.
    player.set_rendition(RenditionMode::pinned("720p"));
    player.set_decoder(decode::Kind::Software);
}
```

- `irl watch --rendition <NAME>` and `RenditionMode::Pinned` play one
  rendition. `RenditionMode::Off` stops video while audio plays, for a tile
  off screen.
- `irl watch --decoder` and `PlayerConfig::decoder` pick a decoder. On a
  Raspberry Pi 4, the software decoder has less latency than the hardware one.
- `PlayerConfig::adaptation` holds the thresholds and timers of automatic
  choice. The [player reference](../reference/player.md) lists them.

## See what happens

The stats overlay of `irl watch` shows the rendition, the frame rate, the
decode time, the latency, and the link's round trip and loss. `Player::stats()`
returns the same values. To see why the player switched, turn on its logs,
as [Environment variables](../reference/environment.md#rust_log) shows.

## Where to go next

- [Adaptive rendition switching](../architecture/adaptive.md) explains how a
  player picks a rendition.
- [Playout and A/V sync](../architecture/playout.md) explains the hold.
- [Rate control](../architecture/rate-control.md) explains how a publisher's
  encoders follow its links.
