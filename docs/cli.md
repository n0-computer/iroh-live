# CLI reference (`irl`)

The `irl` binary lives in the `iroh-live-cli` crate. It has four commands:
`devices`, `publish`, `watch`, and `record`. Watching in a window needs the
`render` feature, which is on by default; `--no-video` plays audio without it,
and `record` is headless whatever the build.

## Commands

### `irl devices`

Lists the cameras, displays, and audio devices this machine offers. Every
identifier it prints is one the `--video` and `--audio` specifiers accept, so
the output doubles as the argument reference for `irl publish`.

Windows and applications appear on macOS only: they are ScreenCaptureKit
concepts. On Linux the displays section reports that listing is unavailable,
because the xdg-desktop-portal picker owns that choice. The audio outputs section
needs the `playback` feature.

### `irl publish`

Publishes a capture device or a media file. Prints a ticket, and a QR code of
it, that `irl watch` takes.

Source specifiers name a kind of source and optionally which device of that
kind. There is no backend segment: `moq_video::capture::Source` names a device
and lets the platform pick the backend that reaches it, so the old
`cam:v4l2:1` form described a choice the caller no longer makes.

| `--video` | Meaning |
|---|---|
| `cam` | The default camera. This is the default for `--video` |
| `cam:<id>` | A camera by the id `irl devices` reports |
| `screen`, `screen:<id>` | A whole display |
| `window:<id>` | One window (macOS) |
| `app:<id>` | Every window of one application (macOS) |
| `file:<path>` | A media file, republished rather than encoded |
| `test` | A generated colour-bar pattern |
| `none` | Publish no video |

| `--audio` | Meaning |
|---|---|
| `mic`, `mic:<id>` | A microphone. This is the default for `--audio` |
| `system` | Everything the machine is playing (macOS) |
| `file:<path>[:loop]` | An audio file, decoded and encoded |
| `test` | A generated tone |
| `none` | Publish no audio |

Anything `--audio` does not recognise is taken as a device name, so an
ALSA-style `hw:0,1` works as written.

Encoding flags:

| Flag | Description |
|---|---|
| `--test-source` | Force `--video test --audio test`, overriding both if they were given |
| `--codec <CODEC>` | `h264` (default) or `h265`, which needs hardware |
| `--encoder <KIND>` | `auto` (default), `hardware`, `software`, or a backend name such as `vaapi`, `nvenc`, `videotoolbox`, `openh264` |
| `--renditions <LIST>` | The simulcast ladder, comma-separated. Each rung is `<height>p`, `<width>x<height>`, or `<name>:<width>x<height>`; a bare name encodes at the source's own resolution. Default: one rendition named `video`, unscaled |
| `--bitrate <BPS>` | Target video bitrate. Omit to derive one from the resolution |
| `--width`, `--height`, `--fps` | Capture hints; the device snaps to its nearest supported mode |
| `--no-cursor` | Hide the pointer in screen, window, and application capture |
| `--audio-codec <CODEC>` | `opus` (default) or `pcm` |
| `--audio-bitrate <BPS>` | Opus only; PCM's bitrate follows from its layout |

Transport flags:

| Flag | Description |
|---|---|
| `--name <NAME>` | Broadcast path, as it appears in the ticket (default: `hello`) |
| `--relay <ENDPOINT_ID>` | Also connect to a relay, which then carries the broadcast on |
| `--no-serve` | Do not accept incoming subscribers, and print no ticket. Useful only with `--relay` |
| `--no-qr` | Suppress the terminal QR code |

File flags, for a `file:` video source:

| Flag | Description |
|---|---|
| `--format <FMT>` | `fmp4` (default) or `avc3` |
| `--transcode` | Re-mux (or re-encode) through ffmpeg first, which a plain MP4 needs |

Publishing to a relay is now just a connection. Every broadcast this node
publishes is announced on every MoQ session it has, so `--relay` connects and
the announce follows.

`--preview` opens a window showing what is being published. It draws the frames
already on their way to the encoders, so it costs no extra decode. It needs the
`render` feature, on by default, and it is not available for a file source, whose
tracks are republished as they are.

### `irl watch`

Subscribes to a broadcast and plays it. `irl play` is an alias.

| Flag | Description |
|---|---|
| `<TICKET>` | Connection ticket, as `irl publish` printed it |
| `--endpoint-id <ID>` | Remote endpoint id, instead of a ticket. Needs `--name` |
| `--name <NAME>` | Broadcast path, alongside `--endpoint-id` |
| `--no-video` | Play audio only; no window opens |
| `--rendition <NAME>` | Hold one rendition instead of following the downlink |
| `--fullscreen` | Start in fullscreen |

Without `--rendition` the video track adapts: the subscription's transport
signals drive rendition selection, and a switch opens the replacement decoder
alongside the incumbent so the picture does not go blank. The window's
rendition combo switches between the two modes at any time.

### `irl record`

Subscribes to a broadcast and writes it to a file. No window, no decoder: the
encoded frames come off the wire and go straight into the container, so a
recording costs about what a subscription costs.

| Flag | Description |
|---|---|
| `<TICKET>` | Connection ticket, as `irl publish` printed it |
| `--endpoint-id <ID>` | Remote endpoint id, instead of a ticket. Needs `--name` |
| `--name <NAME>` | Broadcast path, alongside `--endpoint-id` |
| `-o`, `--output <PATH>` | The file to write (default: `recording.mp4`) |
| `--format <FMT>` | `fmp4` or `mkv`, overriding what `--output`'s extension implies |
| `--rendition <NAME>` | Keep one video rendition instead of every rung the catalog offers |
| `--duration <SECS>` | Stop after this many seconds, instead of on Ctrl+C |
| `--latency <MILLIS>` | How long a stalled group is waited for before it is skipped (default: 2000) |

The container follows `--output`'s extension: `.mp4`, `.m4v`, and `.m4s` are
written as fragmented MP4, `.mkv` and `.webm` as Matroska. Anything else needs
`--format`.

Both containers are fragmented, so the file on disk is complete at every chunk
boundary: a recording stopped with Ctrl+C, or one whose process was killed, is
still playable up to the last fragment written.

Without `--rendition`, a simulcast broadcast is recorded whole and the file
carries one video track per rung. Players pick the largest.

## Examples

Publish the default camera and microphone, and print a ticket:

```sh
irl publish
```

Publish a generated pattern and tone, no hardware needed:

```sh
irl publish --test-source
```

Publish a camera as a two-rung simulcast ladder:

```sh
irl publish --video cam:/dev/video0 --renditions low:320x180,720p
```

Publish a fragmented MP4, re-muxing a plain one on the way:

```sh
irl publish --video file:recording.mp4 --transcode
```

Watch it:

```sh
irl watch <TICKET>
```

Record ten seconds of it to a fragmented MP4:

```sh
irl record <TICKET> -o clip.mp4 --duration 10
```

## What is not here

`call`, `room`, `run`, and `relay` were dropped in the move to the upstream
media stack, and all four are recoverable from the `main` branch. Rooms have left
`iroh-live` for the `iroh-rooms` crate and are being redesigned onto moq's
announce bus.

The relay itself is not gone, only the subcommand that embedded it. Run it as its
own binary with `cargo run -p iroh-live-relay`, and see
[the browser relay guide](guide/browser-relay.md).
