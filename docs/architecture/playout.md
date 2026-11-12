# Playout and A/V sync

Audio and video decode separately, on their own tasks, from their own tracks.
Each `Player` owns a `PlayoutClock` (`moq-ext-player/src/player/clock.rs`)
that keeps them aligned at playout time. It wraps upstream's
`moq_mux::sync`, the clock the moq players share.

## The algorithm

Each media path folds its arrivals into the clock as a track. The clock's
reference is the earliest arrival either track has seen, measured against its
timestamp, so it sits on the wall clock and follows the live edge. Playback
trails that reference by a delay, the player's latency, 100 ms by default.
Until the first frame anchors the clock, every frame is due at once.

Neither path owns the clock. A path that falls behind, such as audio stalled
by congestion, only arrives late: it never holds the pictures back or moves
their schedule. Audio converges on the clock by stretching toward the time its
sound is due instead.

## Who calls what

The video task records each decoded frame with `received(timestamp)`, holds it
until `wait_async(timestamp)` returns, and then puts it in the `VideoFrames`
slot the renderer reads. The hold is a future the supervisor keeps across loop
iterations, so a rendition switch requested while a picture is held is acted
on at once. The wait is recomputed whenever the anchor moves, and returns
`false` once the clock closed. `restart()` starts the video timeline over when
the broadcast moves to a new route, since a publisher behind the new route may
have restarted its timestamps.

Audio writes decoded frames straight to its output sink, and the sink's buffer
absorbs jitter. Each decoded frame is recorded through an `AudioLatency`
guard, taken from `PlayoutClock::register_audio` when the track opens. After
every write the guard says when the end of the written sound is due, and the
speaker stretches toward that time, skipping ahead rather than speeding up when
the reference jumps earlier, and dropping what trails by more than two seconds.
Dropping the guard releases the audio track, so video alone holds the clock.

Each player has its own clock. Two players of one broadcast, say a thumbnail
and a full-screen view, do not hold back each other's frames. Dropping the
player closes its clock and wakes everything waiting on it.

## Latency

`PlayerConfig::latency` is moq-net's subscription `Delay { min, max }`. An
unset `min` counts as zero, and an unset `max` waits as long as the publisher
keeps media.

`min` sets the clock's delay above. A publisher that advertises a larger
`jitter` in its catalog, the most it flushes a frame late, raises the delay to
the ladder's largest jitter, but never past `max`. A change of the delay moves
the schedule once, by the difference. `irl watch --latency` offers
`realtime`, `balanced` and `smooth`, at 60, 100 and 400 ms.
`iroh-live/tests/latency.rs` measures capture-to-decode latency with publisher
and subscriber in one process, once with `IMMEDIATE_LATENCY` and once with the
default, and prints the figures.

The whole `Delay` goes on
`moq_video::decode::Options` and `moq_audio::decode::Options`, so relays and
the publisher see the range too. Upstream skips media older than `max` to
return to the live edge. The default is 150 ms. Raise it when continuity through congestion
matters more than staying live, and lower it to skip stalls instead of playing
them out. `play` and `set_latency` refuse a `min` above `max`, since a skip
threshold below the hold would drop the frames the hold waits for.

A `min` of zero turns pacing off, and frames go to the renderer as they decode.
`IMMEDIATE_LATENCY` is that, with a `max` of 150 ms so the player still skips
to the live edge. It suits a test or a consumer with its own cadence, not live
playback with audio.

```rust
use std::time::Duration;
use iroh_live::media::{Delay, PlayerConfig, video::decode};

let config = PlayerConfig {
    latency: Delay {
        min: Some(Duration::from_millis(400)),
        max: Some(Duration::from_millis(600)),
    },
    decoder: decode::Kind::Software,
    ..Default::default()
};
```

`decoder` becomes the `kind` on `moq_video::decode::Options`. `Auto` tries the
platform's hardware decoders and falls back to software. A named backend is the
only one tried, so a machine without it fails to open instead of falling back.
That helps tell a driver problem from a stream problem.

A changed latency or decoder applies to a running player without a gap:
`set_latency` and `set_decoder` build a replacement video decoder behind the
picture, which takes over once it has caught up. A changed latency also moves
the playout clock to the new delay, so the schedule moves once. An audio
track keeps its range until it next reopens.

## Timing metrics

`Player::stats()` returns a `PlaybackStats` snapshot. `latency` is the clock's
delay. `audio.buffered` is
the sink's fill level alone. `video.decode_time` is one transport read and
decode together, smoothed. `video.fps` counts frames over a window instead of
deriving a rate from the gap between two frames. See [developer
tools](devtools.md) for the overlay that draws them.
