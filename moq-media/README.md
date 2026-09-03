# moq-media

Publish and subscribe plumbing over
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio). No iroh dependency: a
broadcast arrives as a `moq_net::broadcast::Producer` or `Consumer`, whatever
carried it.

The media itself is upstream. `moq_video` captures, encodes, decodes, and
renders; `moq_audio` does the same for sound and owns the speaker. Both are
re-exported as `moq_media::video` and `moq_media::audio`, so a dependent names
the exact build this crate links. What lives here is the layer above, which moq
has no counterpart for.

## Publishing

`LocalBroadcast` owns a broadcast producer and the catalog that describes it.
`VideoPublisher` and `AudioPublisher` take a source and a set of renditions.

```rust
use moq_media::{publish::LocalBroadcast, video};

let broadcast = LocalBroadcast::new(producer)?;
broadcast.video().set(video::capture::Config::default())?;
```

The one thing this adds over `moq_video::encode::publish_capture` is simulcast.
Upstream, one producer publishes one rendition and owns the device it captures
from, so a subscriber that adapts to its downlink cannot be served. Here the
source is opened once and its frames fan out to an encoder per rendition, each
encoding only while someone is watching it.

`VideoSource` is a capture device, a stream of frames the application produced,
or an Annex-B H.264 byte stream a source already encoded. The last is the
Raspberry Pi path.

## Subscribing

`RemoteBroadcast` watches a broadcast's catalog and hands out a `VideoTrack` and
an `AudioTrack`. Decoding is `moq_video::decode::Consumer` and
`moq_audio::decode::Consumer`; three things around it are ours.

`VideoTrack::enable_adaptation` follows transport signals and switches
renditions, opening the replacement decoder alongside the incumbent and swapping
on its first frame, so the picture never goes blank. `sync::Sync` is a shared
playout clock that keeps audio and video aligned across two independent decode
paths. And `catalog::IrohLiveExt` extends hang's catalog with chat and publisher
identity, flattened alongside the media sections so a base consumer ignores them.

## Modules

| Module | What it is |
|---|---|
| `publish` | `LocalBroadcast` and the simulcast ladder |
| `subscribe` | `RemoteBroadcast`, the decode supervisor, and the rendition swap |
| `adaptive` | The rendition selection algorithm and its thresholds |
| `sync`, `playout` | The playout clock and the policy that drives it |
| `catalog` | The iroh-live catalog extension |
| `playback` | The process-wide audio output engine |
| `stats` | Metrics for a debug overlay |
| `frame_channel` | A single-slot latest-wins channel for frames |
| `audio_file` | An audio file demuxed with symphonia and published as if it were a microphone |
| `rpicam` | `rpicam-vid` as a pre-encoded video source |
| `test_source` | Generated video and audio, for tests, and a `timing` pattern built to diagnose playback |
| `net` | `NetworkSignals`, the input to adaptation |

## Feature flags

Every codec compiles unconditionally upstream, so there are no per-codec flags.
What is left gates a build dependency or a graphics stack.

| Feature | Default | What it adds |
|---|---|---|
| `capture` | yes | Camera, screen, and microphone devices |
| `playback` | no | Speaker output |
| `aec` | no | Echo cancellation. Implies `capture` and `playback` |
| `pipewire` | no | Linux screen capture. Links `libpipewire-0.3` |
| `render` | no | The wgpu renderer |
| `vaapi` | no | Intel and AMD hardware H.264 encode |
| `nvidia` | no | NVIDIA hardware encode and decode |
| `rpicam` | no | The `rpicam-vid` source. Linux only |
| `test-source` | no | Generated video and audio sources |
