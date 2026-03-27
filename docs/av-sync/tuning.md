# A/V sync tuning

## The two knobs

`PlaybackPolicy` has two fields that control playout behavior:

- **`sync: SyncMode`** — `Synced` (default) enables the shared playout
  clock; `Unmanaged` falls back to PTS-cadence pacing with no
  cross-track alignment.
- **`max_latency: Duration`** — maximum span of buffered media before
  Hang's ordered consumer skips forward. Default 150 ms.

```rust
PlaybackPolicy::default()              // Synced, 150 ms
    .with_max_latency(Duration::from_millis(500))
```

### When to increase `max_latency`

- Continuity matters more than fast recovery.
- You are on a stable but high-latency path.
- Short stalls should not immediately skip over content.

### When to decrease `max_latency`

- You care more about returning to the live edge quickly.
- Congestion or loss produces long stale runs that are no longer useful.
- You are debugging recovery after latency spikes.

## Changing the policy at runtime

`RemoteBroadcast::set_playback_policy` updates the policy for future
track subscriptions. Already-running pipelines keep whatever policy
they were created with. Typical pattern in a UI resubscribe flow:

```rust
broadcast.set_playback_policy(
    broadcast.playback_policy().with_sync(SyncMode::Unmanaged),
);
// now resubscribe to tracks — new pipelines use Unmanaged
```

## Reading the overlay

The Time section shows:

- **AudioBuf**: audio output ring buffer fill level in ms. Below ~20 ms
  risks underruns. Above ~200 ms means audio is queuing up faster than
  it plays.
- **VideoLag**: wall-clock drift from video PTS cadence since the first
  rendered frame. Positive means video is running behind real-time.
- **AudioLag**: same for the audio path.
- **A/V Δ**: `VideoLag - AudioLag`. Positive means video is behind
  audio. Under normal conditions this stays within ±30 ms.
- **VideoBuf**: decoded frames waiting in the video decode buffer.
  Typically 0–3 during normal playback, higher during DPB bursts.

What the numbers tell you:

- **AudioBuf dropping to zero**: network stall or decode cannot keep up.
  Audio will underrun and silence gets inserted. Check `max_latency` and
  network conditions.
- **VideoLag climbing steadily**: video decode or rendering cannot keep
  up with real-time. Check decoder FPS and decode_ms in the Render
  section.
- **A/V Δ large but both lags small**: the streams are near real-time
  but drifting apart. The shared playout clock should keep this tight;
  if it is not, check whether `SyncMode::Synced` is active.
- **Decoder FPS unstable**: the problem may be decode throughput or
  renderer cadence, not playout policy.
