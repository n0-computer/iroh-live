# moq-ext-publisher

A broadcast that encodes sources into renditions, on top of
[moq-video](https://doc.moq.dev/lib/rs/crate/moq-video) and
[moq-audio](https://doc.moq.dev/lib/rs/crate/moq-audio). It does not depend on
iroh or on a player. A transport publishes it by reading it through
`moq_net::Consume`.

`LocalBroadcast` has one video slot and one audio slot. You open a source
(from `moq-ext-sources`) and hand it to the broadcast with an encoding:

```rust
use moq_ext_publisher::{AudioEncoding, LocalBroadcast, VideoEncoding, VideoRendition};
use moq_ext_sources::{AudioSource, VideoSource};

let broadcast = LocalBroadcast::new();
let camera = VideoSource::capture(moq_video::capture::Config::default()).await?;
broadcast.set_video(
    camera,
    VideoEncoding::ladder([VideoRendition::p360(), VideoRendition::p720()]),
)?;
broadcast.set_audio(AudioSource::tone(440.0, moq_audio::Layout::Mono), AudioEncoding::voice())?;
```

The source is opened once and every rendition gets its own encoder. A rendition
encodes only while someone watches it. `status()` says which renditions are
encoding and with which backend, and `stats()` reports each encoder.

## Feature flags

| Feature | Default | What it adds |
|---|---|---|
| `capture` | no | Camera, screen and microphone devices |
| `sound-server` | no | Audio devices through PipeWire or PulseAudio instead of ALSA |
| `aec` | no | Echo cancellation. Implies `capture` |
| `pipewire` | no | Linux screen capture through xdg-desktop-portal |
| `vaapi`, `nvidia`, `v4l2` | no | Hardware H.264 encoding |
| `rpicam` | no | The `rpicam-vid` sources. Linux only |
