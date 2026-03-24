# A/V sync tuning

The receive side exposes two knobs because there are two distinct tradeoffs.

```rust
PlaybackPolicy {
    sync: SyncMode,
    freshness: FreshnessPolicy,
}
```

If you remember one thing, remember this: `sync` controls how video relates to
audio at playout time. `freshness` controls how long we tolerate stale media
before skipping forward.

## Start with the defaults

The default playback policy is:

```rust
PlaybackPolicy::audio_master()
```

That means:

- `SyncMode::AudioMaster { video_hold_budget: 150ms }`
- `FreshnessPolicy { max_stale_duration: 500ms }`

Those defaults are chosen for interactive playback over real networks. They are
the right starting point for most subscribers.

## Tune `SyncMode` when the picture is the problem

`SyncMode` answers: how much should video wait, if at all, to stay close to the
speaker position?

### `SyncMode::AudioMaster`

Use this when you want normal live playback.

```rust
PlaybackPolicy::audio_master_with_hold(Duration::from_millis(150))
```

Increase `video_hold_budget` when:

- decoder burstiness causes visible micro-freezes after recovery;
- hardware decode emits frames in short bursts and then gaps;
- you prefer smoother video even if that adds a bit more visual delay.

Decrease `video_hold_budget` when:

- you want the lowest practical end-to-end delay;
- you can tolerate more video drops during jitter or recovery;
- you are debugging whether visible lag comes from playout hold versus decode.

In practice, this knob trades smoothness against delay. It does not change how
aggressively we skip stale transport history.

### `SyncMode::Unmanaged`

Use this only when you want raw decode behavior:

- diagnostics;
- local tools;
- tests that should ignore A/V sync policy.

It is useful because it removes one moving part, but it is not a better live
playback mode. It simply stops trying to align video to audio.

## Tune `FreshnessPolicy` when recovery is the problem

`FreshnessPolicy::max_stale_duration` answers: how far behind live are we
willing to fall before we stop preserving continuity and skip forward?

```rust
PlaybackPolicy::audio_master()
    .with_freshness(FreshnessPolicy::new(Duration::from_millis(500)))
```

Increase `max_stale_duration` when:

- continuity matters more than fast recovery;
- you are on a stable but high-latency path;
- short stalls should not immediately skip over content.

Decrease `max_stale_duration` when:

- you care more about returning to the live edge quickly;
- congestion or loss produces long stale runs that are no longer useful;
- you are debugging recovery after latency spikes.

This value is used in two places on purpose:

- Hang uses it as the ordered-consumer staleness ceiling.
- The video decode loop uses it as the stale-packet threshold before skipping
  toward a keyframe.

That keeps transport freshness and decode freshness aligned.

## Runtime tuning

`RemoteBroadcast` lets you change the stale-media threshold without rebuilding
the whole subscription:

```rust
remote.set_max_stale_duration(Duration::from_millis(300));
```

That is the runtime tuning hook we expect callers to use. It is appropriate for
tests, manual experiments, and products that want to react to higher-level
network policy.

We do not currently expose a runtime setter for `SyncMode`. Rebuilding the
track with a different `PlaybackPolicy` is the cleaner way to change the sync
model because it affects thread-local decode behavior and startup conditions.

## Read the overlay before changing knobs

When playback looks wrong, the timing overlay usually tells you which knob is
relevant.

- `audio_live_lag_ms` high and climbing: recovery back to live is the problem.
  Look at `max_stale_duration`.
- `video_live_lag_ms` high while `audio_live_lag_ms` is fine: video is the
  delayed track. Look at decode behavior and `video_hold_budget`.
- `av_delta_ms` large but live-lag values are small: the tracks are both near
  live, but not close to each other. Look at `SyncMode`.
- decoder FPS unstable while timing looks reasonable: the problem may be decode
  throughput or renderer cadence, not playout policy.

Tune from observed behavior, not from guesswork.
