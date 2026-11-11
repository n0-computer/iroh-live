# Playout and A/V sync

Audio and video decode separately, on their own tasks, from their own tracks.
Each `Player` owns a `PlayoutClock` (`moq-ext-player/src/player/clock.rs`)
that keeps them aligned at playout time. It wraps upstream's
`moq_mux::timeline`, the clock the moq players share.

## The algorithm

The timeline maps media time onto the wall clock through one anchor, and holds
every result back by a delay, so playback trails the live edge by that delay.
The delay is the player's `Latency::min`, 100 ms by default.

While audio plays, the speaker owns the anchor. Each write reports the media
time it ends at and the sound queued ahead of the speaker after it. The anchor
is pinned so that a picture with the same timestamp shows when that sound
plays. Without audio, each video frame paces the anchor, and the delay alone
sets when it shows. Until the first frame anchors the clock, every frame is due
at once.

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
absorbs jitter. After every write it reports the write's end and the sink's
buffered duration through an `AudioLatency` guard, taken from
`PlayoutClock::register_audio` when the track opens. Dropping the guard tells
the clock that the speaker went quiet, and video takes the anchor back. A new
audio track takes it again on its first write.

Each player has its own clock. Two players of one broadcast, say a thumbnail
and a full-screen view, do not hold back each other's frames. Dropping the
player closes its clock and wakes everything waiting on it.

## Latency

`PlayerConfig::latency` is a `Latency { min, max }`.

`min` is the clock's delay above. `Player::set_latency` starts a new clock with
the new delay, so the schedule jumps once, and the next video frame or audio
write anchors it again. `irl watch --latency` offers
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
use iroh_live::media::{Latency, PlayerConfig, video::decode};

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
delay. `audio.buffered` is
the sink's fill level alone. `video.decode_time` is one transport read and
decode together, smoothed. `video.fps` counts frames over a window instead of
deriving a rate from the gap between two frames. See [developer
tools](devtools.md) for the overlay that draws them.
