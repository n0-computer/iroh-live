# Playout and A/V sync

Audio and video decode independently, on separate tasks, from separate tracks.
Something has to keep them together at playout time, and no moq crate has one.
Each `Player` owns a `PlayoutClock` (`iroh-live-media/src/player/clock.rs`) that
is that clock, ported from the moq/js player (`js/watch/src/sync.ts` at commit
`53fe78d8`) with the same data model and the same arithmetic in `i64`
milliseconds.

## The algorithm

The clock keeps one number, the *reference*: the earliest
`wall_now - frame_pts` it has ever seen. It only ever moves earlier. Every frame
that arrives faster than any previous one tightens it, and nothing loosens it.

A frame with timestamp `T` is due at wall time `reference + T + latency`, where
`latency` is `audio + jitter`. `jitter` is the network allowance, which is the
player's `Latency::min` and defaults to 100 ms, and `audio` is how much sound is
queued at the speaker.

`received(pts)` updates the reference, and `wait_async(pts)` sleeps until the
frame is due, returning `false` if the clock closed underneath it. The wait is
recomputed whenever the clock moves under it, so a reference that tightened, a
changed jitter, or a new audio depth still holds the frame back correctly.
`delay(pts)` exposes the same arithmetic as a `Delay` value for a caller
driving its own timer. Until the first frame sets a reference, every frame is
due at once rather than stalling.

## Who calls what

Video calls both halves. The player's video task records each decoded frame's
arrival with `received`, holds it until `wait_async` returns, and only then puts
it in the `VideoFrames` slot the renderer reads. The hold is a future the
supervisor keeps across loop iterations rather than one it awaits in place, so a
rendition switch requested while a picture is held is acted on at once.

Audio never calls either. It writes decoded frames straight to its output sink
and lets the sink's own buffer absorb jitter. What it does contribute is the
sink's buffered duration on every frame, reported through an `AudioLatency`
guard it takes from `PlayoutClock::register_audio` when the track opens. How much
sound is still queued ahead of the speaker is the one latency either side can
actually measure, and video holds frames back by it. Without that coupling a
video frame renders as soon as it decodes while its audio is still behind 50 ms
of queued sound.

The guard is the only way the audio term can be set, and dropping it clears the
term. An audio track that ends, fails, or is dropped therefore stops holding
video back with it, rather than leaving a stale depth in the arithmetic.

Beyond that one number the paths never signal each other. They converge because
they share a reference and a latency target, which is the property that made the
JS design worth porting after three earlier attempts at cross-path gating did
worse than no synchronization at all.

The clock belongs to one player. Two players of the same broadcast, say a
thumbnail and a full-screen view, each have their own, so neither holds the
other's frames back, and a player with no audio output has no audio term at all.
Dropping the player closes its clock and wakes everything waiting on it.

## Latency

`PlayerConfig::latency` carries the knobs a caller turns, as a `Latency`.

`min` is the jitter allowance in the arithmetic above, and is the largest delay
a player adds on its own. `Player::set_latency` applies it at once, because the
clock reads it on every wait. `irl watch --latency realtime|balanced|smooth` is
the CLI over it, at 60, 100, and 400 ms. `iroh-live/tests/latency.rs` measures
what the rest of the pipeline costs, with publisher and subscriber on one clock,
once with `Latency::IMMEDIATE` and once with the default: 84ms with no playout
hold, 203ms under the default.

`max` becomes `max_age` on `moq_video::decode::Options` and
`moq_audio::decode::Options`, which is where upstream decides how old buffered
media may get before it skips forward to the live edge. The default is 150 ms.
Raise it when continuity through congestion matters more than returning to the
live edge quickly; lower it when a stall should be skipped over rather than
played out. It should stay above `min`, since a skip threshold below the hold
the clock deliberately adds throws away the very frames that hold exists to wait
for; `play` and `set_latency` refuse a `min` above `max`.

A `min` of zero turns pacing off: frames go to the renderer as they decode,
with no clock at all. `Latency::IMMEDIATE` is that, with a `max` of 150 ms so
the player still skips to the live edge. It suits a test or a consumer that
sets its own cadence, and it is not what you want for live playback with audio.

```rust
use std::time::Duration;
use iroh_live_media::{Latency, PlayerConfig, video::decode};

PlayerConfig::default()                       // 100 ms hold, 150 ms max, Auto
    .with_latency(Latency::range(
        Duration::from_millis(400),
        Duration::from_millis(600),
    ))
    .with_decoder(decode::Kind::Software)
```

`decoder: decode::Kind` becomes the decoder `kind` on
`moq_video::decode::Options`, which is where upstream chooses a backend. `Auto`
tries the platform's hardware decoders in turn and falls back to software. A
named backend is the only one tried, so a machine without it fails to open
rather than falling back, which is what makes the choice useful for telling a
driver problem from a stream problem. The player always asks the decoder to
leave each picture on the GPU, since its frames go to a renderer, and a frame
converts to CPU pixels on demand for anything that reads them.

A changed `max` or decoder applies to a running player without a gap:
`set_latency` and `set_decoder` build a replacement video decoder behind the
picture, and it takes over once it has caught up, the same way a rendition
switch does. An audio track keeps the `max` it opened with until it next
reopens.

## Reading the timing metrics

`Player::stats()` returns a `PlaybackStats` snapshot. `latency` is the clock's
current total, the jitter allowance plus whatever audio is queued, which is how
far behind its arrival a picture is shown. `audio.buffered` is the sink's fill
level on its own, and `video.decode_time` is how long one transport read and
decode take together, smoothed. `video.fps` counts frames shown over the last
second rather than deriving a rate from the gap between two frames.

There is no per-path lag or A/V delta figure. The overlay draws what the
snapshot carries; see [developer tools](devtools.md).
