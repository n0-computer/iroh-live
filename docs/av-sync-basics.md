# A/V Sync from Capture to Render

Real-time audio/video is a pipeline. A camera produces images, a
microphone produces sound, and the job of the system is to deliver both
to a remote viewer so they arrive in lockstep — lips matching words,
gestures matching sounds. When it works, nobody notices. When it
drifts by even 80 milliseconds, the experience feels subtly wrong.

This document walks through the entire path a single audio track (Opus)
and a single video track (H.264) take from capture to render. Along the
way, it introduces the industry terminology you will encounter in codec
specifications, RFCs, and media engineering discussions.

## The pipeline at a glance

```text
  Sender                              Network                     Receiver
 --------                           ---------                    ----------

 Camera (30fps)                                                  Video Renderer
     |                                                                ^
     v                                                                |
 H.264 Encoder ──> MoQ Publisher ─── QUIC stream ──> MoQ Sub ──> H.264 Decoder
                                                                      |
                                                                 Playout Buffer
                                                                      |
 Microphone (48kHz)                                              Audio Backend
     |                                                                ^
     v                                                                |
 Opus Encoder ───> MoQ Publisher ─── QUIC stream ──> MoQ Sub ──> Opus Decoder
                                                                      |
                                                                 Playout Buffer

                        Both tracks share a PlayoutClock
                     that maps timestamps to wall-clock time
```

Each box in the diagram adds latency. Some of that latency is fixed
(codec algorithmic delay), some is variable (network jitter, decode
time), and some accumulates slowly over minutes (clock drift). A/V sync
is the discipline of managing all three.


## Capture

### Video

A camera produces frames at a fixed interval. At 30 fps, a new frame
arrives every ~33 ms. Each frame gets a *presentation timestamp* (PTS)
— the moment it was captured, expressed in some time base (typically
microseconds since an epoch or since the stream started).

```text
time ──────────────────────────────────────────────>

  Frame 0      Frame 1      Frame 2      Frame 3
  PTS=0ms      PTS=33ms     PTS=67ms     PTS=100ms
  |            |            |            |
  ├────33ms────┤────33ms────┤────33ms────┤
```

### Audio

A microphone captures a continuous stream of samples at 48,000 Hz (48
kHz). Unlike video, there are no natural "frame" boundaries in the raw
audio — it is a river of numbers representing air pressure over time.
The encoder imposes frame boundaries later.

At 48 kHz, one second of mono audio is 48,000 samples. One second of
stereo audio is 96,000 samples (48,000 per channel).


## Encoding

Encoding compresses the raw capture data into a format suitable for
transmission. The two codecs behave very differently, and those
differences are central to the sync story.

### H.264 video encoding

H.264 is a video compression standard (also called AVC, for Advanced
Video Coding). It exploits two kinds of redundancy: spatial (within a
single frame, neighboring pixels tend to look alike) and temporal
(consecutive frames tend to look alike).

#### Frame types and the GOP

H.264 organizes frames into a *GOP* (Group of Pictures). A GOP starts
with a special frame called an *IDR keyframe* (Instantaneous Decoder
Refresh) and contains several types of frames.

- **I-frame (Intra)**: Compressed using only data from within the frame
  itself. Large, but decodable without any other frame. An IDR is a
  special I-frame that also resets the decoder state entirely — no
  future frame can reference anything before the IDR.

- **P-frame (Predicted)**: Compressed using a reference to one or more
  earlier frames. Smaller than I-frames because it only encodes what
  changed.

- **B-frame (Bidirectional)**: Compressed using references to both
  earlier *and* later frames. Smallest of the three, but requires the
  encoder and decoder to reorder frames. **B-frames are almost never
  used in live broadcasting** because they add latency (the encoder
  must buffer future frames) and complicate the decode pipeline. All
  encoders in iroh-live use Constrained Baseline profile, which
  forbids B-frames entirely.

A typical GOP *without* B-frames (the common case for live):

```text
 GOP                    GOP
 |                      |
 IDR  P  P  P  P  P    IDR  P  P  P  ...
  0   1  2  3  4  5     6   7  8  9
```

A GOP *with* B-frames (used in offline/VOD encoding, not in our stack):

```text
 GOP                         GOP
 |                           |
 IDR  B  B  P  B  B  P      IDR  B  B  P  ...
  0   1  2  3  4  5  6       7   8  9  10
```

The IDR keyframe is the only safe entry point into a stream. If a
receiver joins mid-stream or loses data, it must wait for the next IDR
before it can decode anything. This is why the *keyframe interval* (the
number of frames between IDRs) directly affects join latency and
recovery time.

#### PTS vs. DTS

Every frame carries two timestamps:

- *PTS* (Presentation Timestamp): when the frame should be displayed.
- *DTS* (Decode Timestamp): when the frame should be fed to the decoder.

**Without B-frames (our case)**: PTS equals DTS for every frame. Frames
are sent, decoded, and displayed in the same order. No reordering.

**With B-frames (VOD/offline)**: B-frames reference future P-frames, so
the encoder must send frames out of display order. DTS and PTS diverge,
and the decoder must reorder output. This is the source of the
DPB burst behavior described later — but since we use Constrained
Baseline (no B-frames), this reordering does not occur in our pipeline.

#### Encoder latency

The H.264 encoder introduces latency from several sources.

- **Rate control lookahead**: some rate control algorithms look ahead
  several frames to make better bitrate decisions. This adds
  proportional delay. Low-latency configurations disable this.

- **Transform and entropy coding**: the actual compression work.
  Hardware encoders (VAAPI, V4L2) do this in single-digit
  milliseconds. Software encoders (openh264, x264) may take 5--30 ms
  per frame depending on preset.

With no B-frames and no lookahead (our configuration across all
encoders — openh264, VAAPI, V4L2, VideoToolbox), the encoder adds
roughly one frame period of latency (~33 ms at 30 fps).

#### NAL units

The encoder's output is a sequence of *NAL units* (Network Abstraction
Layer units). Each NAL unit is a self-contained packet: it might be a
slice of a frame, parameter sets (SPS/PPS, which describe the stream's
resolution, profile, and coding tools), or supplemental metadata. NAL
units are what we put on the wire.


### Opus audio encoding

Opus is an audio codec designed for real-time communication. Where
H.264 is complex and has many knobs, Opus is streamlined for low
latency.

Opus divides the continuous audio stream into *frames* of fixed
duration. The standard frame size for voice-over-IP is 20 ms, which at
48 kHz means 960 samples per channel per frame.

```text
time ──────────────────────────────────────────────────>

  Opus frame 0    Opus frame 1    Opus frame 2
  PTS=0ms         PTS=20ms        PTS=40ms
  |               |               |
  ├────20ms───────┤────20ms───────┤────20ms──────┤
```

Opus encoding is fast — the algorithmic delay is 6.5 ms (for the SILK
layer) or 2.5 ms (for the CELT layer), and practical encode time on
modern hardware is under 1 ms. For sync purposes, Opus encoding
latency is negligible compared to H.264.

Opus has no equivalent of B-frames. Each frame is self-contained (or
references only the immediately preceding frame for prediction). PTS
and DTS are always identical. There is no reordering.


## The latency picture so far

Before the data even hits the network, the two tracks have diverged in
timing.

```text
                           Video (H.264)          Audio (Opus)
                           ─────────────          ────────────
Capture interval:          ~33ms (30fps)          continuous
Encode frame size:         ~33ms                  20ms
Encoder latency:           ~33ms (no B-frames)    <1ms

Total capture-to-wire:     ~33ms                  ~20ms
```

The video track enters the network ~13 ms behind the audio track. This
is normal and expected — the receiver must compensate for it.


## Network: MoQ over QUIC

Media over QUIC (MoQ) is the transport protocol we use. Audio and video
travel as separate *tracks* within the same QUIC connection, but the
mapping to QUIC streams is more granular than one-stream-per-track.

### MoQ track and group structure

In MoQ, each track (e.g. "video", "audio") is a sequence of *groups*.
For video, each group corresponds to a GOP — it starts with a keyframe
and contains all frames until the next keyframe. For audio, grouping
varies by implementation. hang's `moq-mux` batches ~100 ms of Opus
frames per group (5 frames at 20 ms each). In our stack, each Opus
frame is marked as a keyframe, so `OrderedProducer` creates a new
group per frame — one 20 ms frame per group.

The critical detail: **each group is sent on its own QUIC unidirectional
stream**. The publisher opens a new uni stream for every group, writes a
header identifying the track and group sequence number, sends all
frames of that group sequentially, then finishes the stream.

```text
QUIC Connection
│
├─ Control Stream (bidirectional)
│   ├─ SUBSCRIBE (subscribe_id=1, track="video")
│   ├─ SUBSCRIBE (subscribe_id=2, track="audio")
│   └─ ...
│
└─ Data Streams (each a QUIC unidirectional stream)
    │
    ├─ Video Group #0 ─── [Stream #1] ─── header(sub=1, seq=0) → IDR, P, P, P │ FIN
    ├─ Audio Group #0 ─── [Stream #2] ─── header(sub=2, seq=0) → Opus, Opus   │ FIN
    ├─ Video Group #1 ─── [Stream #3] ─── header(sub=1, seq=1) → IDR, P, P, P │ FIN
    ├─ Audio Group #1 ─── [Stream #4] ─── header(sub=2, seq=1) → Opus, Opus   │ FIN
    ├─ Audio Group #2 ─── [Stream #5] ─── header(sub=2, seq=2) → Opus, Opus   │ FIN
    ├─ Video Group #2 ─── [Stream #6] ─── header(sub=1, seq=2) → IDR, P, P, P │ FIN
    └─ ...
```

Groups are the unit of random access and the unit of latency
management. A receiver can skip an entire group if it arrives too late,
but it cannot skip individual frames within a group without corrupting
the decode.

### Why one stream per group matters for sync

Because each group gets its own QUIC stream, QUIC's stream-level
independence applies *within* a track, not just between tracks. If a
single packet in video group #1 is lost, only that stream stalls for
retransmission — video group #2 can arrive and be decoded while group
#1 is still waiting. Audio groups flow completely independently.

This is a significant advantage over TCP-based transports, where a
single lost packet blocks everything (head-of-line blocking). But it
also means any two groups — even from the same track — can experience
different delays at any given moment.

```text
Under normal conditions:

  Video G0  ───►  Video G1  ───►  Video G2  ───►
  Audio G0  ───►  Audio G1  ───►  Audio G2  ───►
                  All groups flow smoothly.

Under congestion (packet loss in Video G1's stream):

  Video G0  ───►  Video G1  ─╳── wait ──►   Video G2  ───►
  Audio G0  ───►  Audio G1  ───►  Audio G2  ───►
                  Video G1 stalls (retransmit), but G2 arrives fine.
                  Audio is completely unaffected.
                  hang may skip G1 if it exceeds max_latency.
```

This per-stream independence is a key source of *inter-track timing
differences* — moments where audio and video arrive at the receiver
with different delays. The receiver's playout system must absorb these
differences.


## hang: group buffering and ordering

*hang* is the library that manages group-level buffering and ordering on
the receive side. Its `TrackConsumer` is the interface between network
delivery and the decode pipeline.

### Group ordering

Since each group arrives on its own QUIC stream, groups can arrive out
of order — a later group's stream may complete before an earlier one
that is waiting for retransmission. hang collects incoming groups into
a `pending: VecDeque<GroupConsumer>`, ordered by sequence number, and
reassembles them into sequence order before handing them to the decoder.

### `max_latency` and group skipping

`TrackConsumer` (via hang's `OrderedConsumer`) has a `max_latency`
parameter that controls how much buffering the transport will tolerate.

The mechanism is a **span check** across all pending groups. hang
tracks the minimum and maximum frame PTS (presentation timestamp)
across all groups currently in its `pending` buffer. When the span
exceeds the threshold, the oldest groups are skipped:

```text
span = max_pts_across_pending - min_pts_across_pending

if span >= max_latency:
    skip all groups up to (but not including) the group
    containing the min PTS
```

This is different from a simple cutoff — it measures how spread out
the buffered media is, not how old any individual group is.

```text
max_latency = 500ms

Pending groups:       [G1: 0-400ms]  [G2: 500-900ms]  [G3: 1000-1400ms]
                                                              |
                      min_pts = 0ms (from G1)     max_pts = 1400ms (from G3)
                      span = 1400 - 0 = 1400ms > 500ms → SKIP

  G1 is skipped (oldest group, contains the min PTS frontier).
  After skip: pending = [G2, G3], span = 1400 - 500 = 900ms > 500ms → SKIP G2.
  After skip: pending = [G3], span = 1400 - 1000 = 400ms < 500ms → STOP.

  Decoder resumes at G3. Viewer sees a brief visual discontinuity
  but stays within the latency budget.
```

Note: the timestamps used are actual frame PTS values (varint-encoded
at the start of each frame payload in the MoQ stream), not group
sequence numbers. Each group tracks its own min/max PTS from the
frames it has buffered so far.

For video, skipping a group means skipping a whole GOP. The next group
starts with a keyframe, so the decoder can resume cleanly. For audio,
skipping a group creates a brief gap — Opus has packet loss concealment
(PLC) that can partially fill it.

`max_latency` can be updated at runtime via `set_max_latency()`,
allowing the system to tighten or relax its latency budget dynamically.


## Decoding

### H.264 video decoding and the DPB

The decoder uses a buffer called the *DPB* (Decoded Picture Buffer) to
hold reference frames. P-frames reference earlier frames, so the DPB
keeps those references alive until they are no longer needed.

#### Without B-frames (our case)

When there are no B-frames, decode order equals display order. In
theory, each frame could be output immediately after decoding. In
practice, **hardware decoders still introduce pipeline latency**.

VAAPI and V4L2 decoders pipeline their GPU work: they accept the next
frame's input while the previous frame is still being decoded. The
hardware decoder's internal pipeline typically holds 1--3 frames before
producing output. This creates bursty behavior even without B-frames:

```text
Baseline profile (no B-frames), VAAPI hardware decoder:

  Push IDR(0)  → decoder outputs: nothing (filling pipeline)
  Push P(1)    → decoder outputs: nothing (still filling)
  Push P(2)    → decoder outputs: IDR(0)        (pipeline primed)
  Push P(3)    → decoder outputs: P(1), P(2)    (burst! 2 frames at once)
  Push P(4)    → decoder outputs: P(3)
  Push P(5)    → decoder outputs: P(4)
               ...pipeline reaches steady state, ~1 frame per push...
  Push P(8)    → decoder outputs: P(7), P(8)    (occasional burst)
```

The exact behavior depends on the hardware, driver, and frame
complexity. The key takeaway: hardware decode output is not perfectly
regular, even without B-frames. The playout buffer exists to absorb
this variability.

Software decoders (openh264) are more predictable — they typically
output one frame per push with no pipeline delay.

#### With B-frames (not used in our stack)

When B-frames are present, the DPB also handles *reordering*: frames
arrive in decode order but must be output in display order. A B-frame
at display position 1 references the P-frame at position 3, so the
decoder must hold frame 1 until frame 3 has been decoded. This causes
even larger bursts (2--3 frames at once, regularly). This is why
low-latency profiles disable B-frames.

### Opus audio decoding

The Opus decoder is the easy case. Each 20 ms encoded frame goes in, 960
decoded samples per channel come out. There is no reordering, no
buffering, no burst behavior. Decode latency is effectively one frame
period: 20 ms.


## The playout buffer and PlayoutClock

Raw decoder output is not suitable for direct rendering. Video frames
arrive in bursts (from the DPB), and network jitter means the time
between received groups is not perfectly regular. The *playout buffer*
sits between the decoder and the renderer, absorbing this variability.

### What the playout buffer does

> **Current impl note:** Only video uses a `PlayoutBuffer`. Audio
> does not call `clock.observe_arrival()` — it pushes decoded samples
> directly to the audio sink without playout-time gating. The audio
> backend's ring buffer provides its own smoothing. The clock is
> anchored exclusively by the video track's first decoded frame.

The playout buffer collects decoded frames and releases them at their
scheduled playout times. It converts bursty, jittery decoder output
into smooth, evenly-spaced rendering events.

```text
                       Without playout buffer:
                       (raw hardware decoder output, Baseline profile)

  time ──>  ·····················F0·F1F2····F3────F4·F5F6····
                                 ↑              ↑
                          pipeline primes     burst from
                          (2-3 frames out     GPU pipeline
                           at once)           irregularity


                       With playout buffer (80ms depth):
                       (smooth output)

  time ──>  ·········F0────F1────F2────F3────F4────F5────F6────
                     |     |     |     |     |     |     |
                     33ms  33ms  33ms  33ms  33ms  33ms  33ms

            Buffer absorbs hardware decoder bursts and releases
            frames at their PTS. 80ms depth ≈ 2.4 frames of runway
            at 30fps.
```

### PlayoutClock: mapping PTS to wall-clock time

Both audio and video tracks need to agree on *when* to present their
data. The PlayoutClock provides this agreement. It maps each track's
PTS values to wall-clock times using a shared time base.

The mapping is straightforward.

```text
playout_time(pts) = base_wall + (pts - base_pts)
```

where `base_wall = now + buffer_offset` at the moment the first frame
arrives, and `base_pts` is that first frame's PTS.

- `base_wall`: wall-clock anchor, set on the first `observe_arrival()`.
  Includes the buffer offset so the first frame plays after the buffer
  fills.
- `base_pts`: the PTS of the first frame received.
- `buffer_offset`: the playout buffer depth (e.g. 80 ms in Live mode,
  0 in Reliable mode).

> **Current impl note:** `base_wall` and `base_pts` are established by
> the video track's first decoded frame (only video calls
> `observe_arrival()`). Audio does not participate in anchoring.
> Additionally, `observe_arrival()` is self-healing: if a frame arrives
> whose playout time is already in the past (buffer underrun), it
> re-anchors `base_wall` forward to restore the buffer depth. This
> differs from hang's JS `Sync`, which tracks the *minimum*
> `now - timestamp` across all frames as its reference and recomputes
> it continuously.

Both tracks reference the same `base_wall`. This means if the
sender captured audio and video with consistent timestamps, the
receiver will present them at the correct relative times — even though
the two tracks traveled different network paths and were decoded by
different codecs with different latencies.

### Per-track gating

Each track independently waits for its playout time before releasing a
frame. There is no explicit cross-track comparison — no logic that says
"audio is ahead, so hold it until video catches up." Instead, both
tracks race toward the same wall-clock targets and the shared
PlayoutClock keeps them aligned.

> **Current impl note:** Video gates on `playout_time()` via
> `PlayoutBuffer::pop_ready()`. Audio does *not* gate — it pushes
> decoded samples to the audio sink immediately and relies on the
> audio backend's ring buffer for smoothing. Only video calls
> `observe_arrival()`, so the clock is anchored entirely by video's
> first decoded frame. Audio effectively plays as soon as it arrives,
> which works well because the audio backend absorbs jitter at the
> hardware level.

This design is inspired by the hang reference implementation, where
both audio and video call `Sync.wait(timestamp)` independently. The
reference point is the earliest `now - timestamp` observed across all
tracks, and the effective latency is the maximum of the two track
delays plus a jitter margin.

```text
PlayoutClock
     |
     ├── Video track: playout_time(video_pts) → wait until wall-clock matches
     |
     └── Audio track: samples pushed directly to audio backend
                      (no observe_arrival — clock anchored by video only)

No cross-track sync_action, skip, or wait logic.
Both tracks converge to the same timeline through the shared clock.
```

This is simpler and more robust than explicit cross-track
synchronization. When one track is delayed (e.g. video during a network
stall), the other track continues at its natural pace. The PlayoutClock
does not force them to lock-step — it gives them the same target and
lets the playout buffers absorb short-term differences.


## Rendering

### Video

The renderer displays each video frame at its scheduled playout time. In
practice, this means maintaining a small queue of "ready" frames and
picking the one whose playout time is closest to (but not after) the
current wall-clock time.

If a frame's playout time has passed and it has not been displayed, it
is dropped. If no frame is ready at the current time, the renderer
holds the previous frame (a "freeze"). Both are visible artifacts, but
the playout buffer's job is to make them rare.

### Audio

Audio rendering works differently from video because the audio backend
(ALSA, PulseAudio, CoreAudio) drives the timing. The backend maintains
a ring buffer and pulls samples from it at a fixed rate — exactly
48,000 samples per second, governed by the sound card's hardware clock.

The application's job is to keep the ring buffer fed. Decoded Opus
frames are written into the ring buffer as they arrive. If the
buffer runs dry (underrun), there is an audible glitch. If the buffer
overflows, samples are dropped.

> **Current impl note:** The audio decode loop pushes samples to the
> sink immediately after decoding — there is no explicit playout-time
> wait. The audio backend's ring buffer (typically 10–50 ms deep)
> absorbs jitter naturally. This is simpler than gating on
> `playout_time()` and works because the OS audio subsystem provides
> its own clock-governed smoothing.

```text
Audio ring buffer (managed by OS audio backend):

  ┌───────────────────────────────────────────────┐
  │ samples  samples  samples  [write cursor]     │
  │  played   ──>     ──>        ──>              │
  │                              [read cursor]    │
  └───────────────────────────────────────────────┘
                                     ↑
                              Opus decoder writes here
                              as frames arrive (no playout gating)

  The hardware read cursor advances at exactly 48kHz.
  The write cursor must stay ahead of it.
```


## Drift

Even when everything is working, audio and video wall-clock positions
can slowly diverge over time. This is *drift*, and it has several
causes.

### Clock skew

The sender's camera and microphone may be driven by different hardware
clocks with slightly different frequencies. If the camera runs at
29.9997 fps instead of exactly 30, and the microphone captures at
exactly 48,000 Hz, the timestamps will diverge by roughly 1 ms per 100
seconds. Over a ten-minute call, that is 6 ms of drift — small, but it
compounds.

The same problem exists on the receiver side. The sound card's hardware
clock (which governs audio playout rate) and the system clock (which
governs video frame scheduling) may run at slightly different rates.

### Network-induced drift

Under sustained congestion, one QUIC stream may consistently receive
less bandwidth than the other. If video packets are routinely delayed
more than audio packets over a period of seconds, the video track
accumulates a time offset relative to audio. The playout buffer can
absorb short bursts of this, but sustained asymmetry pushes the tracks
apart.

In our system, both tracks share a single QUIC connection, which limits
this effect — the congestion controller treats all streams on the
connection together. But per-stream head-of-line independence means the
effect is reduced, not eliminated.

### Variable decode time

H.264 decode time varies per frame: IDR keyframes take longer than
P-frames because they are larger and reset decoder state. If the decoder
occasionally takes 40 ms instead of 20 ms for a complex keyframe, that
variability accumulates over time unless the playout buffer absorbs it.
Hardware decoders also have variable GPU scheduling latency. Opus decode
time is constant enough that it does not contribute meaningfully to
drift.

### Correcting drift

Drift correction is a slow, gentle process. The PlayoutClock can
periodically measure the actual audio-to-video offset (by comparing
each track's most recent PTS against the wall clock) and apply a small
rate adjustment. For audio, this means resampling — playing back at
48,001 Hz instead of 48,000 Hz for a while — which is inaudible. For
video, it means slightly adjusting frame hold times, which is
invisible.

The key principle: never correct drift with a sudden jump. A 50 ms
discontinuity in audio is a click. A 50 ms jump in video is a visible
stutter. Gradual adjustment over seconds is the correct approach.


## Putting it all together: an end-to-end timeline

Here is a concrete example of one video frame and its contemporaneous
audio frame traveling through the entire pipeline. Assume 30 fps video,
20 ms Opus frames, Constrained Baseline profile (no B-frames, as all
our encoders use), and a VAAPI hardware decoder.

```text
                              Video Frame (PTS = 100ms)     Audio Frame (PTS = 100ms)
                              ─────────────────────────     ────────────────────────
 Capture                      t = 100ms                     t = 100ms
                              Camera delivers frame         Mic samples 960 per ch.

 Encode                       t = 133ms (+33ms)             t = 101ms (+~1ms)
                              H.264 encodes 1 frame         Opus encodes 20ms frame
                              period of pipeline delay

 Network send                 t = 134ms                     t = 102ms
                              NAL unit → MoQ group          Opus packet → MoQ group

 Network transit              t = 154ms (+20ms RTT/2)       t = 122ms (+20ms RTT/2)
                              One-way delay, same for both if no congestion

 hang receive & order         t = 155ms (+~1ms)             t = 123ms (+~1ms)
                              Group sequenced in pending    Group sequenced in pending
                              VecDeque                      VecDeque

 Decode                       t = 188ms (+33ms)             t = 143ms (+20ms)
                              VAAPI: 1 frame pipeline       Opus: 1 frame out
                              delay (no B-frames)

 Playout buffer wait          target = base + 100 + 80      (no playout wait —
                              = base + 180ms                 samples pushed to
                                                             audio backend immediately)

 Render                       Video displayed at            Audio samples written to
                              wall_clock = base + 180ms     ring buffer at t ≈ 143ms
                              ─────────────────────────     ────────────────────────
```

> **Current impl note:** Audio does not wait for `playout_time()`.
> Samples are pushed to the audio sink immediately after decode. The
> audio backend's ring buffer provides ~10–50 ms of smoothing. This
> means audio and video are not precisely synchronized in the current
> implementation — audio plays as soon as it arrives, while video
> gates on the shared clock. In practice, the audio backend's internal
> buffering provides enough slack that the result sounds correct, but
> true A/V sync (where audio also waits for its playout time) is not
> yet implemented.

*Ideally*, both frames would target the same playout time (base + 180
ms). The audio frame would wait longer in its buffer (37 ms) while the
video frame waits less (arriving just in time). The buffer absorbs the
asymmetry and the viewer perceives synchronization.

If the buffer is too shallow, the video frame may arrive *after* its
playout time and get dropped. If the buffer is too deep, latency
increases unnecessarily. Tuning the buffer depth is the central tradeoff
of real-time A/V: deeper means more resilience to jitter but higher
latency, shallower means lower latency but more drops.


## Glossary

*B-frame*: A video frame compressed using references to both earlier and
later frames. Requires reordering at both encoder and decoder.

*DPB (Decoded Picture Buffer)*: The decoder-side buffer that holds
reference frames. With B-frames, also reorders output into display
order. Without B-frames, still present as hardware decoders pipeline
their GPU work and buffer 1--3 frames internally.

*DTS (Decode Timestamp)*: The time at which a frame should be fed to the
decoder. Differs from PTS when B-frames cause reordering.

*Drift*: Slow divergence between audio and video timing over minutes or
hours, caused by clock skew or sustained network asymmetry.

*GOP (Group of Pictures)*: A sequence of video frames starting with a
keyframe. The unit of random access in H.264.

*IDR (Instantaneous Decoder Refresh)*: A keyframe that resets all
decoder state. No frame after an IDR can reference anything before it.

*Jitter*: Short-term variation in packet arrival times. Causes frames to
arrive earlier or later than expected, even when average throughput is
stable.

*Keyframe*: A video frame that can be decoded independently, without
reference to any other frame. Synonymous with I-frame in H.264.

*NAL unit (Network Abstraction Layer unit)*: The packetization unit of
H.264. Each NAL unit is a self-contained chunk: a slice of a frame,
parameter sets, or metadata.

*PLC (Packet Loss Concealment)*: A technique Opus uses to mask missing
audio frames by extrapolating from the previous frame's state.

*PTS (Presentation Timestamp)*: The time at which a frame should be
displayed or played to the user.

*Playout buffer*: A buffer between decoder output and rendering that
smooths jitter and maps PTS to wall-clock time.

*PlayoutClock*: A shared clock that maps PTS values from all tracks to
wall-clock playout times, ensuring inter-track synchronization.
