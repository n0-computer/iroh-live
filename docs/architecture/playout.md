# Playout and A/V sync

Audio and video decode independently, on separate tasks, from separate tracks.
Something has to keep them together at playout time, and no moq crate has one.
`moq_media::sync::Sync` is that clock, ported from the moq/js player
(`js/watch/src/sync.ts` at commit `53fe78d8`) with the same data model and the
same arithmetic in `i64` milliseconds.

## The algorithm

The clock keeps one number, the *reference*: the earliest
`wall_now - frame_pts` it has ever seen. It only ever moves earlier. Every frame
that arrives faster than any previous one tightens it, and nothing loosens it.

A frame with timestamp `T` is due at wall time `reference + T + latency`, where
`latency` is `max(audio, video) + jitter`. `jitter` is the network allowance and
defaults to 100 ms, `audio` is how much sound is queued at the speaker, and
`video` is a decode latency a caller may set.

`Sync::received(pts)` updates the reference, and `Sync::wait_async(pts)` sleeps
until the frame is due, returning `false` if the clock closed underneath it.
`Sync::delay(pts)` exposes the same arithmetic as a `Delay` value for a caller
driving its own timer.

## Who calls what

Video calls both halves. `subscribe::video::deliver` records the arrival with
`received`, awaits `wait_async`, and only then hands the frame to the renderer.

Audio never calls either. It writes decoded frames straight to its
`moq_audio::playback::Sink` and lets the sink's own buffer absorb jitter. What it
does contribute is `Sync::set_audio_buffered(Some(sink.buffered()))` on every
frame. How much sound is still queued ahead of the speaker is the one latency
either side can actually measure, and video holds frames back by it. Without that
coupling a video frame renders as soon as it decodes while its audio is still
behind 50 ms of queued sound.

Beyond that one number the paths never signal each other. They converge because
they share a reference and a latency target, which is the property that made the
JS design worth porting after three earlier attempts at cross-path gating did
worse than no synchronization at all.

`Sync` is per-`RemoteBroadcast`, reachable through `RemoteBroadcast::sync()` for
retuning the jitter figure at runtime. Dropping the last handle, or calling
`shutdown()` on the broadcast, closes it and wakes everything waiting.

## Playback policy

`PlaybackPolicy` carries the two knobs a caller turns.

`sync: SyncMode` chooses between `Synced` and `Unmanaged`. `Synced` is the
default and runs the clock as described. `Unmanaged` skips it entirely: frames go
to the renderer as they decode, with no pacing at all. That suits a test or a
single-track playback where the renderer sets the cadence, and it is not what you
want for live playback with audio.

`max_latency: Duration` becomes `latency_max` on `moq_video::decode::Config` and
`moq_audio::decode::Config`, which is where upstream decides how much buffered
media to tolerate before skipping forward to the live edge. The default is
150 ms. Raise it when continuity through congestion matters more than returning
to the live edge quickly; lower it when a stall should be skipped over rather
than played out.

```rust
PlaybackPolicy::default()                              // Synced, 150 ms
    .with_max_latency(Duration::from_millis(500))
```

`RemoteBroadcast::set_playback_policy` affects tracks opened afterwards. A track
already decoding keeps the policy it was created with, so a UI that changes the
policy has to reopen the track for it to take effect.

## Reading the timing metrics

`moq_media::stats::TimingStats` defines the timing panel the egui overlay draws.
`audio_buf_ms` is the sink's fill level, `video_lag_ms` and `audio_lag_ms` are
wall-clock drift from each path's PTS cadence, and `av_delta_ms` is
`video_lag - audio_lag`, positive when video trails audio.

Nothing in this repository writes those four today. `LagTracker` exists and is
unused, and the audio and video decode paths record only `render.fps`. The
overlay draws whatever it finds, so the timing panel reads zero until something
fills it in. See [developer tools](devtools.md).
