# Publish a camera, screen or file

This page publishes a capture device or a file, first with `irl`, then from
Rust. The [library quickstart](../getting-started/library-quickstart.md)
covers the generated test sources.

## With `irl`

`irl devices` lists the cameras, displays and audio devices, each with the id
that `--video` and `--audio` take.

```sh
irl publish                                   # default camera and microphone
irl publish --video cam:/dev/video2 --audio none
irl publish --video screen --audio system     # system audio is macOS only
irl publish --video file:talk.mp4:loop        # the file as it is, with its audio
irl publish --video file:talk.mp4 --reencode --renditions 720p,360p
irl publish --video none --audio file:music.mp3:loop
```

The [CLI reference](../reference/cli.md#sources) lists every source, and
[Raspberry Pi](raspberry-pi.md) covers the Pi camera. The next section
explains the two ways to publish a file.

## Publish a file

`file:` takes an MP4 file, plain or fragmented, and plays it in real time,
from the start again with `:loop`. Its audio comes along unless `--audio`
names another source. By default the file's H.264 and AAC are published as
they are. `--reencode` decodes them and encodes them like a camera, which
`irl call` and `irl room` accept as well.

Passthrough costs no CPU and keeps the file's quality, but it publishes one
rendition at the file's size and bitrate. Keyframes come only where the file
has them, so a new viewer waits up to one GOP for a picture, and a viewer's
request for a keyframe goes unanswered. Nothing adapts the bitrate to the
path, and there are no pictures for a preview. `--reencode` lifts all of
that, at the cost of a decode and an encode, and is needed for
`--renditions`, a size, and `--preview`.

Both ways take H.264 video and AAC audio only. Convert a file in another
codec first, for example with
`ffmpeg -i in.mp4 -c:v libx264 -c:a aac out.mp4`. A file whose audio is in
another codec publishes its video alone, with a warning.

From Rust, `MediaFile` does the same. `MediaFile::open` reads the file's
index, `info()` reports its codecs, size, rate and GOP, and the file hands
out either encoded streams for `set_encoded_video` and `set_encoded_audio`
or decoded sources for `set_video` and `set_audio`. Every stream of one file
plays against one clock, so audio and video stay in sync and loop together.
The `publish-file` example shows both ways.

## From Rust

This program publishes the camera, or the screen with `screen` as its first
argument. A second argument names a WAV, MP3 or MP4 file whose audio plays
instead of the microphone. It needs the `capture` and `file` features.

```rust,no_run
use iroh::protocol::Router;
use iroh_live::{
    EndpointConfig, Node,
    layout::LiveLayout,
    media::{
        AudioSource, FileEnd, LocalBroadcast, VideoEncoding, VideoRendition, VideoSource, audio,
        audio_encoding,
    },
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let screen = args.next().as_deref() == Some("screen");
    let audio_file = args.next();

    let endpoint = EndpointConfig::from_env()?.bind().await?;
    let layout = LiveLayout::default();
    let node = Node::new(endpoint.clone(), layout.config());
    let router = node.mount(Router::builder(endpoint)).spawn();
    let broadcast: LocalBroadcast = layout.publish(&node, "hello")?;

    // Fails if the device cannot be opened. On Wayland the screen needs the
    // `pipewire` feature, and the user picks it in the desktop portal.
    let source = if screen {
        VideoSource::screen().await?
    } else {
        VideoSource::camera(None).await?
    };
    broadcast.set_video(source, VideoEncoding::single(VideoRendition::new("video")))?;

    match audio_file {
        Some(path) => {
            let file = AudioSource::file(path, FileEnd::Restart).await?;
            broadcast.set_audio(file, audio_encoding::music())?;
        }
        None => {
            let microphone = AudioSource::microphone(audio::capture::Config::default()).await?;
            broadcast.set_audio(microphone, audio_encoding::voice())?;
        }
    }
    println!("{}", layout.ticket(node.id(), "hello")?);

    tokio::signal::ctrl_c().await?;
    router.shutdown().await?;
    Ok(())
}
```

`VideoRendition::new("video")` encodes at the source's size. For a ladder, a
bitrate or a keyframe interval, see [Tune quality and latency](tune.md).

### Opening a device

`VideoSource::camera` and `VideoSource::screen` open a camera or the main
screen at its default mode. `VideoSource::capture` takes a full
`capture::Config` instead, and `screen_source()` returns the capture source
`screen` opens, for a config of your own. Each opens the device and fails if
it cannot. On Linux with the `pipewire` feature, the default camera is
PipeWire's, which other applications can share, and the default display is
the screen the user picks in the desktop portal. `capture.width`, `capture.height` and
`capture.framerate` are hints, and the device picks its nearest mode.

`AudioSource::microphone` only checks that the device exists. The microphone
opens once `set_audio` runs, and a failure then shows in
`LocalBroadcast::status()` as a failed audio slot.

### Changing the source

`set_video` and `set_audio` replace the current source of a running
broadcast, and viewers keep their subscription. The broadcast keeps the
sources it encodes: `video_source()` and `audio_source()` hand them back, for
example to draw a preview from `video_source()`'s frames, so the application
does not have to track them itself. To switch cameras behind one
source, `VideoSource::switchable` returns a `VideoSwitch` that swaps what
feeds it.

### Other sources

- `VideoSource::push(format)` and `AudioSource::push(format)` take frames
  that the application makes. A video producer can call
  `FrameSender::request_keyframe()` before a push to make that frame a
  keyframe on every rendition.
- `MediaFile` publishes an MP4 file as it is or decodes it, see
  [Publish a file](#publish-a-file).
- `rpicam_h264` publishes the Raspberry Pi camera's own H.264, see
  [Raspberry Pi](raspberry-pi.md).

## Where to go next

- [Watch a broadcast and render it](watch.md)
- [Record a broadcast](record.md)
- [Publishing](../architecture/publish.md) explains the encoder side.
