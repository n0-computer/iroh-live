# `AudioTrack::visualize()` → `WatchTrack`

## Context

Audio tracks support peak metering via `AudioSinkHandle::smoothed_peak_normalized()`, but there was no way to get a visual representation of audio as renderable frames. Video tracks already have `WatchTrack` which provides `current_frame() -> Option<DecodedFrame>` for polling RGBA frames. This adds the same interface for audio visualization — `AudioTrack::visualize(mode)` returns a `WatchTrack` that renders audio data into images, compatible with the existing `VideoView` egui component.

## Design

Audio packets flow through the decoder thread into an `AudioSink` (speakers). A lightweight `SampleRing` (shared ring buffer, `Arc<Mutex<VecDeque<f32>>>`, ~100ms capacity) forks decoded samples after they reach the sink. `visualize(mode)` spawns a render thread that reads from the ring at ~30fps, renders RGBA frames, and sends them through the same `mpsc::Receiver<DecodedFrame>` channel that `WatchTrack` already uses.

```
Audio packets → Decoder thread → samples → AudioSink (speakers)
                                    ↓ (fork)
                              SampleRing (Arc<Mutex<VecDeque<f32>>>)
                                    ↓ (read at 30fps)
                              Render thread → RgbaImage → WatchTrack
```

## API

```rust
use moq_media::{Visualization, subscribe::AudioTrack};

let vis = audio_track.visualize(Visualization::Waveform);
// or
let vis = audio_track.visualize(Visualization::Peaks);
// Use like any WatchTrack: vis.current_frame(), display with VideoView, etc.
```

## Visualizations

- [x] **Waveform** — Scrolling oscilloscope showing the most recent ~100ms of audio. Min/max envelope per pixel column for crisp display at any size. Dark background, green foreground, center guide line.
- [x] **Peaks** — Per-channel horizontal bars showing RMS level. Green → yellow → red color gradient. White peak-hold markers at max recent level. Similar to DAW channel meters.

## Files changed

- [x] `moq-media/src/visualize.rs` — New — SampleRing, Visualization, renderers, spawn_visualizer, unit tests
- [x] `moq-media/src/subscribe.rs` — Add sample_ring/channels to AudioTrack, fork samples in run_loop, add visualize(), add WatchTrack::from_thread
- [x] `moq-media/src/lib.rs` — Add `pub mod visualize`, re-export `Visualization`

## Future work

### 1. Spectrogram / FFT

Add `Visualization::Spectrum` using FFT to show frequency bands as vertical bars.

**Impl**: Add `rustfft` dep. Add `render_spectrum(samples, channels, width, height) -> RgbaImage` in `visualize.rs`. Mix to mono, apply Hann window, run real FFT on last 1024 samples, take magnitude of first 512 bins, map bins to pixel columns (log-frequency scale: group bins into ~`width` bands via `band = (bin as f32 / 512.0).powf(0.5) * width`), draw vertical bars colored by magnitude (same green→yellow→red gradient as peaks). Add variant to `Visualization` enum. ~60 lines in `render_spectrum` + 1 dep.

### 2. Configurable colors/themes

Allow passing custom colors instead of hardcoded constants.

**Impl**: Add struct to `visualize.rs`:
```rust
pub struct VisualizationStyle {
    pub background: [u8; 4],
    pub foreground: [u8; 4],
    pub guide: [u8; 4],
    pub peak_hold: [u8; 4],
}
```
Default impl returns current hardcoded colors. Change `AudioTrack::visualize` signature to `visualize(&self, mode, style: Option<VisualizationStyle>)` (or add `visualize_styled`). Thread style into `spawn_visualizer` → render functions. Replace `BG`/`WAVE_FG`/`GUIDE`/`PEAK_HOLD` constants with style fields. ~30 lines of plumbing.

### 3. Stereo split view

Show L/R channels as separate waveforms (top/bottom halves).

**Impl**: Add `Visualization::WaveformStereo`. In a new `render_waveform_stereo` function, split image into top half and bottom half. For each half, extract one channel's samples (even indices = L, odd = R from interleaved buffer) and run the same min/max envelope logic as `render_waveform` but within that half's y-range. Draw a 1px guide line between halves. ~40 lines — mostly a copy of `render_waveform` with the channel split.

### 4. Local audio visualization

Add `visualize()` to `PublishBroadcast` audio (local mic input), not just subscribed remote audio.

**Impl**: In `moq-media/src/publish.rs`, add a `SampleRing` to the audio encoder pipeline. The audio capture path already has raw `&[f32]` samples before encoding — fork them into a ring just like `subscribe.rs` does after decoding. Add `sample_ring: SampleRing` + `channels: u32` fields to the audio publish struct. Add `pub fn visualize(&self, mode: Visualization) -> WatchTrack` that calls the existing `spawn_visualizer`. ~15 lines of changes mirroring what was done in `subscribe.rs`.

### 5. Text overlay / Debug mode

Render sample rate, channel count, buffer fill, codec info as text on the visualization.

**Impl**: Add `Visualization::Debug`. The `SampleRing` already tracks capacity — add a `fn len(&self) -> usize` method. Pass metadata as a small struct into the render function: `DebugInfo { sample_rate: u32, channels: u32, buffer_fill: usize, buffer_capacity: usize }`. Store `sample_rate` alongside `channels` in `AudioTrack`. For text rendering, use a minimal 5x7 bitmap font (hardcode ~40 glyphs as `&[u8; 5]` column bitmaps — digits, letters, colon, space, slash). Draw 3-4 lines of key-value text on the dark background. ~100 lines for the font table + `render_debug` function.

### 6. Smooth scrolling waveform

Continuously scrolling waveform instead of a static snapshot per frame.

**Impl**: Change `SampleRing` to track a monotonic write counter (`push` increments by `samples.len()`). In `spawn_visualizer`, maintain a `last_read_pos: u64` and a pixel column buffer (`Vec<(f32, f32)>` of min/max per column). Each frame: compute how many new samples arrived since `last_read_pos`, shift the column buffer left by the proportional number of columns, compute min/max for the new rightmost columns from the new samples, then render the column buffer to `RgbaImage`. Add `fn write_pos(&self) -> u64` and `fn snapshot_since(&self, pos: u64) -> (Vec<f32>, u64)` to `SampleRing`. ~50 lines of changes to ring + render thread.

### 7. GPU rendering

For high-res displays, offload visualization rendering to GPU shaders instead of CPU pixel writes.

### 8. Audio-reactive values

Expose computed values (RMS, peak, spectral centroid) as `Watchable<f32>` for driving UI animations beyond the image frame.
