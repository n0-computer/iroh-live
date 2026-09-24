# iroh-live-media

Sources, broadcasts, and players over
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio). No iroh dependency: a
broadcast is published through `moq_net::Consume` and read from a
`moq_net::broadcast::Consumer`, whatever carried it.

The media itself is upstream. `moq_video` captures, encodes, decodes, and
renders; `moq_audio` does the same for sound and owns the speaker. Both are
re-exported as `iroh_live_media::video` and `iroh_live_media::audio`, so a dependent names
the exact build this crate links. What lives here is the layer above, which moq
has no counterpart for.

## Publishing

`LocalBroadcast` owns a broadcast producer, the catalog that describes it, and a
media clock, with one video slot and one audio slot. A source is a value that is
already open, and `set_video` and `set_audio` hand it to the broadcast with an
encoding.

```rust
use iroh_live_media::{
    AudioEncoding, AudioSource, LocalBroadcast, VideoEncoding, VideoRendition, VideoSource,
    audio, video,
};

let broadcast = LocalBroadcast::new();
let camera = VideoSource::capture(video::capture::Config::default()).await?;
broadcast.set_video(
    camera,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
broadcast.set_audio(AudioSource::tone(440.0, audio::Layout::Mono), AudioEncoding::voice())?;
```

The one thing this adds over `moq_video::encode::publish_capture` is simulcast.
Upstream, one producer publishes one rendition and owns the device it captures
from, so a subscriber that adapts to its downlink cannot be served. Here the
source is opened once and its frames fan out to an encoder per rendition, each
encoding only while someone is watching it. `status()` reports which renditions
are encoding and with which backend, and `stats()` reports each rendition's
encoder on its own.

`VideoSource` is a capture device, the generated test pattern, frames the
application pushes (`push`) or produces on a thread of its own (`spawn`), or the
Raspberry Pi camera. `EncodedVideoSource` is an Annex-B H.264 byte stream a
source already encoded, published with `set_encoded_video`; that is the cheapest
Raspberry Pi path. `AudioSource` is a microphone, a WAV or MP3 file, a tone, or
pushed PCM. `VideoSource::push` and `spawn` take a `VideoFormat`.

A microphone is the one source that is not opened up front:
`AudioSource::microphone` checks that the device exists, and moq-audio opens it
only when a broadcast first has a listener for it, since it opens a microphone
only inside the publication that encodes it. A device that fails at that point
shows in `LocalBroadcast::status()` rather than as an error from `microphone`.

## Subscribing

`RemoteBroadcast` reads a broadcast's catalog and holds the subscription, and
`play(PlayerConfig)` starts a `Player` over it. Decoding is
`moq_video::decode::Consumer` and `moq_audio::decode::Consumer`; two things
around it are ours.

The player picks a rendition from the network signals a transport attached with
`RemoteBroadcast::with_network`, and switches by opening the replacement decoder
beside the incumbent and handing over once it has caught up, so the picture
never goes blank or steps backwards. And each player owns a playout clock that
holds video back by the audio queued at its speaker, so audio and video stay
aligned across two independent decode paths.

A transport builds the `RemoteBroadcast`: `from_origin` follows a path through
a route table and asks it again
when a change of route ends the broadcast, and `from_resolved` does the same
starting from a consumer the caller already resolved, which is what `iroh-live`
uses. A broadcast that follows a route table cannot tell a publisher ending the
broadcast from a change of route, so `closed()` resolves about three seconds
after the publisher ends it. `RemoteBroadcast::local` reads a `LocalBroadcast`
in-process, without a transport.

`record` remuxes a broadcast into a file without decoding. `Recording::stop`
takes `&mut self`, so it can share a `select!` with `wait`:

```rust
use iroh_live_media::RecordConfig;

let file = tokio::fs::File::create("out.mp4").await?;
let mut recording = remote.record(file, RecordConfig::default())?;
let written = tokio::select! {
    written = recording.wait() => written?,
    _ = tokio::signal::ctrl_c() => recording.stop().await?,
};
```

Audio plays through an `AudioOutput` the application opens and passes to each
`PlayerConfig`, and the same output goes to `MicrophoneConfig` to have its echo
cancelled.

## Modules

Every type is exported from the crate root; the modules are internal.

| Module | What it is |
|---|---|
| `source` | `VideoSource`, `EncodedVideoSource`, `AudioSource`, and the `FrameSender` that pushes into them |
| `publish` | `LocalBroadcast`, the simulcast ladder, and `PublishStatus` |
| `remote` | `RemoteBroadcast`, which follows a broadcast across route changes |
| `player` | `Player`, the rendition selector, the decoder handover, and the playout clock |
| `frames` | `VideoFrames`, the latest-wins frame stream every renderer reads |
| `output` | `AudioOutput`, an opened speaker or one that discards |
| `record` | Recording a broadcast to fragmented MP4 or Matroska without decoding |
| `catalog` | `Catalog`, the rendition descriptions, and the iroh-live catalog extension |
| `network` | `NetworkSignals` and `NetworkSample`, the input to adaptation |
| `stats` | `PublishStats` and `PlaybackStats` snapshots for a debug overlay |

## Feature flags

Every codec compiles unconditionally upstream, so there are no per-codec flags.
What is left gates a build dependency or a graphics stack. The test pattern and
tones need no flag.

| Feature | Default | What it adds |
|---|---|---|
| `capture` | yes | Camera, screen, and microphone devices |
| `sound-server` | yes | Reaches audio devices through PipeWire or PulseAudio |
| `playback` | no | Speaker output |
| `aec` | no | Echo cancellation. Implies `capture` and `playback` |
| `pipewire` | no | Linux screen capture. Links `libpipewire-0.3` |
| `render` | no | The wgpu renderer |
| `vaapi` | no | Intel and AMD hardware H.264 encode |
| `nvidia` | no | NVIDIA hardware encode and decode |
| `v4l2` | no | The V4L2 hardware H.264 codecs on ARM SoCs |
| `rpicam` | no | The `rpicam-vid` sources. Linux only |

## Errors

`iroh_live_media::Error` covers devices, codecs, the catalog, files, and
`Broadcast`, which is moq-net refusing a track or broadcast operation or
resetting a track being read, whatever transport carries it. Failures of the
transport itself belong to the transport's error, `iroh_moq::Error` for iroh.
