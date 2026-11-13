# Player settings

`PlayerConfig` sets how a `Player` plays. Start from `Default`, and set a
field with its `with_*` builder:

```rust,ignore
let config = PlayerConfig::default()
    .with_audio(AudioMode::Play(engine))
    .with_latency(DEFAULT_LATENCY);
```

| Field, builder | Default | Meaning |
|---|---|---|
| `rendition`, `with_rendition` | `RenditionMode::Auto` | Which video rendition plays |
| `latency`, `with_latency` | `DEFAULT_LATENCY`, 100 ms to 150 ms | moq-net's `Delay { min, max }`: the least time each frame is held, and the lag past which the player skips to the live edge |
| `audio`, `with_audio` | `AudioMode::Off` | Whether audio is subscribed, decoded and played |
| `decoder`, `with_decoder` | `decode::Kind::Auto` | Which video decoder opens |
| `adaptation`, `with_adaptation` | see below | Thresholds and timers of automatic choice |
| `record_arrivals`, `with_recorded_arrivals` | `false` | Records each frame's arrival time, for the timeline. Opens a second subscription |

[Tune quality and latency](../how-to/tune.md) shows them in use.

## Rendition modes

| `RenditionMode` | Effect |
|---|---|
| `Auto` | The player picks a rendition from the link's measurements |
| `Pinned(name)` | Plays one rendition. While it cannot, the player plays as `Auto` would and says why in `PlayerStatus::switch_error` |
| `Off` | Unsubscribes video while audio keeps playing |

`Player::set_rendition` changes the mode of a running player.

## Latency

| Constant or `irl watch --latency` | `min` | `max` |
|---|---|---|
| `IMMEDIATE_LATENCY` | 0, no pacing | 150 ms |
| `realtime` | 60 ms | 100 ms |
| `DEFAULT_LATENCY`, `balanced` | 100 ms | 150 ms |
| `smooth` | 400 ms | 600 ms |

The playout clock holds each frame for at least `min`, and longer when the
catalog's `delay` or the measured audio jitter asks for more. `play` and
`set_latency` refuse a `min` above `max`. `Player::set_latency` changes a
running player without a gap. [Playout and A/V sync](../architecture/playout.md)
explains the clock.

## Audio modes

| `AudioMode` | Effect |
|---|---|
| `Off` | Does not subscribe to audio |
| `Decode` | Decodes the audio and discards it, or hands it to `Player::audio_frames` |
| `Play(engine)` | Plays through a `playback::Engine`. Needs the `playback` feature |

## Decoders

`decode::Kind::Auto` tries the platform's hardware decoders and falls back to
software. `Hardware` and `Software` restrict the choice, and
`Named("vaapi")` opens one backend only, with no fallback. `irl watch
--decoder` takes `auto`, `hardware`, `software` or a backend name.
`Player::set_decoder` changes a running player.

## Adaptation

`PlayerConfig::adaptation` is an `Adaptation`:

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

[Adaptive rendition switching](../architecture/adaptive.md) explains each one.

## Status and statistics

- `Player::status()` watches a `PlayerStatus`: the mode, the rendition on
  screen and the one warming up, the decoder, and `switch_error`.
- `Player::stats()` returns a `PlaybackStats` snapshot: frame rate, decode
  time, frames shown and skipped, audio buffer, playout latency, and the last
  `NetworkSample` of the link.
- `Player::timeline()` returns the last few hundred frames of each medium,
  with when each left its decoder and when it was presented.

[Instrumentation and tests](../architecture/devtools.md) describes the stats
and the overlay that draws them.
