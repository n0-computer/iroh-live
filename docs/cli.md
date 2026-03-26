# CLI reference (`irl`)

The `irl` binary lives in the `iroh-live-cli` crate. It provides five
commands: `devices`, `publish`, `play`, `call`, and `room`. The `play`,
`call`, and `room` commands require the `wgpu` feature (enabled by
default).

## Commands

### `irl devices`

Lists available cameras, screens, audio devices, and codecs on the
current system.

### `irl publish [capture|file]`

Publishes media over iroh. Two input modes:

- **`capture`** (default when subcommand is omitted) — captures from
  camera, screen, or microphone and encodes live.
- **`file`** — reads from a media file (or stdin) in fMP4 or AVC3
  format, optionally re-encoding with ffmpeg via `--transcode`.

Transport flags (shared by both modes):

| Flag | Description |
|------|-------------|
| `--name <NAME>` | Broadcast name (default: `hello`) |
| `--relay <ID>` | Push to a relay endpoint |
| `--room <TICKET>` | Publish into a room |
| `--no-serve` | Don't accept incoming subscribers (push-only) |
| `--no-qr` | Suppress terminal QR code |
| `--preview` | Open an egui preview window (capture only) |

Capture flags:

| Flag | Description |
|------|-------------|
| `--video <SPEC>` | Video source: `cam`, `cam:<id>`, `screen`, `screen:<backend>:<id>`, `test`, `none`. Repeatable for multiple sources. Default: first camera. |
| `--audio <SPEC>` | Audio source: device name, `test`, `none`. Default: system mic. |
| `--test-source` | Synthetic SMPTE test pattern and tone (shorthand for `--video test --audio test`) |
| `--codec <CODEC>` | Video codec: `h264`, `av1`, `h264-vaapi`, etc. |
| `--video-presets <LIST>` | Comma-separated simulcast presets: `180p,360p,720p,1080p`. Default: all. |
| `--audio-preset <PRESET>` | Audio quality preset (default: `hq`) |

File flags:

| Flag | Description |
|------|-------------|
| `<FILE>` | Input file path (reads stdin if omitted) |
| `--format <FMT>` | Input format: `fmp4` (default), `avc3` |
| `--transcode` | Re-encode with ffmpeg |

### `irl play`

Subscribes to and plays a remote broadcast in an egui/wgpu window.

| Flag | Description |
|------|-------------|
| `<TICKET>` | Connection ticket (conflicts with `--endpoint-id`) |
| `--endpoint-id <ID>` | Remote endpoint ID (requires `--name`) |
| `--name <NAME>` | Broadcast name (with `--endpoint-id`) |
| `--no-video` | Audio-only, no window |
| `--decoder <BACKEND>` | Decoder backend: `auto` (default), `sw` |
| `--audio-device <ID>` | Audio output device |
| `--fullscreen` | Start in fullscreen |

### `irl call`

Bidirectional 1:1 video call. Pass a ticket to dial, or omit it to
wait for an incoming connection. Accepts all capture flags plus:

| Flag | Description |
|------|-------------|
| `<TICKET>` | Remote ticket to auto-dial |
| `--decoder <BACKEND>` | Decoder backend: `auto`, `sw` |
| `--audio-device <ID>` | Audio output device |

### `irl room`

Multi-party room with a video grid. Pass a room ticket to join, or
omit it to create a new room. Accepts all capture flags plus:

| Flag | Description |
|------|-------------|
| `<TICKET>` | Room ticket to join |
| `--decoder <BACKEND>` | Decoder backend: `auto`, `sw` |
| `--audio-device <ID>` | Audio output device |

## Examples

Publish from camera with default settings, print a ticket:

```sh
irl publish
```

Publish a test pattern at 720p using H.264:

```sh
irl publish capture --test-source --codec h264 --video-presets 720p
```

Publish an fMP4 file:

```sh
irl publish file recording.mp4
```

Play a remote broadcast:

```sh
irl play <TICKET>
```

Start a 1:1 call, dialing a remote peer:

```sh
irl call <TICKET> --test-source
```

Create a room and publish into it:

```sh
irl room
```

## Remaining work

See [plans/cli.md](../plans/cli.md) for the open items: `--preview`
window, room grid layout options, auto-hide UI bars, and shared
control widgets.
