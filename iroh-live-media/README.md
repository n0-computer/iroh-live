# iroh-live-media

Sources, broadcasts and players on top of
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio). The codecs and devices
are upstream and re-exported as `iroh_live_media::video` and
`iroh_live_media::audio`. This crate adds simulcast publishing, rendition
selection and audio/video sync.

It does not depend on iroh. A broadcast is published through
`moq_net::Consume` and read from a `moq_net::broadcast::Consumer`, whatever
carried it.

## Publishing

`LocalBroadcast` has one video slot and one audio slot. You open a source and
hand it to the broadcast with an encoding:

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

The source is opened once and every rendition gets its own encoder. A
rendition encodes only while someone watches it. `status()` says which
renditions are encoding and with which backend, and `stats()` reports each
encoder.

A `VideoSource` is a capture device, the test pattern, frames the app pushes
(`push`) or makes on its own thread (`spawn`), or the Raspberry Pi camera.
`EncodedVideoSource` publishes Annex-B H.264 that is already encoded, which is
the cheapest path on a Pi. An `AudioSource` is a microphone, a WAV or MP3 file,
a tone, or pushed PCM.

A microphone opens only when a broadcast first has a listener, because
moq-audio opens it inside the publication that encodes it.
`AudioSource::microphone` just checks that the device exists. A later failure
shows in `LocalBroadcast::status()`.

## Playing

`RemoteBroadcast` reads a broadcast's catalog and holds the subscription.
`play(PlayerConfig)` starts a `Player`. The player picks a rendition from the
network signals a transport attached with `RemoteBroadcast::with_network`. It
switches by opening the new decoder next to the old one and handing over once
the new one has caught up, so the picture never goes blank. Each player also
owns a playout clock that holds video back by the audio queued at the speaker.

A transport builds the `RemoteBroadcast`. `from_origin` follows a path in a
route table and looks it up again when a route change ends the broadcast.
`from_resolved` does the same, starting from a consumer the caller already
has. `RemoteBroadcast::local` reads a `LocalBroadcast` in-process.

`record` writes a broadcast to fragmented MP4 or Matroska without decoding.
`Recording::stop` takes `&mut self`, so it can share a `select!` with `wait`:

```rust
use iroh_live_media::RecordConfig;

let file = tokio::fs::File::create("out.mp4").await?;
let mut recording = remote.record(file, RecordConfig::default())?;
let written = tokio::select! {
    written = recording.wait() => written?,
    _ = tokio::signal::ctrl_c() => recording.stop().await?,
};
```

Audio plays through an `AudioOutput` that the app opens and passes to each
`PlayerConfig`. Pass the same output to `MicrophoneConfig` to cancel its echo.

## Feature flags

| Feature | Default | What it adds |
|---|---|---|
| `capture` | yes | Camera, screen and microphone devices. Needs the V4L2 and ALSA headers on Linux |
| `sound-server` | yes | Audio devices through PipeWire or PulseAudio instead of ALSA |
| `playback` | no | Speaker output |
| `aec` | no | Echo cancellation. Implies `capture` and `playback` |
| `pipewire` | no | Linux screen capture through xdg-desktop-portal. Links `libpipewire-0.3` |
| `render` | no | The wgpu renderer |
| `vaapi` | no | Intel and AMD hardware H.264 through VA-API |
| `nvidia` | no | NVIDIA hardware encoding and decoding |
| `v4l2` | no | The V4L2 memory-to-memory H.264 codecs of ARM SoCs such as the Raspberry Pi |
| `rpicam` | no | The `rpicam-vid` sources. Linux only, needs the program on `PATH` |

Every codec is always compiled upstream, so there are no codec flags. The test
pattern and tones need no flag either.

## Errors

`iroh_live_media::Error` covers devices, codecs, the catalog and files, plus
`Broadcast` for a track or broadcast that moq-net refused or reset. Failures of
the transport itself belong to the transport's error, `iroh_moq::Error` for
iroh.
