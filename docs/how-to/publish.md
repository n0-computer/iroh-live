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
irl publish --video file:talk.mp4 --transcode
irl publish --video none --audio file:music.mp3:loop
```

A `file:` video source is republished without decoding. A plain MP4 or a
looped file needs `--transcode`, which runs it through ffmpeg first. The
[CLI reference](../reference/cli.md#sources) lists every source, and
[Raspberry Pi](raspberry-pi.md) covers the Pi camera.

## From Rust

This program publishes the camera, or the screen with `screen` as its first
argument. A second argument names a WAV or MP3 file to play instead of the
microphone. It needs the `capture` and `file` features.

```rust,no_run
use iroh::protocol::Router;
use iroh_live::{
    EndpointConfig, Node,
    layout::LiveLayout,
    media::{
        AudioSource, FileEnd, LocalBroadcast, VideoEncoding, VideoRendition, VideoSource, audio,
        audio_encoding, video,
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

    let mut capture = video::capture::Config::default();
    if screen {
        // The main display. On Wayland, the desktop portal asks the user.
        capture.source = video::capture::Source::Display(None);
    }
    // Fails if the device cannot be opened.
    let source = VideoSource::capture(capture).await?;
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

`VideoSource::capture` opens the device and fails if it cannot. On Linux with
the `pipewire` feature, the default camera is PipeWire's, which other
applications can share, and the default display is the screen the user picks
in the desktop portal. `capture.width`, `capture.height` and
`capture.framerate` are hints, and the device picks its nearest mode.

`AudioSource::microphone` only checks that the device exists. The microphone
opens once `set_audio` runs, and a failure then shows in
`LocalBroadcast::status()` as a failed audio slot.

### Changing the source

`set_video` and `set_audio` replace the current source of a running
broadcast, and viewers keep their subscription. To switch cameras behind one
source, `VideoSource::switchable` returns a `VideoSwitch` that swaps what
feeds it.

### Other sources

- `VideoSource::push(format)` and `AudioSource::push(format)` take frames
  that the application makes.
- A video file is republished without decoding, through moq-mux's importer.
  `irl` does this in `iroh-live-cli/src/import.rs`. There is no
  `VideoSource` for files.
- `rpicam_h264` publishes the Raspberry Pi camera's own H.264, see
  [Raspberry Pi](raspberry-pi.md).

## Where to go next

- [Watch a broadcast and render it](watch.md)
- [Record a broadcast](record.md)
- [Publishing](../architecture/publish.md) explains the encoder side.
