# Playout and A/V Sync

| Field | Value |
|-------|-------|
| Modified | 2026-03-19 |
| Status | stable |
| Applies to | moq-media |

The playout system sits between the decoder and the renderer, solving two
problems: smoothing bursty decoder output into a steady frame cadence,
and keeping audio and video tracks synchronized.

## PlayoutMode

Two modes serve fundamentally different use cases:

**`PlayoutMode::Live { buffer, max_latency }`** targets real-time
playback. The `buffer` duration offsets frame display from decode time,
absorbing decoder burst output. The `max_latency` ceiling is propagated
to hang's `TrackConsumer`, which skips entire groups older than this
threshold at the transport level. Late video frames may be skipped to
maintain sync with the audio master. The default is `buffer: 0`,
`max_latency: 150ms`.

**`PlayoutMode::Reliable`** delivers every frame in order without
skipping. No latency target is enforced. Hang's group-skip threshold
is set very high (one hour) to avoid dropping any groups. This mode
suits recordings, demos, and debugging where completeness matters more
than latency.

## PlayoutClock

`PlayoutClock` is the shared time reference for all tracks in a
broadcast. Both audio and video decoder threads hold a clone of the
same instance (it is `Arc<Mutex<...>>` internally).

The clock maps media timestamps (PTS) to wall-clock playout times. On
the first frame arrival, it establishes a base mapping:

```
base_pts  = first frame's PTS
base_wall = now + buffer_duration
```

For subsequent frames:

```
playout_time = base_wall + (frame_pts - base_pts)
```

This means a frame whose PTS is 100ms after the first frame's PTS will
be scheduled for display 100ms after the first frame's playout time,
regardless of when it actually arrived from the network.

### Jitter measurement

The clock measures inter-arrival jitter per RFC 3550, using an
exponential moving average:

```
jitter_sample = |actual_interval - expected_interval|
smoothed_jitter += (jitter_sample - smoothed_jitter) / 16
```

This is diagnostic only; it does not drive playout timing. The value is
available via `clock.jitter()` for UI display and debugging.

### Re-anchoring

When a stream discontinuity occurs (e.g., after a rendition switch or
a long pause), the clock detects that incoming PTS values have jumped
relative to the established base and re-anchors the mapping. This
prevents a single discontinuity from making all subsequent playout times
wrong.

`clock.reanchor_stats()` returns the total accumulated drift and the
number of re-anchor events, useful for diagnosing stream stability.

### Mode changes

`clock.set_mode(mode)` adjusts the playout behavior at runtime. If a
base mapping already exists, it shifts `base_wall` by the difference in
buffer offset so frames already in the playout buffer get correct new
playout times without a visible gap.

`clock.set_buffer(duration)` adjusts the Live mode buffer duration
independently of `max_latency`. This is a no-op in Reliable mode.

## PlayoutBuffer

A per-decoder-thread buffer that holds decoded `VideoFrame` values in a
`VecDeque`, ordered by arrival. Frames are inserted as they come from the
decoder and released when their playout time arrives.

The buffer's `recv()` method combines waiting for new packets from the
transport with checking whether buffered frames are ready for playout. It
returns the next frame whose playout time has passed, or reports how long
to wait until one is ready. A safety valve caps the buffer at 30 frames
(one second at 30fps); overflow drops the oldest frames.

### DPB burst absorption

This is the primary reason the PlayoutBuffer exists. Hardware video
decoders (VAAPI, cros-codecs) use a Decoded Picture Buffer (DPB) that
introduces bursty output at keyframe boundaries. For H.264 Baseline at
720p with a 5-frame DPB (level 3.1), a keyframe causes the DPB to flush
approximately three frames simultaneously, followed by a gap of two to
three packets where no frames emerge while the DPB refills.

Without a playout buffer, this burst-then-gap pattern produces visible
stutter every GOP (typically once per second). The PlayoutBuffer absorbs
the burst: the three frames have PTS values approximately 33ms apart, so
they are released at 33ms intervals. During the DPB refill gap, the
buffer already has frames queued from the earlier burst. In practice, a
buffer depth of three to five frames is enough to smooth DPB output
completely.

## A/V sync

Audio and video tracks share the same `PlayoutClock`, which provides
the shared time anchor. Both tracks independently map their PTS values
to wall-clock playout times through the same base reference. Because
the anchor is shared and the PTS-to-wall mapping is identical, the
tracks stay synchronized without explicit cross-track coordination.

Audio acts as the sync master. Video frames that fall behind the audio
playout position by more than half the configured `max_latency` are
candidates for skipping in Live mode. In Reliable mode, no frames are
ever skipped; drift is absorbed by waiting.

## Integration with hang

The playout system operates on top of hang's group-level latency
management. Hang's `OrderedConsumer` handles coarse-grained skipping:
when the transport falls behind by more than `max_latency`, entire
groups (which start at keyframes) are discarded. The PlayoutBuffer
handles fine-grained timing: within the frames that survive hang's
group skip, it ensures steady cadence and cross-track sync.

| Layer | Live mode | Reliable mode |
|-------|-----------|---------------|
| hang group-skip | Active (e.g. 150ms ceiling) | Disabled (1-hour ceiling) |
| `current_frame()` render-side skip | Active (returns latest) | Active but buffer rarely has >1 frame |
| PlayoutBuffer frame-level timing | Targets buffer duration | Delivers ASAP in PTS order |
| A/V sync | Skip video if behind audio | Wait, never skip |

## Integration with decoder threads

The PlayoutBuffer sits inside the video decoder thread, between
`decoder.pop_frame()` and the output channel send. The decode loop:

1. Waits for the next packet or the next playout deadline, whichever
   comes first.
2. Feeds any received packet to the decoder immediately (keeping the
   DPB fed).
3. Drains all decoded frames from the decoder into the PlayoutBuffer.
4. Releases all frames whose playout time has passed into the output
   channel.

This design ensures decoder pool buffers are freed immediately (step 3)
while frame display timing is controlled by the playout clock (step 4).
