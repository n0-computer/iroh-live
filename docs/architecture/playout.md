# Playout and A/V sync

Audio and video decode separately, on their own tasks, from their own tracks.
Each `Player` owns a `PlayoutClock` (`iroh-live-media/src/player/clock.rs`)
that keeps them aligned at playout time. It is a port of the moq/js player's
`js/watch/src/sync.ts` at commit `53fe78d8`, with the same data model and the
same arithmetic in `i64` milliseconds.

## The algorithm

The clock keeps one number, the *reference*: the earliest `wall_now - frame_pts`
it has seen. It only moves earlier. `restart()` clears it when the broadcast
moves to a new route, since a publisher behind the new route may have restarted
its timestamps.

A frame with timestamp `T` is due at wall time `reference + T + latency`, where
`latency` is `audio + jitter`. `jitter` is the player's `Latency::min`, 100 ms
by default. `audio` is how much sound is queued at the speaker.

`received(pts)` updates the reference, and `wait_async(pts)` sleeps until the
frame is due. It returns `false` if the clock closed. The wait is recomputed
whenever the reference, the jitter or the audio depth changes. Until the first
frame sets a reference, every frame is due at once.

## Who calls what

Only video calls the clock. The video task records each decoded frame with
`received`, holds it until `wait_async` returns, and then puts it in the
`VideoFrames` slot the renderer reads. The hold is a future the supervisor keeps
across loop iterations, so a rendition switch requested while a picture is held
is acted on at once.

Audio writes decoded frames straight to its output sink, and the sink's buffer
absorbs jitter. On every frame it reports the sink's buffered duration through
an `AudioLatency` guard, taken from `PlayoutClock::register_audio` when the
track opens. Video is held back by that figure. Without it, a video frame would
show while its audio still waits behind the sound already queued.

The guard is the only way to set the audio term, and dropping it clears the
term. An audio track that ends, fails or is dropped stops holding video back.

Each player has its own clock. Two players of one broadcast, say a thumbnail
and a full-screen view, do not hold back each other's frames. A player without
audio has no audio term. Dropping the player closes its clock and wakes
everything waiting on it.

## Latency

`PlayerConfig::latency` is a `Latency { min, max }`.

`min` is the jitter allowance above, and the largest delay the player adds on
its own. `Player::set_latency` applies it at once. `irl watch --latency` offers
`realtime`, `balanced` and `smooth`, at 60, 100 and 400 ms.
`iroh-live/tests/latency.rs` measures capture-to-decode latency with publisher
and subscriber in one process, once with `Latency::IMMEDIATE` and once with the
default, and prints the figures.

`max` becomes `max_age` on `moq_video::decode::Options` and
`moq_audio::decode::Options`, where upstream skips old media to return to the
live edge. The default is 150 ms. Raise it when continuity through congestion
matters more than staying live, and lower it to skip stalls instead of playing
them out. `play` and `set_latency` refuse a `min` above `max`, since a skip
threshold below the hold would drop the frames the hold waits for.

A `min` of zero turns pacing off, and frames go to the renderer as they decode.
`Latency::IMMEDIATE` is that, with a `max` of 150 ms so the player still skips
to the live edge. It suits a test or a consumer with its own cadence, not live
playback with audio.

```rust
use std::time::Duration;
use iroh_live_media::{Latency, PlayerConfig, video::decode};

let config = PlayerConfig {
    latency: Latency {
        min: Duration::from_millis(400),
        max: Duration::from_millis(600),
    },
    decoder: decode::Kind::Software,
    ..Default::default()
};
```

`decoder` becomes the `kind` on `moq_video::decode::Options`. `Auto` tries the
platform's hardware decoders and falls back to software. A named backend is the
only one tried, so a machine without it fails to open instead of falling back.
That helps tell a driver problem from a stream problem.

A changed `max` or decoder applies to a running player without a gap:
`set_latency` and `set_decoder` build a replacement video decoder behind the
picture, which takes over once it has caught up. An audio track keeps its `max`
until it next reopens.

## Timing metrics

`Player::stats()` returns a `PlaybackStats` snapshot. `latency` is the clock's
current total, the jitter allowance plus the queued audio. `audio.buffered` is
the sink's fill level alone. `video.decode_time` is one transport read and
decode together, smoothed. `video.fps` counts frames over a window instead of
deriving a rate from the gap between two frames. See [developer
tools](devtools.md) for the overlay that draws them.
