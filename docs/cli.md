# CLI reference (`irl`)

The `irl` binary lives in the `iroh-live-cli` crate. It has seven commands:
`devices`, `publish`, `watch`, `call`, `room`, `record`, and `run`. Watching in a
window needs the `render` feature, which is on by default; `--no-video` plays
audio without it, and `record` and `run` are headless whatever the build. `call`
and `room` are windows and nothing else, so a build without `render` does not
offer them at all.

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
| `--audio-output <ID>` | Play through this device, as the first column of `irl devices` lists it |
| `--fullscreen` | Start in fullscreen |

Without `--rendition` the video track adapts: the subscription's transport
signals drive rendition selection, and a switch opens the replacement decoder
alongside the incumbent so the picture does not go blank. The window's
rendition combo switches between the two modes at any time.

### `irl call`

Opens a 1:1 video call. Both peers publish their own camera and microphone and
subscribe to the other's, so there is no caller and no callee once the session
is up: the difference is only who dialed.

| Flag | Description |
|---|---|
| `<TICKET>` | Ticket of the peer to call. Omit to wait for somebody to call this node |
| `--no-qr` | Suppress the terminal QR code |
| `--fullscreen` | Start in fullscreen |

Every `--video`, `--audio`, `--codec`, `--renditions`, and geometry flag
`irl publish` takes applies here too, and describes what this node sends.

The window starts on a waiting screen showing this node's ticket, a box to paste
the peer's into, and the local camera. A ticket given on the command line is
dialed straight away, so the two halves of a call are `irl call` on one machine
and `irl call <TICKET>` on the other. Once connected, the peer fills the window
and the local camera moves to a corner. Moving the pointer brings up the ticket
bar, the stats overlay, and the rendition and volume controls; leaving it still
hides them again.

Hanging up on either side returns both windows to the waiting screen, ready for
the next call. The path a peer publishes on is `calls/<its endpoint id>`, which
`irl watch` can subscribe to like any other broadcast if all that is wanted is
one direction.

### `irl room`

Joins a multi-party room: publishes this node's camera into it and draws
everybody else's in a grid, with a chat panel underneath.

| Flag | Description |
|---|---|
| `<TICKET>` | Ticket of the room to join. Omit to open a new one |
| `--display-name <NAME>` | Name the other participants see (default: this node's short endpoint id) |
| `--no-qr` | Suppress the terminal QR code |
| `--fullscreen` | Start in fullscreen |

Every `irl publish` capture flag applies here too and describes what this node
sends.

Rooms are a gossip topic plus the MoQ subscriptions that follow from it. Every
participant announces the names of its broadcasts on the topic and subscribes to
every name it sees, so this is a full mesh and a small-group design: there is no
selective forwarding. The ticket a window prints includes itself as a bootstrap
peer, so it is the one to pass on to the next participant.

Chat rides on the same broadcast as the video, on a track named `chat`, so a
peer subscribed for the picture gets the messages without a second
subscription. Joining and leaving appear in the panel as they happen.

Leaving is derived from media, not from membership: a participant's tile
disappears when its broadcast closes or its session drops. A peer that joined
the topic and published nothing is not shown at all. See
[the rooms guide](guide/rooms.md) for why, and what replaces it.

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

### `irl run`

Runs a whole session from a TOML file: one endpoint publishing several
broadcasts, subscribing to several others, and recording any of them. That is
what a pile of `irl publish` and `irl watch` processes cannot do between them,
because each of those binds an endpoint of its own.

```sh
irl run session.toml
```

The session is headless. A `[[recv]]` block plays audio and can record to a
file, but no window opens: a window owns the main thread and there is only one
of those.

```toml
# Optional. Names a stored endpoint identity under
# <config dir>/iroh-live/secret_keys/<name>.key, generated on first use, so the
# tickets this session prints are the same on every run.
secret_key_name = "studio"

[[send]]
name = "camera"              # broadcast path, and the label in the output
video = "cam"                # every other key is an `irl publish` capture flag,
audio = "mic"                # under the flag's own name and with its default
codec = "h264"
encoder = "auto"
renditions = ["low:320x180", "720p"]
bitrate = 3_000_000
width = 1280
height = 720
fps = 30
no_cursor = false
audio_codec = "opus"
audio_bitrate = 96_000

[[send]]
name = "screen"
video = "screen"
audio = "none"

[[recv]]
name = "friend"
ticket = "iroh-live:..."     # as `irl publish` or another `irl run` printed it
audio_output = "default"     # or "none" to leave the speakers alone
record = "friend.mp4"        # optional; the container follows the extension
rendition = "low"            # optional; which video rendition to record
```

`name` is the only required key in a `[[send]]` block, and `name` and `ticket`
the only ones in a `[[recv]]`. An unrecognised key is an error rather than a
value silently dropped, so a typo is reported with the key that caused it.

A block that fails to start is reported and the rest of the session runs
without it. The session ends on Ctrl+C, which flushes every recording before
the endpoint closes.

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

Wait for a call, and call somebody from another machine:

```sh
irl call
irl call <TICKET>
```

Open a room, and join it from two more machines:

```sh
irl room --display-name alice
irl room --display-name bob <TICKET>
irl room --display-name carol <TICKET>
```

Record ten seconds of it to a fragmented MP4:

```sh
irl record <TICKET> -o clip.mp4 --duration 10
```

Publish two broadcasts and record a third, all from one endpoint:

```sh
irl run session.toml
```

## What is not here

`relay` was dropped in the move to the upstream media stack, and is recoverable
from the `main` branch.

The relay itself is not gone, only the subcommand that embedded it. Run it as its
own binary with `cargo run -p iroh-live-relay`, and see
[the browser relay guide](guide/browser-relay.md).
