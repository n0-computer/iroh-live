# A/V sync tuning

> A/V sync is currently disabled. `SyncMode` has only the `Unmanaged`
> variant; the `AudioMaster` variant was removed. The freshness controls
> below are still active.

## Freshness: the one knob that matters right now

`FreshnessPolicy::max_stale_duration` controls how far behind live we fall
before skipping forward. This is the primary congestion-response tuning
parameter.

```rust
PlaybackPolicy::default()
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

Hang uses this value as the ordered-consumer staleness ceiling, so it
controls how aggressively old groups are dropped before they reach the
decoder.

## Runtime tuning

`RemoteBroadcast` lets you change the stale-media threshold without
rebuilding the subscription:

```rust
remote.set_max_stale_duration(Duration::from_millis(300));
```

## Reading the overlay

The Time section shows:

- **AudioBuf**: audio output ring buffer fill level in ms. Below ~20ms risks
  underruns. Above ~200ms means audio is queuing up faster than it plays.
- **VideoLag**: wall-clock drift from video PTS cadence since the first
  rendered frame. Positive means video is running behind real-time.
- **AudioLag**: same for the audio path.
- **A/V Δ**: `VideoLag - AudioLag`. Positive means video is behind audio.
  Under normal conditions this stays within ±30ms. Larger values indicate
  the two streams are drifting apart.
- **VideoBuf**: decoded frames waiting in the video playout buffer. Typically
  0-3 during normal playback, higher during DPB bursts.

What the numbers tell you:

- **AudioBuf dropping to zero**: network stall or decode can't keep up. Audio
  will underrun and silence gets inserted. Check `max_stale_duration` and
  network conditions.
- **VideoLag climbing steadily**: video decode or rendering can't keep up with
  real-time. Check decoder FPS and decode_ms in the Render section.
- **A/V Δ large but both lags small**: the streams are near real-time but
  drifting apart. This is the gap that A/V sync would address when
  re-enabled.
- **Decoder FPS unstable**: the problem may be decode throughput or
  renderer cadence, not playout policy.
